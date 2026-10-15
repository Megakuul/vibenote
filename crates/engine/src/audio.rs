//! Audio in the engine (ADR 0009, R-AUDIO-001..003): the recording in progress, playback, and
//! the links from strokes to recordings. Live recordings encode and store their chunks on a
//! worker thread; the engine only collects the stored chunk hashes (`pump`), so recording never
//! blocks inking.

use crate::{EngineError, Session};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use vibenote_audio::opusogg::StreamEncoder;
use vibenote_audio::{AudioError, Pcm, RATE};
use vibenote_geom::Point;
use vibenote_model::{AudioMark, NoteId, ObjectId};

/// Default lead-in before a stroke's moment (SPEC R-AUDIO-002 [proposal]).
pub const LEAD_IN_MS: u64 = 2_000;
/// Longest wait for a live recording to finish its last chunk at stop.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn aerr(e: AudioError) -> EngineError {
    EngineError::Model(format!("audio: {e}"))
}

/// Where a recording's samples come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The generated test tone (never the microphone): tests and demos.
    TestTone,
    /// The default input device through PipeWire (an explicit user action).
    Device,
}

enum Msg {
    Chunk(String),
    Done { duration_ms: u64 },
    Failed(String),
}

/// Stops a live recording's worker when the last handle to it goes.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

type Shared<T> = Arc<Mutex<T>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
enum Kind {
    /// Deterministic: the test tone is encoded up to a given length on demand.
    Simulated {
        enc: Shared<Option<StreamEncoder>>,
        samples: u64,
    },
    /// A worker thread captures (or generates in real time), encodes and stores chunks.
    Live {
        rx: Shared<Receiver<Msg>>,
        stop: Arc<AtomicBool>,
        thread: Shared<Option<std::thread::JoinHandle<()>>>,
        _guard: Arc<StopOnDrop>,
    },
}

/// The recording in progress.
#[derive(Clone)]
pub struct Recording {
    pub id: ObjectId,
    pub note: NoteId,
    pub origin: Point,
    /// CLOCK_MONOTONIC µs of its first sample (the pen events' clock).
    pub start_us: u64,
    pub chunks: Vec<String>,
    kind: Kind,
}

/// A playback in progress.
struct Playing {
    object: ObjectId,
    rate: u32,
    position: Arc<AtomicUsize>,
    #[cfg(feature = "device")]
    _stream: vibenote_audio::device::Running,
}

/// The audio state of a session (cheap to clone; clones share it, like the PDF service).
#[derive(Clone, Default)]
pub struct AudioService {
    pub(crate) rec: Option<Recording>,
    playing: Shared<Option<Playing>>,
    /// The last decoded object (playback seeks without decoding again).
    decoded: Shared<Option<(ObjectId, Arc<Pcm>)>>,
}

impl AudioService {
    /// True while a recording runs.
    pub fn is_recording(&self) -> bool {
        self.rec.is_some()
    }
}

impl PartialEq for AudioService {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for AudioService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioService")
            .field("recording", &self.rec.as_ref().map(|r| r.id))
            .finish()
    }
}

/// CLOCK_MONOTONIC in µs (the clock of pen events in the app).
pub fn now_us() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(t.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000)
        .saturating_add(u64::try_from(t.tv_nsec).unwrap_or(0) / 1_000)
}

fn samples_for_us(us: u64) -> u64 {
    us.saturating_mul(u64::from(RATE)) / 1_000_000
}

fn live_worker(
    source: Source,
    blobs: Arc<vibenote_storage::BlobStore>,
    serial: u32,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
) {
    let fail = |tx: &Sender<Msg>, e: String| {
        let _ = tx.send(Msg::Failed(e));
    };
    let mut enc = match StreamEncoder::new(serial) {
        Ok(e) => e,
        Err(e) => return fail(&tx, e.to_string()),
    };
    let store = |chunks: Vec<Vec<u8>>, tx: &Sender<Msg>| -> Result<(), String> {
        for c in chunks {
            let h = blobs.put(&c).map_err(|e| e.to_string())?;
            let _ = tx.send(Msg::Chunk(h));
        }
        Ok(())
    };
    match source {
        Source::TestTone => {
            let t0 = std::time::Instant::now();
            let mut pos = 0u64;
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let due = samples_for_us(u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX));
                let n = usize::try_from(due.saturating_sub(pos)).unwrap_or(0);
                let chunks = match enc.push(&vibenote_audio::tone::samples(pos, n)) {
                    Ok(c) => c,
                    Err(e) => return fail(&tx, e.to_string()),
                };
                pos = due;
                if let Err(e) = store(chunks, &tx) {
                    return fail(&tx, e);
                }
            }
        }
        Source::Device => {
            #[cfg(feature = "device")]
            {
                let (dtx, drx) = std::sync::mpsc::sync_channel::<Vec<f32>>(512);
                let capture = vibenote_audio::device::capture(move |buf| {
                    // Never blocks PipeWire's thread: a full queue drops this buffer.
                    let _ = dtx.try_send(buf.to_vec());
                });
                let mut capture = match capture {
                    Ok(c) => c,
                    Err(e) => return fail(&tx, e.to_string()),
                };
                while !stop.load(Ordering::Acquire) {
                    match drx.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(buf) => match enc.push(&buf).map_err(|e| e.to_string()).and_then(|c| store(c, &tx)) {
                            Ok(()) => {}
                            Err(e) => return fail(&tx, e),
                        },
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                capture.stop();
                while let Ok(buf) = drx.try_recv() {
                    if let Err(e) = enc.push(&buf).map_err(|e| e.to_string()).and_then(|c| store(c, &tx)) {
                        return fail(&tx, e);
                    }
                }
            }
            #[cfg(not(feature = "device"))]
            return fail(&tx, "built without audio devices".into());
        }
    }
    match enc.finish() {
        Ok((last, ms)) => match store(vec![last], &tx) {
            Ok(()) => {
                let _ = tx.send(Msg::Done { duration_ms: ms });
            }
            Err(e) => fail(&tx, e),
        },
        Err(e) => fail(&tx, e.to_string()),
    }
}

/// Parameters of `audio.record.start`.
pub struct StartParams {
    pub source: Source,
    pub at: Point,
    /// Start time (pen-event clock); `None` = now. Given: the recording is simulated.
    pub at_us: Option<u64>,
    /// Encode this much of the test tone right away (simulated recording).
    pub simulate_seconds: Option<f64>,
}

impl Session {
    /// The audio object as it would be stored now (the crash-recovery journal).
    fn recording_journal(&mut self) {
        let Some(r) = &self.audio.rec else { return };
        if !self.persistent() || r.chunks.is_empty() {
            return;
        }
        let kind = vibenote_model::ObjectKind::Audio {
            origin: r.origin,
            blobs: r.chunks.clone(),
            duration_ms: 0,
            recorded: true,
        };
        if let Ok(data) = serde_json::to_string(&kind) {
            self.derived_pending.push(vibenote_storage::Derived::Recording {
                id: r.id,
                note: r.note,
                data,
            });
        }
    }

    pub(crate) fn record_start(&mut self, note: NoteId, p: &StartParams) -> Result<ObjectId, EngineError> {
        if self.audio.rec.is_some() {
            return Err(EngineError::Model("a recording is already running".into()));
        }
        let id = ObjectId(self.ws.alloc_id()?);
        let serial = u32::try_from(id.0 & 0xffff_ffff).unwrap_or(1);
        let simulated = p.at_us.is_some() || p.simulate_seconds.is_some();
        let kind = if simulated {
            if p.source != Source::TestTone {
                return Err(EngineError::Model("only the test tone can be simulated".into()));
            }
            Kind::Simulated {
                enc: Arc::new(Mutex::new(Some(StreamEncoder::new(serial).map_err(aerr)?))),
                samples: 0,
            }
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let (s2, blobs, source) = (Arc::clone(&stop), Arc::clone(&self.blobs.0), p.source);
            let thread = std::thread::Builder::new()
                .name("vibenote-recorder".into())
                .spawn(move || live_worker(source, blobs, serial, tx, s2))
                .map_err(|e| EngineError::Internal(e.to_string()))?;
            Kind::Live {
                rx: Arc::new(Mutex::new(rx)),
                _guard: Arc::new(StopOnDrop(Arc::clone(&stop))),
                stop,
                thread: Arc::new(Mutex::new(Some(thread))),
            }
        };
        self.audio.rec = Some(Recording {
            id,
            note,
            origin: p.at,
            start_us: p.at_us.unwrap_or_else(now_us),
            chunks: Vec::new(),
            kind,
        });
        if let Some(sec) = p.simulate_seconds {
            if !(sec.is_finite() && (0.0..=86_400.0).contains(&sec)) {
                self.audio.rec = None;
                return Err(EngineError::Model("simulate_seconds out of range".into()));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
            let n = (sec * f64::from(RATE)).round() as u64;
            self.simulate_to(n)?;
        }
        Ok(id)
    }

    /// Simulated recording: encodes the test tone up to `n` samples; stores finished chunks.
    fn simulate_to(&mut self, n: u64) -> Result<(), EngineError> {
        let blobs = Arc::clone(&self.blobs.0);
        let Some(Recording {
            kind: Kind::Simulated { enc, samples },
            chunks,
            ..
        }) = &mut self.audio.rec
        else {
            return Ok(());
        };
        let mut g = lock(enc);
        let Some(enc) = g.as_mut() else { return Ok(()) };
        while *samples < n {
            let k = n.saturating_sub(*samples).min(u64::from(RATE));
            let part = vibenote_audio::tone::samples(*samples, usize::try_from(k).unwrap_or(0));
            for c in enc.push(&part).map_err(aerr)? {
                chunks.push(blobs.put(&c).map_err(|e| EngineError::Storage(e.to_string()))?);
            }
            *samples = samples.saturating_add(k);
        }
        drop(g);
        self.recording_journal();
        Ok(())
    }

    /// Collects chunks a live recording stored meanwhile (never waits).
    pub(crate) fn audio_pump(&mut self) -> usize {
        let mut got = 0usize;
        let mut failed = None;
        if let Some(Recording {
            kind: Kind::Live { rx, .. },
            chunks,
            ..
        }) = &mut self.audio.rec
        {
            let rx = lock(rx);
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Chunk(h) => {
                        chunks.push(h);
                        got = got.saturating_add(1);
                    }
                    Msg::Done { .. } => {}
                    Msg::Failed(e) => failed = Some(e),
                }
            }
        }
        if let Some(e) = failed {
            self.warn(format!("recording stopped: {e}"));
        }
        if got > 0 {
            self.recording_journal();
        }
        got
    }

    /// Stops the recording; stores it as an audio object (one undo step). `at_us` ends a
    /// simulated recording at that time.
    pub(crate) fn record_stop(&mut self, id: Option<u64>, at_us: Option<u64>) -> Result<(ObjectId, u64), EngineError> {
        let rec = self
            .audio
            .rec
            .as_ref()
            .ok_or_else(|| EngineError::Model("no recording is running".into()))?;
        if id.is_some_and(|i| i != rec.id.0) {
            return Err(EngineError::Model(format!(
                "recording {} is not running",
                id.unwrap_or(0)
            )));
        }
        if let (Some(at), Kind::Simulated { .. }) = (at_us, &rec.kind) {
            let n = samples_for_us(at.saturating_sub(rec.start_us));
            self.simulate_to(n)?;
        }
        let Some(mut rec) = self.audio.rec.take() else {
            return Err(EngineError::Model("no recording is running".into()));
        };
        let duration_ms = match &mut rec.kind {
            Kind::Simulated { enc, .. } => {
                let enc = lock(enc)
                    .take()
                    .ok_or_else(|| EngineError::Internal("encoder gone".into()))?;
                let (last, ms) = enc.finish().map_err(aerr)?;
                rec.chunks.push(
                    self.blobs
                        .0
                        .put(&last)
                        .map_err(|e| EngineError::Storage(e.to_string()))?,
                );
                ms
            }
            Kind::Live { rx, stop, thread, .. } => {
                stop.store(true, Ordering::Release);
                let rx = lock(rx);
                let deadline = std::time::Instant::now()
                    .checked_add(STOP_WAIT)
                    .unwrap_or_else(std::time::Instant::now);
                let mut ms = None;
                while ms.is_none() {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    match rx.recv_timeout(left) {
                        Ok(Msg::Chunk(h)) => rec.chunks.push(h),
                        Ok(Msg::Done { duration_ms }) => ms = Some(duration_ms),
                        Ok(Msg::Failed(e)) => {
                            self.warn(format!("recording: {e}"));
                            break;
                        }
                        Err(_) => break,
                    }
                }
                if let Some(t) = lock(thread).take() {
                    let _ = t.join();
                }
                drop(rx);
                // Without a clean end, the chunks so far are kept (their length is decoded).
                match ms {
                    Some(ms) => ms,
                    None => self.decoded_ms(&rec.chunks).unwrap_or(0),
                }
            }
        };
        if rec.chunks.is_empty() {
            return Err(EngineError::Model("the recording has no audio".into()));
        }
        let object = vibenote_model::CanvasObject {
            id: rec.id,
            z: self.next_z(rec.note)?,
            kind: vibenote_model::ObjectKind::Audio {
                origin: rec.origin,
                blobs: rec.chunks.clone(),
                duration_ms,
                recorded: true,
            },
        };
        if self.persistent() {
            self.derived_pending
                .push(vibenote_storage::Derived::RecordingDone { id: rec.id });
        }
        self.commit(
            crate::Scope::Note(rec.note),
            "record audio",
            vec![vibenote_model::Op::InsertObject { note: rec.note, object }],
        )?;
        Ok((rec.id, duration_ms))
    }

    fn decoded_ms(&self, chunks: &[String]) -> Option<u64> {
        let mut bytes = Vec::new();
        for c in chunks {
            bytes.extend_from_slice(&self.blobs.0.get(c).ok()?);
        }
        vibenote_audio::opusogg::decode(&bytes).ok().map(|p| p.duration_ms())
    }

    /// The mark for a stroke written at `written_at_us` (pen-event clock) during the
    /// recording, if one runs and the stroke is not older than it.
    pub(crate) fn audio_mark_for(&self, written_at_us: Option<u64>) -> Option<AudioMark> {
        let r = self.audio.rec.as_ref()?;
        let at = written_at_us.unwrap_or_else(now_us);
        let off = at.checked_sub(r.start_us)?;
        Some(AudioMark {
            recording: r.id,
            offset_ms: off / 1_000,
        })
    }

    /// Finds an audio object by id (any note).
    pub(crate) fn audio_object(&self, id: ObjectId) -> Option<(NoteId, vibenote_model::CanvasObject)> {
        self.ws
            .canvases
            .iter()
            .find_map(|(n, c)| c.objects.get(&id).map(|o| (*n, o.clone())))
            .filter(|(_, o)| matches!(o.kind, vibenote_model::ObjectKind::Audio { .. }))
    }

    /// Decodes an audio object (cached for the last one).
    pub fn audio_pcm(&self, id: ObjectId) -> Result<Arc<Pcm>, EngineError> {
        if let Some((k, p)) = lock(&self.audio.decoded).as_ref()
            && *k == id
        {
            return Ok(Arc::clone(p));
        }
        let (_, o) = self
            .audio_object(id)
            .ok_or_else(|| EngineError::Model(format!("no audio object {}", id.0)))?;
        let vibenote_model::ObjectKind::Audio { blobs, recorded, .. } = &o.kind else {
            return Err(EngineError::Model(format!("object {} is not audio", id.0)));
        };
        let mut bytes = Vec::new();
        for b in blobs {
            let part = self.blobs.0.get(b).map_err(|e| EngineError::Storage(e.to_string()))?;
            if bytes.len().saturating_add(part.len())
                > usize::try_from(vibenote_audio::import::MAX_FILE).unwrap_or(usize::MAX)
            {
                return Err(aerr(AudioError::TooLong));
            }
            bytes.extend_from_slice(&part);
        }
        let pcm = Arc::new(if *recorded {
            vibenote_audio::opusogg::decode(&bytes).map_err(aerr)?
        } else {
            vibenote_audio::import::decode(&bytes).map_err(aerr)?
        });
        *lock(&self.audio.decoded) = Some((id, Arc::clone(&pcm)));
        Ok(pcm)
    }

    /// Plays an audio object from `position_ms` (replacing any playback).
    pub(crate) fn audio_play(&self, id: ObjectId, position_ms: u64) -> Result<(), EngineError> {
        let pcm = self.audio_pcm(id)?;
        let frame = usize::try_from(position_ms.saturating_mul(u64::from(pcm.rate)) / 1000).unwrap_or(usize::MAX);
        let mut g = lock(&self.audio.playing);
        *g = None; // stops the previous playback
        #[cfg(feature = "device")]
        {
            let position = Arc::new(AtomicUsize::new(frame));
            let stream = vibenote_audio::device::play(Arc::clone(&pcm), frame, Arc::clone(&position)).map_err(aerr)?;
            *g = Some(Playing {
                object: id,
                rate: pcm.rate,
                position,
                _stream: stream,
            });
            Ok(())
        }
        #[cfg(not(feature = "device"))]
        {
            let _ = (frame, &mut g);
            Err(aerr(AudioError::Device("built without audio devices".into())))
        }
    }

    pub(crate) fn audio_stop_playback(&self) -> bool {
        lock(&self.audio.playing).take().is_some()
    }

    /// State for `audio.status` and `state.inspect`.
    pub(crate) fn audio_status(&self) -> serde_json::Value {
        let rec = self.audio.rec.as_ref().map(|r| {
            serde_json::json!({
                "id": r.id.0,
                "note": r.note.0,
                "start_us": r.start_us,
                "chunks": r.chunks.len(),
                "live": matches!(r.kind, Kind::Live { .. }),
            })
        });
        let play = lock(&self.audio.playing).as_ref().map(|p| {
            let frame = u64::try_from(p.position.load(Ordering::Acquire)).unwrap_or(0);
            serde_json::json!({
                "object": p.object.0,
                "position_ms": frame.saturating_mul(1000).checked_div(u64::from(p.rate)).unwrap_or(0),
            })
        });
        serde_json::json!({ "recording": rec, "playing": play })
    }

    /// Recordings a crash left in the journal become audio objects (with a warning).
    pub(crate) fn recover_recordings(&mut self) -> Result<usize, EngineError> {
        let Some(Ok(rows)) = self.reader.with(vibenote_storage::Store::recordings) else {
            return Ok(0);
        };
        let mut n = 0usize;
        for (id, note, data) in rows {
            self.derived_pending
                .push(vibenote_storage::Derived::RecordingDone { id });
            let Ok(vibenote_model::ObjectKind::Audio { origin, blobs, .. }) = serde_json::from_str(&data) else {
                continue;
            };
            if !self.ws.notes.contains_key(&note) || blobs.is_empty() || self.audio_object(id).is_some() {
                continue;
            }
            // The id was handed out before the crash; it may not have been saved as used.
            self.ws.next_id = self.ws.next_id.max(id.0.saturating_add(1));
            let duration_ms = self.decoded_ms(&blobs).unwrap_or(0);
            let object = vibenote_model::CanvasObject {
                id,
                z: self.next_z(note)?,
                kind: vibenote_model::ObjectKind::Audio {
                    origin,
                    blobs,
                    duration_ms,
                    recorded: true,
                },
            };
            self.commit(
                crate::Scope::Note(note),
                "recovered recording",
                vec![vibenote_model::Op::InsertObject { note, object }],
            )?;
            self.warn(format!(
                "a recording interrupted by a crash was recovered ({} s)",
                duration_ms / 1000
            ));
            n = n.saturating_add(1);
        }
        Ok(n)
    }
}
