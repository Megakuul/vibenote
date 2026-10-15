//! Document model: workspace, folders, notes, canvas objects, strokes.
//!
//! The model changes **only** through [`Op`]s. Every op is validated against the current state
//! and [`Workspace::apply`] returns its exact inverse, which the engine uses for undo
//! (R-UNDO-001) and the storage layer for incremental saves. All values that can come from
//! outside (fixtures, control channel, files) are validated here.

pub mod changes;
mod ops;
pub mod search;
pub mod settings;
mod types;

pub use changes::{ChangeLog, Since};
pub use ops::Op;
pub use settings::Settings;
pub use types::*;

use std::collections::BTreeMap;
use thiserror::Error;

/// Limits for hostile input (AGENTS.md §3).
pub mod limits {
    /// Absolute canvas coordinate bound (R-CANVAS-001 needs ±1e7; leave headroom).
    pub const MAX_COORD: f64 = 1.0e9;
    pub const MAX_SAMPLES_PER_STROKE: usize = 1_000_000;
    /// Corners of one shape; largest decoded image side (pixels).
    pub const MAX_SHAPE_POINTS: usize = 10_000;
    pub const MAX_IMAGE_SIDE: u32 = 65_535;
    /// Longest link URL.
    pub const MAX_URL: usize = 4096;
    /// Characters and formatting marks of one text box.
    pub const MAX_TEXT_CHARS: usize = 1_000_000;
    pub const MAX_TEXT_MARKS: usize = 100_000;
    /// Pages of one PDF object (R-PDF-003 asks for 2,000; leave headroom).
    pub const MAX_PDF_PAGES: usize = 100_000;
    /// Largest PDF page side in points (the PDF format's own limit).
    pub const MAX_PDF_PAGE_PT: f64 = 14_400.0;
    pub const MAX_NAME_LEN: usize = 1_000;
    /// Blobs of one audio object (recording chunks of ~10 s: about 11 days) and its length.
    pub const MAX_AUDIO_BLOBS: usize = 100_000;
    pub const MAX_AUDIO_MS: u64 = 30 * 24 * 3600 * 1000;
    pub const MAX_FOLDER_DEPTH: usize = 100_000;
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ModelError {
    #[error("folder {0} not found")]
    FolderNotFound(FolderId),
    #[error("note {0} not found")]
    NoteNotFound(NoteId),
    #[error("object {1} not found in note {0}")]
    ObjectNotFound(NoteId, ObjectId),
    #[error("id {0} already exists")]
    DuplicateId(u64),
    #[error("folder {0} is not empty")]
    FolderNotEmpty(FolderId),
    #[error("{0} is in the trash")]
    Trashed(String),
    #[error("{0} is not in the trash")]
    NotTrashed(String),
    #[error("moving folder {0} into {1} would create a cycle")]
    FolderCycle(FolderId, FolderId),
    #[error("invalid value: {0}")]
    Invalid(String),
}

/// Whole workspace state. `BTreeMap` keeps iteration order deterministic.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    pub folders: BTreeMap<FolderId, Folder>,
    pub notes: BTreeMap<NoteId, Note>,
    pub canvases: BTreeMap<NoteId, Canvas>,
    /// Next id to hand out (shared id space for folders, notes and objects).
    pub next_id: u64,
    /// Notes and folders in the trash (R-ORG-004). They keep their place in the tree and their
    /// canvas until the trash is emptied.
    #[serde(default)]
    pub trashed_notes: std::collections::BTreeSet<NoteId>,
    #[serde(default)]
    pub trashed_folders: std::collections::BTreeSet<FolderId>,
    /// Prettified blocks per note, by block (its first stroke) (ADR 0008).
    #[serde(default)]
    pub prettified: BTreeMap<NoteId, BTreeMap<ObjectId, Prettify>>,
    /// Strokes written while recording, per note: when, in which recording (R-AUDIO-002).
    #[serde(default)]
    pub audio_marks: BTreeMap<NoteId, BTreeMap<ObjectId, AudioMark>>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Default::default()
        }
    }

    /// Allocates a fresh id (deterministic: depends only on prior allocations).
    pub fn alloc_id(&mut self) -> Result<u64, ModelError> {
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| ModelError::Invalid("id space exhausted".into()))?;
        Ok(id)
    }

    pub fn folder(&self, id: FolderId) -> Result<&Folder, ModelError> {
        self.folders.get(&id).ok_or(ModelError::FolderNotFound(id))
    }

    pub fn note(&self, id: NoteId) -> Result<&Note, ModelError> {
        self.notes.get(&id).ok_or(ModelError::NoteNotFound(id))
    }

    pub fn canvas(&self, id: NoteId) -> Result<&Canvas, ModelError> {
        self.canvases.get(&id).ok_or(ModelError::NoteNotFound(id))
    }

    /// True if `f` or one of its ancestors is in the trash.
    pub fn folder_in_trash(&self, f: FolderId) -> bool {
        let mut cur = Some(f);
        for _ in 0..=limits::MAX_FOLDER_DEPTH {
            match cur {
                Some(c) if self.trashed_folders.contains(&c) => return true,
                Some(c) => cur = self.folders.get(&c).and_then(|x| x.parent),
                None => return false,
            }
        }
        true
    }

    /// Children folders of `parent` (None = root), in sort order.
    pub fn child_folders(&self, parent: Option<FolderId>) -> Vec<&Folder> {
        let mut v: Vec<&Folder> = self.folders.values().filter(|f| f.parent == parent).collect();
        v.sort_by(|a, b| a.sort.cmp(&b.sort).then(a.id.cmp(&b.id)));
        v
    }

    /// Notes in `folder` (None = root), in sort order.
    pub fn notes_in(&self, folder: Option<FolderId>) -> Vec<&Note> {
        let mut v: Vec<&Note> = self.notes.values().filter(|n| n.folder == folder).collect();
        v.sort_by(|a, b| a.sort.cmp(&b.sort).then(a.id.cmp(&b.id)));
        v
    }

    /// True if `ancestor` is `f` or an ancestor of `f` (iterative, bounded).
    pub fn is_ancestor_or_self(&self, ancestor: FolderId, f: FolderId) -> bool {
        let mut cur = Some(f);
        for _ in 0..=limits::MAX_FOLDER_DEPTH {
            match cur {
                Some(c) if c == ancestor => return true,
                Some(c) => cur = self.folders.get(&c).and_then(|x| x.parent),
                None => return false,
            }
        }
        true // depth limit reached: treat as cycle (refuse)
    }
}

/// URLs a link may open: http, https and mailto (no local files, R-LINK-001), bounded length,
/// no control characters or spaces.
pub fn url_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    url.len() <= limits::MAX_URL
        && ["http://", "https://", "mailto:"]
            .iter()
            .any(|p| lower.starts_with(p) && lower.len() > p.len())
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}
