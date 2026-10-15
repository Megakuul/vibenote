//! Canvas loading (R-PERF-004): `Engine::open` reads folders and notes only; `note.open` reads
//! the objects in view through the R-tree and asks this loader thread for the rest of the note.
//! The thread has its own read-only connection and streams the note in id-ordered pages; the
//! engine merges them in [`crate::Engine::pump`]. The newest request is served first; earlier
//! loads continue after it. Every request carries a generation number so pages of a load the
//! engine no longer wants (the note was completed meanwhile) are dropped.
//!
//! Merging only adds objects that are not in memory and were not removed in memory meanwhile
//! (`Session::merge`); removing or replacing an object that is not in memory, and deleting a
//! note, first read the note completely (`Session::ensure_complete`).

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use vibenote_model::{CanvasObject, NoteId};
use vibenote_storage::Store;

/// Objects per streamed page (decode stays a few ms per page in release builds).
const PAGE: u32 = 2048;
/// Pages decoded ahead of the engine (bounds the memory held in the channel).
const AHEAD: usize = 4;

pub(crate) struct Page {
    pub note: NoteId,
    pub generation: u64,
    pub objects: Vec<CanvasObject>,
    /// Last page of this load (also set after an error).
    pub done: bool,
    pub error: Option<String>,
}

pub(crate) struct Loader {
    requests: Option<Sender<(NoteId, u64)>>,
    pages: Option<Receiver<Page>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Loader {
    pub fn spawn(store: Store) -> Result<Loader, std::io::Error> {
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let (page_tx, page_rx) = std::sync::mpsc::sync_channel(AHEAD);
        let thread = std::thread::Builder::new()
            .name("vibenote-load".into())
            .spawn(move || run(&store, &req_rx, &page_tx))?;
        Ok(Loader {
            requests: Some(req_tx),
            pages: Some(page_rx),
            thread: Some(thread),
        })
    }

    /// Asks for the whole note (`generation` identifies this load).
    pub fn request(&self, note: NoteId, generation: u64) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "loader stopped".to_string())?
            .send((note, generation))
            .map_err(|_| "loader stopped".to_string())
    }

    /// The next page if one is ready.
    pub fn try_page(&self) -> Option<Page> {
        self.pages.as_ref()?.try_recv().ok()
    }

    /// The next page, waiting for it; `None` once the thread has stopped.
    pub fn wait_page(&self) -> Option<Page> {
        self.pages.as_ref()?.recv().ok()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        // Closing both channels unblocks the thread wherever it waits.
        self.requests = None;
        self.pages = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One load in progress: note, generation, last object id read.
type Job = (NoteId, u64, Option<vibenote_model::ObjectId>);

fn run(store: &Store, requests: &Receiver<(NoteId, u64)>, pages: &SyncSender<Page>) {
    // Newest request first (the note just opened), but every queued load runs to its end:
    // a note left half-loaded would stay partial for the session.
    let mut queue: VecDeque<Job> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match requests.recv() {
                Ok((n, g)) => queue.push_front((n, g, None)),
                Err(_) => return,
            }
        }
        while let Ok((n, g)) = requests.try_recv() {
            queue.push_front((n, g, None));
        }
        let Some((note, generation, after)) = queue.pop_front() else {
            continue;
        };
        let (objects, error) = match store.load_page(note, after, PAGE) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let done = error.is_some() || objects.len() < usize::try_from(PAGE).unwrap_or(usize::MAX);
        let after = objects.last().map(|o| o.id).or(after);
        let page = Page {
            note,
            generation,
            objects,
            done,
            error,
        };
        if pages.send(page).is_err() {
            return;
        }
        if !done {
            queue.push_front((note, generation, after));
        }
    }
}
