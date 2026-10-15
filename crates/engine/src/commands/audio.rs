//! Audio commands (R-AUDIO-001..003, ADR 0009): record into a note, import a file, play, and
//! play from the moment a stroke was written.

use crate::audio::{LEAD_IN_MS, Source, StartParams, aerr};
use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, NoteId, ObjectId, ObjectKind, Op};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum SourceP {
    /// The generated test tone (never the microphone).
    TestTone,
    /// The default input device (PipeWire).
    Device,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StartP {
    /// Target note; defaults to the current note.
    note: Option<u64>,
    #[serde(default = "default_source")]
    source: SourceP,
    /// Where the player goes on the canvas (default: the view's centre).
    at: Option<[f64; 2]>,
    /// Start time on the pen-event clock (CLOCK_MONOTONIC µs). Given (or `simulate_seconds`):
    /// a simulated test-tone recording that advances only by these times (tests).
    at_us: Option<u64>,
    /// Encode this many seconds of the test tone right away (simulated recording).
    simulate_seconds: Option<f64>,
}
fn default_source() -> SourceP {
    SourceP::Device
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StopP {
    /// The recording (`audio.record.start`'s id); default: the running one.
    id: Option<u64>,
    /// End time of a simulated recording (pen-event clock, µs).
    at_us: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportP {
    path: PathBuf,
    note: Option<u64>,
    at: Option<[f64; 2]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlayStrokeP {
    stroke: u64,
    /// The stroke's note; default: the current note.
    note: Option<u64>,
    /// Played before the stroke's moment (default 2,000 ms).
    lead_in_ms: Option<u64>,
    /// Report the position without playing.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlayP {
    id: u64,
    #[serde(default)]
    position_ms: u64,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoP {}

#[derive(Serialize)]
struct Started {
    id: u64,
    note: u64,
    start_us: u64,
}

#[derive(Serialize)]
struct Stopped {
    id: u64,
    duration_ms: u64,
}

#[derive(Serialize)]
struct Imported {
    id: u64,
    blob: String,
    duration_ms: u64,
    rate: u32,
    channels: u16,
}

#[derive(Serialize)]
struct Played {
    recording: u64,
    position_ms: u64,
    playing: bool,
}

fn place(s: &Session, at: Option<[f64; 2]>) -> Point {
    match at {
        Some([x, y]) => Point::new(x, y),
        None => Point::new(s.view.origin_x, s.view.origin_y),
    }
}

fn check_point(p: Point) -> Result<(), EngineError> {
    let ok = |v: f64| v.is_finite() && v.abs() <= vibenote_model::limits::MAX_COORD;
    if ok(p.x) && ok(p.y) {
        Ok(())
    } else {
        Err(EngineError::Model("position out of range".into()))
    }
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.record.start",
            doc: "Start recording into the note (the test tone or the input device); strokes written meanwhile are linked to it. Recording never blocks inking.",
            reqs: &["R-AUDIO-001", "R-AUDIO-003"],
            implemented: true,
        },
        |s: &mut Session, p: StartP| {
            let note = super::ink::target_note(s, p.note)?;
            let at = place(s, p.at);
            check_point(at)?;
            let source = match p.source {
                SourceP::TestTone => Source::TestTone,
                SourceP::Device => Source::Device,
            };
            let id = s.record_start(
                note,
                &StartParams {
                    source,
                    at,
                    at_us: p.at_us,
                    simulate_seconds: p.simulate_seconds,
                },
            )?;
            let start_us = s.audio.rec.as_ref().map_or(0, |r| r.start_us);
            Ok(Started {
                id: id.0,
                note: note.0,
                start_us,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.record.stop",
            doc: "Stop recording: the recording becomes an audio object in its note (one undo step).",
            reqs: &["R-AUDIO-001"],
            implemented: true,
        },
        |s: &mut Session, p: StopP| {
            s.audio_pump();
            let (id, duration_ms) = s.record_stop(p.id, p.at_us)?;
            Ok(Stopped { id: id.0, duration_ms })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.import",
            doc: "Import an audio file (WAV, FLAC, MP3, Ogg Vorbis, Opus) as a player object; the file is kept unchanged (one undo step).",
            reqs: &["R-AUDIO-001", "R-DATA-004"],
            implemented: true,
        },
        |s: &mut Session, p: ImportP| {
            let note = super::ink::target_note(s, p.note)?;
            let at = place(s, p.at);
            check_point(at)?;
            let len = std::fs::metadata(&p.path)
                .map_err(|e| EngineError::Model(format!("{}: {e}", p.path.display())))?
                .len();
            if len > vibenote_audio::import::MAX_FILE {
                return Err(EngineError::Model("audio file too large".into()));
            }
            let bytes = std::fs::read(&p.path).map_err(|e| EngineError::Model(format!("{}: {e}", p.path.display())))?;
            let pcm = vibenote_audio::import::decode(&bytes).map_err(aerr)?;
            let blob = s.blobs.0.put(&bytes).map_err(|e| EngineError::Storage(e.to_string()))?;
            let id = ObjectId(s.ws.alloc_id()?);
            let duration_ms = pcm.duration_ms();
            let object = CanvasObject {
                id,
                z: s.next_z(note)?,
                kind: ObjectKind::Audio {
                    origin: at,
                    blobs: vec![blob.clone()],
                    duration_ms,
                    recorded: false,
                },
            };
            s.commit(Scope::Note(note), "import audio", vec![Op::InsertObject { note, object }])?;
            Ok(Imported {
                id: id.0,
                blob,
                duration_ms,
                rate: pcm.rate,
                channels: pcm.channels,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.play_at_stroke",
            doc: "Play the recording a stroke was written during, from that moment minus a lead-in (default 2 s).",
            reqs: &["R-AUDIO-002"],
            implemented: true,
        },
        |s: &mut Session, p: PlayStrokeP| {
            let note = super::ink::target_note(s, p.note)?;
            let mark = s
                .ws
                .audio_marks
                .get(&note)
                .and_then(|m| m.get(&ObjectId(p.stroke)))
                .copied()
                .ok_or_else(|| EngineError::Model(format!("stroke {} was not written during a recording", p.stroke)))?;
            let position_ms = mark.offset_ms.saturating_sub(p.lead_in_ms.unwrap_or(LEAD_IN_MS));
            if !p.dry_run {
                s.audio_play(mark.recording, position_ms)?;
            }
            Ok(Played {
                recording: mark.recording.0,
                position_ms,
                playing: !p.dry_run,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.play",
            doc: "Play an audio object from a position.",
            reqs: &["R-AUDIO-001"],
            implemented: true,
        },
        |s: &mut Session, p: PlayP| {
            let pcm = s.audio_pcm(ObjectId(p.id))?;
            let position_ms = p.position_ms.min(pcm.duration_ms());
            if !p.dry_run {
                s.audio_play(ObjectId(p.id), position_ms)?;
            }
            Ok(Played {
                recording: p.id,
                position_ms,
                playing: !p.dry_run,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.stop",
            doc: "Stop playback.",
            reqs: &["R-AUDIO-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoP| Ok(serde_json::json!({ "stopped": s.audio_stop_playback() })),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "audio.status",
            doc: "The recording in progress and the playback position.",
            reqs: &["R-AUDIO-001", "R-AUDIO-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoP| {
            s.audio_pump();
            Ok(s.audio_status())
        },
    )))?;
    Ok(())
}

/// The note an audio mark's stroke belongs to is the stroke's note; marks follow strokes that
/// are split (erasers): the pieces keep their parent's moment.
pub(crate) fn inherit_marks(s: &Session, note: NoteId, parent: ObjectId, pieces: &[ObjectId]) -> Vec<Op> {
    let Some(m) = s.ws.audio_marks.get(&note).and_then(|m| m.get(&parent)).copied() else {
        return Vec::new();
    };
    pieces
        .iter()
        .map(|p| Op::SetAudioMark {
            note,
            stroke: *p,
            after: Some(m),
        })
        .collect()
}
