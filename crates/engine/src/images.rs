//! Images (R-CANVAS-003, R-DATA-004): decoding PNG, JPEG, WebP and SVG with bounded memory, and
//! a display cache with a background decoder (the pen path never decodes).
//!
//! Decoders are pure Rust (`image`, `resvg`); hostile files are bounded by pixel and
//! allocation limits and fail with an error. Display copies are at most `DISPLAY_SIDE`
//! pixels on their long side; the original bytes stay in the blob store unchanged.

use crate::EngineError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// Largest image file imported (bytes).
pub const MAX_FILE: u64 = 256 << 20;
/// Largest decoded side and pixel count accepted (decompression bombs).
pub const MAX_SIDE: u32 = 32_768;
pub const MAX_PIXELS: u64 = 100_000_000;
/// Long side of the decoded copy kept for display.
pub const DISPLAY_SIDE: u32 = 4096;
/// Bytes of decoded images kept.
const CACHE_BYTES: usize = 256 << 20;

/// Decoded pixels: RGBA8, premultiplied alpha, `width * 4` bytes per row.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl AsRef<[u8]> for Decoded {
    fn as_ref(&self) -> &[u8] {
        &self.rgba
    }
}

fn bad(m: impl std::fmt::Display) -> EngineError {
    EngineError::Model(format!("image: {m}"))
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    t.starts_with("<svg") || (t.starts_with("<?xml") && text.contains("<svg")) || t.starts_with("<!--")
}

fn check_size(w: u32, h: u32) -> Result<(), EngineError> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || u64::from(w).saturating_mul(u64::from(h)) > MAX_PIXELS {
        return Err(bad(format!("{w}×{h} pixels is out of range")));
    }
    Ok(())
}

/// Scale factor that fits `w`×`h` into `max` on the long side (never enlarges).
fn fit(w: u32, h: u32, max: u32) -> f64 {
    (f64::from(max) / f64::from(w.max(h).max(1))).min(1.0)
}

/// Decodes `bytes`, scaled down to at most `max_side` on the long side.
pub fn decode(bytes: &[u8], max_side: u32) -> Result<Decoded, EngineError> {
    if is_svg(bytes) {
        return decode_svg(bytes, max_side);
    }
    let mut r = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(bad)?;
    if !matches!(
        r.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)
    ) {
        return Err(bad("not a PNG, JPEG, WebP or SVG file"));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_PIXELS.saturating_mul(4).saturating_add(64 << 20));
    r.limits(limits);
    let img = r.decode().map_err(bad)?;
    check_size(img.width(), img.height())?;
    let k = fit(img.width(), img.height(), max_side);
    let img = if k < 1.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // ≤ max_side
        let (w, h) = (
            ((f64::from(img.width()) * k).round() as u32).max(1),
            ((f64::from(img.height()) * k).round() as u32).max(1),
        );
        img.resize_exact(w, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut px = rgba.into_raw();
    premultiply(&mut px);
    Ok(Decoded {
        width,
        height,
        rgba: px,
    })
}

fn premultiply(px: &mut [u8]) {
    for c in px.as_chunks_mut::<4>().0 {
        let a = u16::from(c[3]);
        for v in &mut c[..3] {
            let p = u16::from(*v).saturating_mul(a).saturating_add(127) / 255;
            *v = u8::try_from(p).unwrap_or(255);
        }
    }
}

/// SVG parse options: `<image>` references are never resolved. usvg's default resolvers read
/// any local path (`/dev/zero`, a FIFO, private files) and parse nested data-URL SVGs; an
/// imported SVG must depend on its own bytes only.
fn svg_options() -> resvg::usvg::Options<'static> {
    resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..resvg::usvg::Options::default()
    }
}

fn decode_svg(bytes: &[u8], max_side: u32) -> Result<Decoded, EngineError> {
    let opt = svg_options();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt).map_err(bad)?;
    let size = tree.size();
    let (w, h) = (f64::from(size.width()), f64::from(size.height()));
    if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0 && w <= f64::from(MAX_SIDE) && h <= f64::from(MAX_SIDE))
    {
        return Err(bad("SVG size out of range"));
    }
    // Vector: render at the display size (up to twice the natural size for sharpness).
    let k = (f64::from(max_side) / w.max(h)).min(2.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // ≤ max_side
    let (pw, ph) = (((w * k).round() as u32).max(1), ((h * k).round() as u32).max(1));
    check_size(pw, ph)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| bad("SVG too large"))?;
    #[allow(clippy::cast_possible_truncation)] // a small scale factor
    let t = resvg::tiny_skia::Transform::from_scale(k as f32, k as f32);
    resvg::render(&tree, t, &mut pixmap.as_mut());
    // tiny-skia pixmaps are premultiplied RGBA already.
    Ok(Decoded {
        width: pw,
        height: ph,
        rgba: pixmap.take(),
    })
}

/// Natural size (pixels) of an image file, checking that it decodes.
pub fn probe(bytes: &[u8]) -> Result<(u32, u32), EngineError> {
    if is_svg(bytes) {
        let opt = svg_options();
        let tree = resvg::usvg::Tree::from_data(bytes, &opt).map_err(bad)?;
        let s = tree.size();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked right after
        let (w, h) = (s.width().ceil() as u32, s.height().ceil() as u32);
        check_size(w, h)?;
        return Ok((w, h));
    }
    // A full (bounded) decode: a file that only has a valid header is not accepted.
    let d = decode(bytes, MAX_SIDE)?;
    Ok((d.width, d.height))
}

/// Decoded images by blob (last use, pixels), total bytes, use counter.
type Cache = (HashMap<String, (u64, Arc<Decoded>)>, usize, u64);

struct Shared {
    cache: Mutex<Cache>,
    queue: Mutex<Vec<(String, Arc<vibenote_storage::BlobStore>)>>,
    /// Blobs that could not be read or decoded: not retried (the bytes cannot change, blobs are
    /// content-addressed), so a damaged file is not re-read on every frame.
    failed: Mutex<std::collections::HashSet<String>>,
    wake: Condvar,
    done: AtomicU64,
    busy: AtomicBool,
    stop: AtomicBool,
}

struct StopOnDrop(Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
        self.0.wake.notify_all();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Display copies of images, decoded in the background.
#[derive(Clone)]
pub struct ImageService {
    shared: Arc<Shared>,
    started: Arc<Mutex<bool>>,
    _guard: Arc<StopOnDrop>,
}

impl Default for ImageService {
    fn default() -> Self {
        let shared = Arc::new(Shared {
            cache: Mutex::new((HashMap::new(), 0, 0)),
            queue: Mutex::default(),
            failed: Mutex::default(),
            wake: Condvar::new(),
            done: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        ImageService {
            _guard: Arc::new(StopOnDrop(Arc::clone(&shared))),
            shared,
            started: Arc::default(),
        }
    }
}

impl std::fmt::Debug for ImageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImageService")
    }
}

impl PartialEq for ImageService {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

fn put(shared: &Shared, blob: String, d: Arc<Decoded>) {
    let mut g = lock(&shared.cache);
    let (map, bytes, tick) = &mut *g;
    *tick = tick.saturating_add(1);
    *bytes = bytes.saturating_add(d.rgba.len());
    if let Some((_, old)) = map.insert(blob, (*tick, d)) {
        *bytes = bytes.saturating_sub(old.rgba.len());
    }
    while *bytes > CACHE_BYTES {
        let Some(victim) = map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) else {
            break;
        };
        if let Some((_, d)) = map.remove(&victim) {
            *bytes = bytes.saturating_sub(d.rgba.len());
        }
    }
}

impl ImageService {
    /// The decoded display copy if cached (marks it used).
    pub fn cached(&self, blob: &str) -> Option<Arc<Decoded>> {
        let mut g = lock(&self.shared.cache);
        let (map, _, tick) = &mut *g;
        *tick = tick.saturating_add(1);
        let t = *tick;
        map.get_mut(blob).map(|(used, d)| {
            *used = t;
            Arc::clone(d)
        })
    }

    /// Decodes now (headless pictures).
    pub fn decode_now(&self, blob: &str, blobs: &vibenote_storage::BlobStore) -> Result<Arc<Decoded>, EngineError> {
        if let Some(d) = self.cached(blob) {
            return Ok(d);
        }
        let bytes = blobs.get(blob).map_err(|e| EngineError::Storage(e.to_string()))?;
        let d = Arc::new(decode(&bytes, DISPLAY_SIDE)?);
        put(&self.shared, blob.to_string(), Arc::clone(&d));
        Ok(d)
    }

    /// Asks the background thread to decode a blob (no-op if cached or queued).
    pub fn want(&self, blob: &str, blobs: &Arc<vibenote_storage::BlobStore>) {
        if lock(&self.shared.cache).0.contains_key(blob) || lock(&self.shared.failed).contains(blob) {
            return;
        }
        let mut q = lock(&self.shared.queue);
        if q.iter().any(|(b, _)| b == blob) {
            return;
        }
        q.push((blob.to_string(), Arc::clone(blobs)));
        drop(q);
        self.start();
        self.shared.wake.notify_one();
    }

    pub fn busy(&self) -> bool {
        !lock(&self.shared.queue).is_empty() || self.shared.busy.load(Ordering::Acquire)
    }

    pub fn finished(&self) -> u64 {
        self.shared.done.load(Ordering::Acquire)
    }

    fn start(&self) {
        let mut started = lock(&self.started);
        if *started {
            return;
        }
        *started = true;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("vibenote-images".into())
            .spawn(move || {
                loop {
                    let (blob, store) = {
                        let mut q = lock(&shared.queue);
                        loop {
                            if shared.stop.load(Ordering::Acquire) {
                                return;
                            }
                            if let Some(j) = q.pop() {
                                shared.busy.store(true, Ordering::Release);
                                break j;
                            }
                            q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                        }
                    };
                    // A failed read or decode stays a placeholder and is not tried again.
                    match store
                        .get(&blob)
                        .ok()
                        .and_then(|bytes| decode(&bytes, DISPLAY_SIDE).ok())
                    {
                        Some(d) => {
                            put(&shared, blob, Arc::new(d));
                            shared.done.fetch_add(1, Ordering::AcqRel);
                        }
                        None => {
                            lock(&shared.failed).insert(blob);
                        }
                    }
                    shared.busy.store(false, Ordering::Release);
                }
            });
        if spawned.is_err() {
            *started = false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn svg_and_hostile_inputs() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="20" height="20" fill="red"/></svg>"#;
        assert_eq!(probe(svg).unwrap(), (40, 20));
        let d = decode(svg, 4096).unwrap();
        assert_eq!((d.width, d.height), (80, 40), "twice the natural size");
        assert_eq!(&d.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &d.rgba[d.rgba.len() - 4..],
            &[0, 0, 0, 0],
            "transparent outside the rect"
        );
        for junk in [
            &b""[..],
            b"\x89PNG\r\n\x1a\n garbage",
            b"<svg",
            b"GIF89a....",
            b"<svg width='100000' height='100000'/>",
        ] {
            assert!(decode(junk, 4096).is_err());
        }
    }

    #[test]
    fn svg_image_references_are_never_resolved() {
        // Regression (M6–M9 review): usvg's default resolver read `/dev/zero` until memory ran
        // out; local files must never be read, and nested data URLs are not decoded either.
        let t = std::time::Instant::now();
        for href in [
            "/dev/zero",
            "/etc/hostname",
            "file:///etc/hostname",
            "data:image/svg+xml;base64,PHN2Zy8+",
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#
            );
            let d = decode(svg.as_bytes(), 64).unwrap();
            assert!(d.rgba.iter().all(|v| *v == 0), "{href}: nothing drawn");
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }
}
