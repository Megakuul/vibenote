//! System clipboard and drag-and-drop (wl_data_device): paste and drop files (PDFs, images,
//! audio, text files) from a file manager, image data and plain text from other apps; an
//! in-app copy claims the clipboard with a private type so a later paste knows the objects
//! are vibenote's own (R-UNDO-002).
//!
//! Reading an offer can block (the other app writes when it likes), so it happens on a worker
//! thread; the bytes come back to the event loop through a channel and are inserted there.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The type an in-app copy offers: "paste vibenote's own clipboard".
pub const MARKER: &str = "application/x-vibenote-objects";
/// Most bytes read from one offer (a large image or PDF; anything larger is refused).
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Most files inserted from one paste or drop.
pub const MAX_FILES: usize = 64;

/// What a paste or drop carries, by preference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// vibenote's own clipboard (`clipboard.paste`).
    Own,
    /// `text/uri-list`: files.
    Files(String),
    /// Image bytes of this MIME type.
    Image(String),
    /// UTF-8 text of this MIME type.
    Text(String),
}

impl Kind {
    pub fn mime(&self) -> &str {
        match self {
            Kind::Own => MARKER,
            Kind::Files(m) | Kind::Image(m) | Kind::Text(m) => m,
        }
    }
}

const IMAGE_TYPES: [&str; 7] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
    "image/tiff",
    "image/svg+xml",
];
const TEXT_TYPES: [&str; 4] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING"];

/// The best thing to take from an offer with these types; `text_first` while typing into a
/// text box.
pub fn choose(mimes: &[String], text_first: bool) -> Option<Kind> {
    let has = |m: &str| mimes.iter().any(|x| x.eq_ignore_ascii_case(m));
    if has(MARKER) {
        return Some(Kind::Own);
    }
    let text = TEXT_TYPES.iter().find(|m| has(m)).map(|m| Kind::Text((*m).to_string()));
    if text_first && text.is_some() {
        return text;
    }
    if has("text/uri-list") {
        return Some(Kind::Files("text/uri-list".into()));
    }
    if let Some(m) = IMAGE_TYPES.iter().find(|m| has(m)) {
        return Some(Kind::Image((*m).to_string()));
    }
    text
}

/// Data read from an offer, back on the event loop.
pub struct Pasted {
    pub kind: Kind,
    pub data: Result<Vec<u8>, String>,
    /// Where it goes (surface px): the drop point, or `None` for a paste.
    pub at: Option<(f64, f64)>,
    /// The drag-and-drop offer to finish once read.
    pub dnd: Option<u64>,
}

/// Reads `fd` to the end on a worker thread (bounded) and sends the result.
pub fn read_async(
    fd: std::os::fd::OwnedFd,
    kind: Kind,
    at: Option<(f64, f64)>,
    dnd: Option<u64>,
    tx: sctk_channel::Sender<Pasted>,
) {
    let spawned = std::thread::Builder::new()
        .name("vibenote-paste".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let r = std::fs::File::from(fd)
                .take(MAX_BYTES.saturating_add(1))
                .read_to_end(&mut buf)
                .map_err(|e| e.to_string())
                .and_then(|n| {
                    if u64::try_from(n).unwrap_or(u64::MAX) > MAX_BYTES {
                        Err("too large to paste".to_string())
                    } else {
                        Ok(buf)
                    }
                });
            let _ = tx.send(Pasted { kind, data: r, at, dnd });
        });
    if let Err(e) = spawned {
        eprintln!("vibenote: paste: {e}");
    }
}

pub use smithay_client_toolkit::reexports::calloop::channel as sctk_channel;

/// Local paths in a `text/uri-list` (file URIs only, percent-decoded; at most [`MAX_FILES`]).
pub fn uri_paths(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/path: only the local host.
        .filter_map(|rest| match rest.find('/') {
            Some(0) => Some(rest),
            Some(i) if matches!(rest.get(..i), Some("localhost")) => rest.get(i..),
            _ => None,
        })
        .filter_map(percent_decode)
        .map(PathBuf::from)
        .take(MAX_FILES)
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hex = b.get(i.saturating_add(1)..i.saturating_add(3))?;
            let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(v);
            i = i.saturating_add(3);
        } else {
            out.push(c);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8(out).ok()
}

/// What a file becomes on the canvas, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Image,
    Audio,
    Text,
}

pub fn file_kind(p: &Path) -> Option<FileKind> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => FileKind::Pdf,
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "svg" => FileKind::Image,
        "wav" | "mp3" | "ogg" | "oga" | "opus" | "flac" => FileKind::Audio,
        "txt" | "md" | "markdown" | "text" => FileKind::Text,
        _ => return None,
    })
}

/// File extension for pasted image bytes of `mime`.
pub fn image_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn v(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn choice_prefers_own_then_files_then_images_then_text() {
        assert_eq!(choose(&v(&["text/plain", MARKER]), false), Some(Kind::Own));
        assert_eq!(
            choose(&v(&["text/plain", "text/uri-list"]), false),
            Some(Kind::Files("text/uri-list".into()))
        );
        assert_eq!(
            choose(&v(&["text/plain", "text/uri-list"]), true),
            Some(Kind::Text("text/plain".into())),
            "typing: text wins"
        );
        assert_eq!(
            choose(&v(&["image/jpeg", "image/png"]), false),
            Some(Kind::Image("image/png".into()))
        );
        assert_eq!(choose(&v(&["application/octet-stream"]), false), None);
    }

    #[test]
    fn uri_lists_give_local_paths_only() {
        let l = "# comment\r\nfile:///home/u/My%20Notes.pdf\r\nfile://localhost/tmp/a.png\r\nhttps://x/y.png\r\nfile://otherhost/z.pdf\r\nfile:///bad%zz\n";
        assert_eq!(
            uri_paths(l),
            vec![PathBuf::from("/home/u/My Notes.pdf"), PathBuf::from("/tmp/a.png")]
        );
        assert_eq!(file_kind(Path::new("/a/B.PDF")), Some(FileKind::Pdf));
        assert_eq!(file_kind(Path::new("/a/b.jpeg")), Some(FileKind::Image));
        assert_eq!(file_kind(Path::new("/a/b.exe")), None);
    }
}
