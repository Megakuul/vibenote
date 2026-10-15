//! Client side of the PDF worker (ADR 0004): spawns `vibenote-pdfd`, sends requests, and
//! restarts the worker when it crashes, hangs (timeout) or misbehaves. A document that kills
//! the worker twice in a row fails with an error instead of looping.

use crate::proto::{self, PageSize, ProtoError, Request, Response};
use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// Time allowed for one request (opening a 2,000-page PDF included).
const TIMEOUT: Duration = Duration::from_secs(20);
/// Documents kept open at once (bytes here, parsed in the worker); the worker allows 8 too.
const MAX_KEPT: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    /// The PDF itself is the problem (corrupt, encrypted, no such page …).
    #[error("{0}")]
    Document(String),
    /// The worker failed (crash, timeout, protocol); it is restarted on the next request.
    #[error("pdf worker: {0}")]
    Worker(String),
}

impl From<ProtoError> for PdfError {
    fn from(e: ProtoError) -> Self {
        PdfError::Worker(e.to_string())
    }
}

/// A rendered page: RGBA8 (premultiplied), `width * 4` bytes per row.
#[derive(Clone, Debug, PartialEq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl AsRef<[u8]> for Bitmap {
    fn as_ref(&self) -> &[u8] {
        &self.rgba
    }
}

struct Worker {
    child: Child,
    sock: UnixStream,
    open: HashSet<String>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Finds the worker binary: `$VIBENOTE_PDFD`, next to this executable, one directory up (test
/// binaries live in `target/<profile>/deps`), then `$PATH`.
pub fn find_worker() -> PathBuf {
    if let Some(p) = std::env::var_os("VIBENOTE_PDFD") {
        return PathBuf::from(p);
    }
    let name = "vibenote-pdfd";
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(2) {
            let p = dir.join(name);
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from(name)
}

pub struct PdfClient {
    exe: PathBuf,
    worker: Option<Worker>,
    /// Documents' bytes, to reopen them after a restart.
    docs: HashMap<String, Arc<[u8]>>,
    /// Page counts of opened documents.
    counts: HashMap<String, usize>,
    /// Last use of each document (least recently used ones are closed beyond `MAX_KEPT`).
    used: HashMap<String, u64>,
    tick: u64,
    /// Worker processes started so far.
    pub starts: u64,
}

impl PdfClient {
    pub fn new(exe: &Path) -> PdfClient {
        PdfClient {
            exe: exe.to_path_buf(),
            worker: None,
            docs: HashMap::new(),
            counts: HashMap::new(),
            used: HashMap::new(),
            tick: 0,
            starts: 0,
        }
    }

    fn spawn(&mut self) -> Result<Worker, PdfError> {
        let (ours, theirs) = UnixStream::pair().map_err(|e| PdfError::Worker(e.to_string()))?;
        let child = Command::new(&self.exe)
            .stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| PdfError::Worker(format!("starting {}: {e}", self.exe.display())))?;
        ours.set_read_timeout(Some(TIMEOUT))
            .map_err(|e| PdfError::Worker(e.to_string()))?;
        ours.set_write_timeout(Some(TIMEOUT))
            .map_err(|e| PdfError::Worker(e.to_string()))?;
        self.starts = self.starts.saturating_add(1);
        Ok(Worker {
            child,
            sock: ours,
            open: HashSet::new(),
        })
    }

    /// Starts the worker now (so the first page does not pay for it).
    pub fn warm_up(&mut self) -> Result<(), PdfError> {
        if self.worker.is_none() {
            self.worker = Some(self.spawn()?);
        }
        Ok(())
    }

    /// One request on a live worker that has `key` open; the worker is dropped (killed) on
    /// any transport failure. A document the worker evicted (its reply is [`proto::NOT_OPEN`])
    /// is reopened and the request repeated once.
    fn call(&mut self, key: Option<&str>, req: &Request) -> Result<(Response, Option<std::os::fd::OwnedFd>), PdfError> {
        if let Some(k) = key {
            self.touch(k);
        }
        let r = self.call_once(key, req)?;
        match (&r.0, key) {
            (Response::Error { message }, Some(k)) if message == proto::NOT_OPEN => {
                if let Some(w) = self.worker.as_mut() {
                    w.open.remove(k);
                }
                self.call_once(key, req)
            }
            _ => Ok(r),
        }
    }

    fn call_once(
        &mut self,
        key: Option<&str>,
        req: &Request,
    ) -> Result<(Response, Option<std::os::fd::OwnedFd>), PdfError> {
        if self.worker.is_none() {
            self.worker = Some(self.spawn()?);
        }
        let reopen = key.filter(|k| !self.worker.as_ref().is_some_and(|w| w.open.contains(*k)));
        let counts = &mut self.counts;
        let result = (|| -> Result<_, PdfError> {
            let w = self
                .worker
                .as_mut()
                .ok_or_else(|| PdfError::Worker("no worker".into()))?;
            if let Some(k) = reopen {
                let bytes = self
                    .docs
                    .get(k)
                    .ok_or_else(|| PdfError::Document(format!("document {k} not opened")))?;
                let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                proto::send(
                    &w.sock,
                    &Request::Open {
                        key: k.to_string(),
                        payload: len,
                    },
                    Some(bytes),
                    None,
                )?;
                match proto::recv::<Response>(&w.sock)?.0 {
                    Response::Opened { pages } => {
                        w.open.insert(k.to_string());
                        counts.insert(k.to_string(), pages.len());
                    }
                    Response::Error { message } => return Err(PdfError::Document(message)),
                    other => return Err(PdfError::Worker(format!("unexpected reply {other:?}"))),
                }
            }
            proto::send(&w.sock, req, None, None)?;
            Ok(proto::recv::<Response>(&w.sock)?)
        })();
        if matches!(result, Err(PdfError::Worker(_))) {
            self.worker = None; // killed by Drop; the next request starts a fresh one
        }
        result
    }

    /// `call`, retried once on a fresh worker if the worker failed.
    fn call_retry(
        &mut self,
        key: Option<&str>,
        req: &Request,
    ) -> Result<(Response, Option<std::os::fd::OwnedFd>), PdfError> {
        match self.call(key, req) {
            Err(PdfError::Worker(_)) => self.call(key, req),
            r => r,
        }
    }

    /// Opens a document; returns its page sizes. The bytes are kept to reopen it later.
    pub fn open(&mut self, key: &str, bytes: Arc<[u8]>) -> Result<Vec<PageSize>, PdfError> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > proto::MAX_PDF_BYTES {
            return Err(PdfError::Document("PDF too large".into()));
        }
        // At most MAX_KEPT documents (bytes here, open in the worker): the least recently used
        // one is closed first, so the worker never evicts behind our back.
        while !self.docs.contains_key(key) && self.docs.len() >= MAX_KEPT {
            let Some(old) = self
                .used
                .iter()
                .filter(|(k, _)| self.docs.contains_key(*k))
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.close(&old);
        }
        self.docs.insert(key.to_string(), Arc::clone(&bytes));
        self.touch(key);
        if let Some(w) = self.worker.as_mut() {
            w.open.remove(key);
        }
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let req = Request::Open {
            key: key.to_string(),
            payload: len,
        };
        let attempt = |c: &mut Self| -> Result<Vec<PageSize>, PdfError> {
            if c.worker.is_none() {
                c.worker = Some(c.spawn()?);
            }
            let r = (|| -> Result<Response, PdfError> {
                let w = c.worker.as_mut().ok_or_else(|| PdfError::Worker("no worker".into()))?;
                proto::send(&w.sock, &req, Some(&bytes), None)?;
                Ok(proto::recv::<Response>(&w.sock)?.0)
            })();
            match r {
                Ok(Response::Opened { pages }) => {
                    if let Some(w) = c.worker.as_mut() {
                        w.open.insert(key.to_string());
                    }
                    c.counts.insert(key.to_string(), pages.len());
                    Ok(pages)
                }
                Ok(Response::Error { message }) => Err(PdfError::Document(message)),
                Ok(other) => {
                    c.worker = None;
                    Err(PdfError::Worker(format!("unexpected reply {other:?}")))
                }
                Err(e) => {
                    c.worker = None;
                    Err(e)
                }
            }
        };
        let r = match attempt(self) {
            Err(PdfError::Worker(_)) => attempt(self),
            r => r,
        };
        if r.is_err() {
            self.docs.remove(key);
            self.counts.remove(key);
        }
        r
    }

    /// Renders a page (0-based) at `width` pixels.
    pub fn render(&mut self, key: &str, page: u32, width: u32) -> Result<Bitmap, PdfError> {
        let req = Request::Render {
            key: key.to_string(),
            page,
            width,
        };
        match self.call_retry(Some(key), &req)? {
            (Response::Rendered { width, height }, Some(fd)) => {
                let len = proto::bitmap_len(width, height)?;
                let f = std::fs::File::from(fd);
                // The memfd shares its offset with the worker's copy: read from the start.
                if f.metadata().map(|m| m.len()).unwrap_or(0) < u64::try_from(len).unwrap_or(u64::MAX) {
                    self.worker = None;
                    return Err(PdfError::Worker("short bitmap".into()));
                }
                let mut rgba = vec![0u8; len];
                std::os::unix::fs::FileExt::read_exact_at(&f, &mut rgba, 0)
                    .map_err(|e| PdfError::Worker(e.to_string()))?;
                Ok(Bitmap { width, height, rgba })
            }
            (Response::Error { message }, _) => Err(PdfError::Document(message)),
            (other, _) => {
                self.worker = None;
                Err(PdfError::Worker(format!("unexpected reply {other:?}")))
            }
        }
    }

    /// The text layer of a page (0-based).
    pub fn text(&mut self, key: &str, page: u32) -> Result<String, PdfError> {
        let req = Request::Text {
            key: key.to_string(),
            page,
        };
        match self.call_retry(Some(key), &req)?.0 {
            Response::Text { text } => Ok(text),
            Response::Error { message } => Err(PdfError::Document(message)),
            other => {
                self.worker = None;
                Err(PdfError::Worker(format!("unexpected reply {other:?}")))
            }
        }
    }

    /// Asks the worker what its sandbox blocks (needs `VIBENOTE_PDFD_TEST=1` in the worker's
    /// environment).
    pub fn probe(&mut self) -> Result<Response, PdfError> {
        Ok(self.call(None, &Request::Probe)?.0)
    }

    /// True if the document's bytes are known (it can be reopened after a restart).
    pub fn has(&self, key: &str) -> bool {
        self.docs.contains_key(key)
    }

    fn touch(&mut self, key: &str) {
        self.tick = self.tick.saturating_add(1);
        self.used.insert(key.to_string(), self.tick);
    }

    /// Number of pages of an opened document.
    pub fn page_count(&self, key: &str) -> Option<usize> {
        self.counts.get(key).copied()
    }

    /// Forgets a document (its bytes and the worker's copy).
    pub fn close(&mut self, key: &str) {
        self.docs.remove(key);
        self.counts.remove(key);
        self.used.remove(key);
        if let Some(w) = self.worker.as_mut()
            && w.open.remove(key)
        {
            let _ = proto::send(&w.sock, &Request::Close { key: key.to_string() }, None, None)
                .and_then(|()| proto::recv::<Response>(&w.sock).map(|_| ()));
        }
    }

    /// Pid of the running worker (tests kill it to check restarts).
    pub fn worker_pid(&self) -> Option<u32> {
        self.worker.as_ref().map(|w| w.child.id())
    }
}
