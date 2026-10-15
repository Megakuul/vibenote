//! PDF pages for display (ADR 0004 decision 3, R-PDF-003, R-PERF-006): the worker client, a
//! cache of rendered pages bounded in bytes, and a background thread that renders the pages
//! the view asks for. The UI never waits for pdfium: it asks (`want`), keeps drawing what is
//! cached, and picks up new pages on the next `Engine::pump`.

use crate::EngineError;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use vibenote_pdf::{Bitmap, PdfClient, PdfError};

/// Bytes of rendered pages kept (least recently used dropped first).
const CACHE_BYTES: usize = 512 << 20;
/// Render widths: 256 · √2^k between these bounds (fewer re-renders while zooming; a page is
/// shown at 0.71–1× of its bitmap, or magnified beyond the largest).
pub const MIN_WIDTH: u32 = 256;
pub const MAX_WIDTH: u32 = 4096;
const WIDTHS: [u32; 9] = [256, 362, 512, 724, 1024, 1448, 2048, 2896, 4096];
/// Pending background requests kept (older ones are dropped: the view moved on).
const MAX_PENDING: usize = 64;

/// A rendered page in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageKey {
    /// Index into the service's document table.
    pub doc: u64,
    pub page: u32,
    pub width: u32,
}

/// The render width to ask for when a page is `px` pixels wide on screen.
pub fn width_for(px: f64) -> u32 {
    WIDTHS
        .iter()
        .copied()
        .find(|&w| f64::from(w) >= px)
        .unwrap_or(MAX_WIDTH)
}

struct Cache {
    pages: HashMap<PageKey, (u64, Arc<Bitmap>)>,
    bytes: usize,
    tick: u64,
}

impl Cache {
    fn get(&mut self, k: &PageKey) -> Option<Arc<Bitmap>> {
        self.tick = self.tick.saturating_add(1);
        let t = self.tick;
        self.pages.get_mut(k).map(|(used, b)| {
            *used = t;
            Arc::clone(b)
        })
    }

    fn put(&mut self, k: PageKey, b: Arc<Bitmap>) {
        self.tick = self.tick.saturating_add(1);
        self.bytes = self.bytes.saturating_add(b.rgba.len());
        if let Some((_, old)) = self.pages.insert(k, (self.tick, b)) {
            self.bytes = self.bytes.saturating_sub(old.rgba.len());
        }
        while self.bytes > CACHE_BYTES {
            let Some(victim) = self.pages.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| *k) else {
                break;
            };
            if let Some((_, b)) = self.pages.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(b.rgba.len());
            }
        }
    }

    /// The best cached width for a page: the smallest at least `want`, else the largest.
    fn best(&mut self, doc: u64, page: u32, want: u32) -> Option<(PageKey, Arc<Bitmap>)> {
        let mut have: Vec<u32> = self
            .pages
            .keys()
            .filter(|k| k.doc == doc && k.page == page)
            .map(|k| k.width)
            .collect();
        have.sort_unstable();
        let w = have
            .iter()
            .copied()
            .find(|&w| w >= want)
            .or_else(|| have.last().copied())?;
        let k = PageKey { doc, page, width: w };
        self.get(&k).map(|b| (k, b))
    }
}

/// A document's blob hash and bytes.
/// Reads a document's bytes (from the blob store) when the worker needs them.
pub type Loader = Arc<dyn Fn() -> Result<Arc<[u8]>, EngineError> + Send + Sync>;
type Doc = (String, Loader);

struct Shared {
    client: Mutex<PdfClient>,
    cache: Mutex<Cache>,
    /// Blob hash of each document number, and how to read its bytes (only when the worker
    /// opens it, on the PDF thread: never while drawing a frame).
    docs: Mutex<BTreeMap<u64, Doc>>,
    /// Pending background renders (newest last) and a wake-up for the thread.
    queue: Mutex<Vec<PageKey>>,
    /// Documents whose text the thread extracts when no page waits (R-PDF-004), and results.
    text_queue: Mutex<Vec<u64>>,
    texts: Mutex<Vec<(String, Vec<String>)>>,
    /// Documents whose text extraction failed this session (not queued again).
    text_failed: Mutex<std::collections::HashSet<String>>,
    wake: Condvar,
    /// Pages finished by the background thread (the UI redraws when it grows).
    done: AtomicU64,
    /// Pages whose rendering failed: (doc, page) → (retry not before, failures). A page that
    /// crashes or hangs pdfium must not restart the worker on every frame.
    failed: Mutex<HashMap<(u64, u32), (std::time::Instant, u32)>>,
    /// The thread is rendering a page right now.
    busy: std::sync::atomic::AtomicBool,
    stop: std::sync::atomic::AtomicBool,
}

/// Stops the background thread when the last handle to the service goes (the thread itself
/// holds `Shared`, so this cannot live there).
struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        self.0.wake.notify_all();
    }
}

/// The PDF service of a session (cheap to clone; one worker and cache per engine).
#[derive(Clone)]
pub struct PdfService {
    shared: Arc<Shared>,
    numbers: Arc<Mutex<HashMap<String, u64>>>,
    started: Arc<Mutex<bool>>,
    _guard: Arc<StopOnDrop>,
}

impl std::fmt::Debug for PdfService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfService")
    }
}

impl PartialEq for PdfService {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Default for PdfService {
    fn default() -> Self {
        let shared = Arc::new(Shared {
            client: Mutex::new(PdfClient::new(&vibenote_pdf::find_worker())),
            cache: Mutex::new(Cache {
                pages: HashMap::new(),
                bytes: 0,
                tick: 0,
            }),
            docs: Mutex::default(),
            queue: Mutex::default(),
            text_queue: Mutex::default(),
            texts: Mutex::default(),
            failed: Mutex::default(),
            text_failed: Mutex::default(),
            wake: Condvar::new(),
            done: AtomicU64::new(0),
            busy: std::sync::atomic::AtomicBool::new(false),
            stop: std::sync::atomic::AtomicBool::new(false),
        });
        PdfService {
            _guard: Arc::new(StopOnDrop(Arc::clone(&shared))),
            shared,
            numbers: Arc::default(),
            started: Arc::default(),
        }
    }
}

fn pdf_err(e: PdfError) -> EngineError {
    match e {
        PdfError::Document(m) => EngineError::Model(format!("pdf: {m}")),
        PdfError::Worker(m) => EngineError::Storage(format!("pdf worker: {m}")),
    }
}

/// Delay before a failed page is tried again: 2 s, doubling, at most 5 minutes.
fn retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.saturating_pow(failures.min(9)).min(300))
}

/// Pages that may crash the worker before a document's text extraction is given up.
const MAX_FAILED_PAGES: usize = 3;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PdfService {
    /// The document number of a blob, registering it (bytes kept for the worker).
    pub fn doc(&self, blob: &str, load: Loader) -> u64 {
        let mut numbers = lock(&self.numbers);
        if let Some(n) = numbers.get(blob) {
            return *n;
        }
        let n = u64::try_from(numbers.len()).unwrap_or(u64::MAX);
        numbers.insert(blob.to_string(), n);
        lock(&self.shared.docs).insert(n, (blob.to_string(), load));
        n
    }

    /// Opens a document in the worker; returns its page sizes (import).
    pub fn open(&self, blob: &str, bytes: Arc<[u8]>) -> Result<Vec<vibenote_pdf::PageSize>, EngineError> {
        lock(&self.shared.client).open(blob, bytes).map_err(pdf_err)
    }

    fn ensure_open(client: &mut PdfClient, shared: &Shared, doc: u64) -> Result<String, PdfError> {
        let (blob, load) = lock(&shared.docs)
            .get(&doc)
            .cloned()
            .ok_or_else(|| PdfError::Document(format!("unknown document {doc}")))?;
        if !client.has(&blob) {
            let bytes = load().map_err(|e| PdfError::Document(e.to_string()))?;
            client.open(&blob, bytes)?;
        }
        Ok(blob)
    }

    /// Renders now (commands, headless rendering); the result is cached.
    pub fn render_now(&self, key: PageKey) -> Result<Arc<Bitmap>, EngineError> {
        if let Some(b) = lock(&self.shared.cache).get(&key) {
            return Ok(b);
        }
        let b = {
            let mut c = lock(&self.shared.client);
            let blob = Self::ensure_open(&mut c, &self.shared, key.doc).map_err(pdf_err)?;
            Arc::new(c.render(&blob, key.page, key.width).map_err(pdf_err)?)
        };
        lock(&self.shared.cache).put(key, Arc::clone(&b));
        Ok(b)
    }

    /// The page's text layer (search indexing).
    pub fn text(&self, doc: u64, page: u32) -> Result<String, EngineError> {
        let mut c = lock(&self.shared.client);
        let blob = Self::ensure_open(&mut c, &self.shared, doc).map_err(pdf_err)?;
        c.text(&blob, page).map_err(pdf_err)
    }

    /// The best cached bitmap for a page (never waits for rendering).
    pub fn cached(&self, doc: u64, page: u32, width: u32) -> Option<(PageKey, Arc<Bitmap>)> {
        lock(&self.shared.cache).best(doc, page, width)
    }

    /// Asks the background thread for a page (no-op if cached or already asked for).
    pub fn want(&self, key: PageKey) {
        if lock(&self.shared.cache).pages.contains_key(&key) {
            return;
        }
        if lock(&self.shared.failed)
            .get(&(key.doc, key.page))
            .is_some_and(|(until, _)| std::time::Instant::now() < *until)
        {
            return;
        }
        let mut q = lock(&self.shared.queue);
        if q.contains(&key) {
            return;
        }
        q.push(key);
        if q.len() > MAX_PENDING {
            q.remove(0);
        }
        drop(q);
        self.start_thread();
        self.shared.wake.notify_one();
    }

    /// True while the background thread has pages to render (the UI keeps polling).
    pub fn busy(&self) -> bool {
        !lock(&self.shared.queue).is_empty() || self.shared.busy.load(Ordering::Acquire)
    }

    /// Every page's text, extracted now (unreadable pages are empty).
    pub fn all_text(&self, doc: u64) -> Result<Vec<String>, EngineError> {
        Self::extract(&self.shared, doc).map_err(pdf_err)
    }

    fn extract(shared: &Shared, doc: u64) -> Result<Vec<String>, PdfError> {
        let mut c = lock(&shared.client);
        let blob = Self::ensure_open(&mut c, shared, doc)?;
        let count = c
            .page_count(&blob)
            .unwrap_or(0)
            .min(vibenote_model::limits::MAX_PDF_PAGES);
        let mut pages = Vec::with_capacity(count);
        let mut failed = 0usize;
        for page in 0..count {
            let Ok(n) = u32::try_from(page) else { break };
            match c.text(&blob, n) {
                Ok(t) => pages.push(t),
                // A page pdfium cannot read has no text; the others still count.
                Err(PdfError::Document(_)) => pages.push(String::new()),
                // The worker died on this page (twice): skip it, but a document that keeps
                // killing the worker is given up.
                Err(e) => {
                    failed = failed.saturating_add(1);
                    if failed >= MAX_FAILED_PAGES {
                        return Err(e);
                    }
                    pages.push(String::new());
                }
            }
        }
        Ok(pages)
    }

    /// Asks the background thread for a document's text (after import).
    pub fn want_text(&self, doc: u64) {
        let mut q = lock(&self.shared.text_queue);
        if !q.contains(&doc) {
            q.push(doc);
        }
        drop(q);
        self.start_thread();
        self.shared.wake.notify_one();
    }

    /// True if background text is waiting to be taken (tests poll this).
    pub fn take_texts_pending_for_test(&self) -> bool {
        !lock(&self.shared.texts).is_empty()
    }

    /// Worker processes started so far (tests check that stored data avoids the worker).
    pub fn worker_starts(&self) -> u64 {
        lock(&self.shared.client).starts
    }

    /// True if extracting this document's text failed in the background this session.
    pub fn text_failed(&self, blob: &str) -> bool {
        lock(&self.shared.text_failed).contains(blob)
    }

    /// Text extracted in the background since the last call: (blob, page texts).
    pub fn take_texts(&self) -> Vec<(String, Vec<String>)> {
        std::mem::take(&mut *lock(&self.shared.texts))
    }

    /// Pages the background thread finished so far.
    pub fn finished(&self) -> u64 {
        self.shared.done.load(Ordering::Acquire)
    }

    fn start_thread(&self) {
        let mut started = lock(&self.started);
        if *started {
            return;
        }
        *started = true;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new().name("vibenote-pdf".into()).spawn(move || {
            loop {
                let key = {
                    let mut q = lock(&shared.queue);
                    loop {
                        if shared.stop.load(Ordering::Acquire) {
                            return;
                        }
                        // Newest first: what the view shows now.
                        if let Some(k) = q.pop() {
                            shared.busy.store(true, Ordering::Release);
                            break k;
                        }
                        // No page waits: extract a document's text.
                        let doc = lock(&shared.text_queue).pop();
                        if let Some(doc) = doc {
                            drop(q);
                            let blob = lock(&shared.docs).get(&doc).map(|(b, _)| b.clone());
                            match (Self::extract(&shared, doc), blob) {
                                (Ok(pages), Some(blob)) => {
                                    lock(&shared.texts).push((blob, pages));
                                    shared.done.fetch_add(1, Ordering::AcqRel);
                                }
                                (Err(_), Some(blob)) => {
                                    lock(&shared.text_failed).insert(blob);
                                }
                                _ => {}
                            }
                            q = lock(&shared.queue);
                            continue;
                        }
                        q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                if lock(&shared.cache).pages.contains_key(&key) {
                    shared.busy.store(false, Ordering::Release);
                    continue;
                }
                let result = {
                    let mut c = lock(&shared.client);
                    Self::ensure_open(&mut c, &shared, key.doc).and_then(|blob| c.render(&blob, key.page, key.width))
                };
                // A failed page stays a placeholder and is retried with a growing delay.
                match result {
                    Ok(b) => {
                        lock(&shared.failed).remove(&(key.doc, key.page));
                        lock(&shared.cache).put(key, Arc::new(b));
                        shared.done.fetch_add(1, Ordering::AcqRel);
                    }
                    Err(_) => {
                        let mut f = lock(&shared.failed);
                        let n = f.get(&(key.doc, key.page)).map_or(0, |e| e.1).saturating_add(1);
                        f.insert(
                            (key.doc, key.page),
                            (
                                std::time::Instant::now()
                                    .checked_add(retry_delay(n))
                                    .unwrap_or_else(std::time::Instant::now),
                                n,
                            ),
                        );
                    }
                }
                shared.busy.store(false, Ordering::Release);
            }
        });
        if spawned.is_err() {
            *started = false;
        }
    }

    /// Starts the worker process ahead of the first page.
    pub fn warm_up(&self) {
        let _ = lock(&self.shared.client).warm_up();
    }
}

/// A PDF page or image to show: where, and the best pixels available for it now (RGBA8,
/// premultiplied).
#[derive(Clone)]
pub struct PageImage {
    /// Identifies the pixels (same key, same pixels).
    pub key: u64,
    pub rect: vibenote_geom::Rect,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl std::fmt::Debug for PageImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PageImage({} {}×{} at {:?})",
            self.key, self.width, self.height, self.rect
        )
    }
}

/// A texture key for an image blob (distinct from page keys by construction of the hash).
fn blake_key(blob: &str) -> u64 {
    let mut h = 0x8422_2325_cbf2_9ce4u64;
    for b in blob.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn image_key(k: &PageKey) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in [k.doc, u64::from(k.page), u64::from(k.width)] {
        h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// PDF and image objects of a note (ids in z order), cached by the note's change journal.
#[derive(Default)]
pub(crate) struct PdfIndex(Mutex<Option<(JournalKey, Vec<vibenote_model::ObjectId>)>>);

/// A change journal's (epoch, revision).
type JournalKey = (u64, u64);

impl Clone for PdfIndex {
    fn clone(&self) -> Self {
        PdfIndex::default()
    }
}
impl PartialEq for PdfIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl std::fmt::Debug for PdfIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfIndex")
    }
}

impl crate::Session {
    /// PDF pages and images of the current note visible in a view (`origin`, `zoom`,
    /// surface size in px), in z order, each with the best bitmap available now. Missing ones
    /// are queued for the background renderers (`wait`: made now instead, for headless
    /// pictures). Never waits for pdfium or a decoder unless `wait`.
    pub fn images_in_view(&self, origin: (f64, f64), zoom: f64, size: (f64, f64), wait: bool) -> Vec<PageImage> {
        use vibenote_model::{ObjectKind, pdf_layout};
        let Some(note) = self.current_note else {
            return Vec::new();
        };
        let Ok(canvas) = self.ws.canvas(note) else {
            return Vec::new();
        };
        if !(zoom.is_finite() && zoom > 0.0) {
            return Vec::new();
        }
        let ids = self.non_stroke_ids(note, canvas);
        let view = vibenote_geom::Rect {
            min: vibenote_geom::Point::new(origin.0, origin.1),
            max: vibenote_geom::Point::new(origin.0 + size.0 / zoom, origin.1 + size.1 / zoom),
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(o) = canvas.objects.get(&id) else { continue };
            // A selection being dragged shows where it would land (R-PEN-009).
            let moved = self.drag_preview(note, o);
            let o = moved.as_ref().unwrap_or(o);
            if let ObjectKind::Image {
                origin: io,
                width,
                height,
                blob,
                ..
            } = &o.kind
            {
                let r = vibenote_geom::Rect {
                    min: *io,
                    max: vibenote_geom::Point::new(io.x + width, io.y + height),
                };
                if r.max.x < view.min.x || r.min.x > view.max.x || r.max.y < view.min.y || r.min.y > view.max.y {
                    continue;
                }
                let d = if wait {
                    self.images.decode_now(blob, &self.blobs.0).ok()
                } else {
                    let d = self.images.cached(blob);
                    if d.is_none() {
                        self.images.want(blob, &self.blobs.0);
                    }
                    d
                };
                if let Some(d) = d {
                    let key = blake_key(blob);
                    out.push(PageImage {
                        key,
                        rect: r,
                        width: d.width,
                        height: d.height,
                        pixels: d,
                    });
                }
                continue;
            }
            let ObjectKind::Pdf {
                origin: o_origin,
                scale,
                gap,
                blob,
                pages,
            } = &o.kind
            else {
                continue;
            };
            let doc = crate::commands::pdf::doc_of(self, blob);
            for (r, p) in pdf_layout(*o_origin, *scale, *gap, pages).into_iter().zip(pages) {
                if r.max.x < view.min.x || r.min.x > view.max.x || r.max.y < view.min.y || r.min.y > view.max.y {
                    continue;
                }
                let want = width_for((r.max.x - r.min.x) * zoom);
                let key = PageKey {
                    doc,
                    page: p.index,
                    width: want,
                };
                let best = if wait {
                    self.pdf.render_now(key).ok().map(|b| (key, b))
                } else {
                    let best = self.pdf.cached(doc, p.index, want);
                    if best.as_ref().is_none_or(|(k, _)| k.width < want) {
                        self.pdf.want(key);
                    }
                    best
                };
                if let Some((k, bitmap)) = best {
                    out.push(PageImage {
                        key: image_key(&k),
                        rect: r,
                        width: bitmap.width,
                        height: bitmap.height,
                        pixels: bitmap,
                    });
                }
            }
        }
        out
    }
}

impl crate::Session {
    /// Ids of a note's non-stroke objects in z order, cached by the note's change journal.
    pub(crate) fn non_stroke_ids(
        &self,
        note: vibenote_model::NoteId,
        canvas: &vibenote_model::Canvas,
    ) -> Vec<vibenote_model::ObjectId> {
        use vibenote_model::ObjectKind;
        let key = self.logs.get(&note).map(|l| (l.epoch(), l.rev()));
        let mut g = lock(&self.pdf_index.0);
        match (&*g, key) {
            (Some((k, ids)), Some(key)) if *k == key => ids.clone(),
            _ => {
                let mut v: Vec<_> = canvas
                    .objects
                    .values()
                    .filter(|o| !matches!(o.kind, ObjectKind::Stroke(_)))
                    .map(|o| (o.z, o.id))
                    .collect();
                v.sort_unstable();
                let ids: Vec<_> = v.into_iter().map(|(_, id)| id).collect();
                if let Some(key) = key {
                    *g = Some((key, ids.clone()));
                }
                ids
            }
        }
    }
}
