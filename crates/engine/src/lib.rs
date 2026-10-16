//! Engine: session state, command registry, undo. Drives everything without UI.
//!
//! Determinism (AGENTS.md §4): engine behaviour depends only on the commands it receives
//! (and their params, which include any timestamps). No clocks, no randomness.

pub mod audio;
pub mod command;
mod commands;
mod extras;
pub mod gesture;
mod hit;
pub mod hwr;
pub mod images;
mod load;
pub mod pdf;
mod persist;
pub mod quickopen;
pub mod search;

pub use command::{Command, Registry, Spec};
pub use extras::{VIM_COLUMNS, frame_of as text_frame, vim_pad};
pub use gesture::{CursorHint, Handle};
pub use persist::Status as StorageStatus;

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use vibenote_geom::Rect;
use vibenote_model::{CanvasObject, ModelError, NoteId, Op, Workspace};

#[derive(Debug, Error, Clone, PartialEq, Serialize)]
#[serde(tag = "error", content = "detail", rename_all = "snake_case")]
pub enum EngineError {
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("invalid params for `{cmd}`: {msg}")]
    InvalidParams { cmd: String, msg: String },
    #[error("command `{0}` is not enabled in the current state")]
    Disabled(String),
    #[error("command `{0}` is not implemented yet")]
    NotImplemented(String),
    #[error("{0}")]
    Model(String),
    #[error("nothing to {0}")]
    NothingTo(&'static str),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("storage: {0}")]
    Storage(String),
}

impl From<ModelError> for EngineError {
    fn from(e: ModelError) -> Self {
        EngineError::Model(e.to_string())
    }
}

/// Undo scope: per note (R-UNDO-001) or workspace-level (folder/note management).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Workspace,
    Note(NoteId),
}

/// One undoable step: the inverse ops (apply in order to undo) plus a label.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub label: String,
    pub inverse: Vec<Op>,
}

/// Maximum undo steps kept per scope (SPEC R-UNDO-001: 1,000).
pub const UNDO_LIMIT: usize = 1000;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct History {
    pub undo: Vec<Step>,
    pub redo: Vec<Step>,
}

/// Mapping from surface (logical px) to canvas units: canvas = origin + surface / zoom.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct View {
    pub origin_x: f64,
    pub origin_y: f64,
    pub zoom: f64,
}

impl Default for View {
    fn default() -> Self {
        View {
            origin_x: 0.0,
            origin_y: 0.0,
            zoom: 1.0,
        }
    }
}

impl View {
    pub fn to_canvas(&self, sx: f64, sy: f64) -> (f64, f64) {
        (self.origin_x + sx / self.zoom, self.origin_y + sy / self.zoom)
    }
}

/// Live (uncommitted) pen state. The renderer draws `stroke` before it is committed.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LiveInput {
    /// Tool in proximity, if any.
    pub tool: Option<vibenote_ink::input::PenTool>,
    pub stroke: Option<LiveStroke>,
    /// Last hover position (canvas units) for the tool cursor (R-PEN-011).
    pub hover: Option<(f64, f64)>,
    /// Barrel button held (Linux code; R-PEN-010).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<u32>,
    /// Pan in progress (middle mouse button, or a pan barrel button): last position (surface
    /// px; R-PEN-012, R-CANVAS-002).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pan: Option<(f64, f64)>,
    /// The pan is the pen's (pan barrel button), not the mouse's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pen_pan: bool,
    /// The selection is being dragged by its box or a handle (R-PEN-009).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drag: Option<gesture::Drag>,
    /// Corners of a polygon being placed with the shape tool, one per tap (canvas units).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub polygon: Vec<(f64, f64)>,
    /// The live stroke was turned into a shape by holding still (R-SHAPE-002): the rest of
    /// this touch draws nothing (`true` for the mouse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapped: Option<bool>,
    /// The last press (time µs, surface px) and whether the current press is a double click
    /// with it (text: a double click selects the word).
    #[serde(skip)]
    pub last_down: Option<(u64, f64, f64)>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub double: bool,
    /// Shift is held (the last pointer event said so).
    #[serde(skip)]
    pub shift: bool,
    /// Presses in a row at one spot: 1, 2 (double click: a word), 3 (triple click: a line).
    #[serde(skip)]
    pub clicks: u8,
    /// A press into a text box's text that a drag extends into a selection (R-TEXT-005):
    /// where it started, and whether it is the mouse's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_drag: Option<((f64, f64), bool)>,
    /// The ruler is being moved or rotated (R-RULER-001).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruler_drag: Option<commands::shape::RulerDrag>,
}

/// The stroke the pen tool commits (and the live preview draws): same tool, colour and width.
pub fn pen_stroke(samples: Vec<vibenote_model::Sample>) -> vibenote_model::Stroke {
    vibenote_model::Stroke {
        tool: vibenote_model::Tool::Pen,
        color: vibenote_model::Color::INK,
        width: 2.0,
        samples,
    }
}

/// Most object ids one command takes (hostile input).
pub(crate) const MAX_IDS: usize = 1 << 20;

/// How far ink may reach beyond its stored R-tree box (0.25 × the largest stroke width).
const STORED_BOX_SLACK: f64 = 2_500.0;

/// Linux codes of the barrel buttons (R-PEN-010).
pub(crate) const BTN_STYLUS: u32 = 331;
pub(crate) const BTN_STYLUS2: u32 = 332;
/// Eraser disc radius on screen (logical px); in canvas units it shrinks as you zoom in.
pub(crate) const ERASER_RADIUS_PX: f64 = 8.0;
/// Colour of the eraser's swept path in the overlay.
const ERASER_COLOR: vibenote_model::Color = vibenote_model::Color(0x8a8a8a50);
/// Overlay (lasso path, selection box): colour and on-screen width in px.
pub(crate) const OVERLAY_COLOR: vibenote_model::Color = vibenote_model::Color(0x3b7be0d0);
pub(crate) const OVERLAY_PX: f64 = 1.5;

/// Default highlighter colour (yellow; the renderer makes highlighter ink translucent).
pub const HIGHLIGHTER_COLOR: vibenote_model::Color = vibenote_model::Color(0xf5c800ff);

/// The stroke the selected tool commits from pen-tip samples (R-PEN-005), or `None` for tools
/// that do not draw ink (erasers, lasso, shapes, text: their own milestones).
pub fn tool_stroke(
    settings: &vibenote_model::settings::Settings,
    samples: Vec<vibenote_model::Sample>,
) -> Option<vibenote_model::Stroke> {
    use vibenote_model::settings::ToolKind;
    match settings.tool {
        ToolKind::Pen => Some(vibenote_model::Stroke {
            color: settings.pen_color,
            ..pen_stroke(samples)
        }),
        ToolKind::Highlighter => Some(vibenote_model::Stroke {
            tool: vibenote_model::Tool::Highlighter,
            color: settings.highlighter_color,
            width: 12.0,
            samples,
        }),
        ToolKind::Cover => Some(vibenote_model::Stroke {
            tool: vibenote_model::Tool::Cover,
            color: COVER_COLOR,
            width: COVER_WIDTH,
            samples,
        }),
        _ => None,
    }
}

/// Colour and width (canvas units) of a cover (R-PEN-014).
pub const COVER_COLOR: vibenote_model::Color = vibenote_model::Color(0x5b65_77ff);
pub const COVER_WIDTH: f32 = 28.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LiveStroke {
    pub note: NoteId,
    pub tool: vibenote_ink::input::PenTool,
    pub t0_us: u64,
    pub samples: Vec<vibenote_model::Sample>,
    /// Barrel button held when the stroke began: it lassoes or erases instead of inking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<u32>,
    /// Drawn with the mouse (R-PEN-012): pen events never extend or end it, and vice versa.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mouse: bool,
}

/// Selected objects (R-PEN-009), in one note.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Selection {
    pub note: Option<NoteId>,
    pub ids: Vec<u64>,
    /// The selection's objects before and after the last transform (`invert_last` restores
    /// exactly while nothing else changed them).
    #[serde(skip)]
    pub(crate) last: Option<(Vec<CanvasObject>, Vec<CanvasObject>)>,
}

/// Shared handle to the blob store (a session's identity ignores it).
#[derive(Clone, Debug)]
pub struct BlobHandle(pub std::sync::Arc<vibenote_storage::BlobStore>);

impl Default for BlobHandle {
    fn default() -> Self {
        BlobHandle(std::sync::Arc::new(vibenote_storage::BlobStore::in_memory()))
    }
}
impl PartialEq for BlobHandle {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Cache of the selection's bounds: (note log epoch, log rev, hash of the selected ids) → box.
#[derive(Default)]
struct OverlayCache(std::sync::Mutex<Option<(OverlayKey, Rect)>>);

type OverlayKey = (u64, u64, u64);

impl OverlayCache {
    fn get(&self, key: OverlayKey) -> Option<Rect> {
        let g = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        g.as_ref().filter(|(k, _)| *k == key).map(|(_, r)| *r)
    }
    fn put(&self, key: OverlayKey, r: Rect) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, r));
    }
}

// A derived cache: copies start empty and never affect a session's identity.
impl Clone for OverlayCache {
    fn clone(&self) -> Self {
        OverlayCache::default()
    }
}
impl PartialEq for OverlayCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl std::fmt::Debug for OverlayCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OverlayCache")
    }
}

/// UI state that commands change and the UI reads (no dialogs from programmatic calls: these
/// only record what the UI should show).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UiState {
    /// Open panel: "quickopen", "find" or "search".
    pub panel: Option<&'static str>,
    /// The app should close (Ctrl+Q).
    pub quit: bool,
    /// A folder link asked to show this folder in the tree.
    pub reveal_folder: Option<u64>,
}

/// The in-app clipboard: copies of objects, independent of later edits to the originals.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Clipboard {
    /// Objects copied (shown as a count in `state.inspect`).
    #[serde(rename = "count", serialize_with = "count")]
    pub(crate) objects: Vec<CanvasObject>,
    /// Note they were copied from.
    pub note: Option<NoteId>,
    /// Pastes of this content into its own note so far (each lands a bit further off).
    #[serde(skip)]
    pub(crate) pastes_home: u32,
}

fn count<S: serde::Serializer>(v: &[CanvasObject], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(u64::try_from(v.len()).unwrap_or(u64::MAX))
}

/// Read-only access to the stored workspace (a second connection next to the storage thread):
/// undo/redo logs per scope (R-UNDO-001) and canvases of notes being opened (R-PERF-004). Not
/// part of the session's identity (equality ignores it).
#[derive(Clone, Default)]
pub struct StoreReader(Option<std::sync::Arc<std::sync::Mutex<vibenote_storage::Store>>>);

impl std::fmt::Debug for StoreReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "StoreReader(store)"
        } else {
            "StoreReader(none)"
        })
    }
}

impl PartialEq for StoreReader {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl StoreReader {
    /// Runs `f` on the store; `None` for an in-memory session.
    fn with<T>(
        &self,
        f: impl FnOnce(&vibenote_storage::Store) -> Result<T, vibenote_storage::StorageError>,
    ) -> Option<Result<T, String>> {
        let m = self.0.as_ref()?;
        let st = m.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(f(&st).map_err(|e| e.to_string()))
    }
}

/// How much of a note's canvas is in memory (R-PERF-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadState {
    /// Only objects added in this session (the note has not been opened).
    Unloaded,
    /// The first viewport is in; the rest streams in the background.
    Partial,
    Complete,
}

/// Load bookkeeping of an incomplete note (complete notes have none).
#[derive(Clone, Debug, PartialEq)]
struct NoteLoad {
    state: LoadState,
    /// Identifies the background load; pages of another generation are dropped.
    generation: u64,
    /// Topmost stored objects (z, id), highest first, once read (`None` = not read yet). Only
    /// entries not in memory count (see [`Session::next_z`]).
    top: Option<Vec<(i64, vibenote_model::ObjectId)>>,
    /// Objects removed in memory while the note was incomplete. Their rows may still be on
    /// disk (the removal is queued), so merging must not bring them back.
    removed: BTreeSet<vibenote_model::ObjectId>,
}

impl NoteLoad {
    fn unloaded() -> Self {
        NoteLoad {
            state: LoadState::Unloaded,
            generation: 0,
            top: None,
            removed: BTreeSet::new(),
        }
    }
}

/// Topmost stored objects remembered per incomplete note for z allocation.
const TOP_Z: u32 = 64;

/// Objects per read when a note is completed synchronously.
const COMPLETE_PAGE: u32 = 8192;

fn record(step: &Step) -> vibenote_storage::StepRecord {
    vibenote_storage::StepRecord {
        label: step.label.clone(),
        ops: step.inverse.clone(),
    }
}

fn from_record(r: vibenote_storage::StepRecord) -> Step {
    Step {
        label: r.label,
        inverse: r.ops,
    }
}

/// Storage problems that did not stop the command (kept for `state.inspect`, bounded).
const MAX_WARNINGS: usize = 32;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Session {
    pub ws: Workspace,
    pub current_note: Option<NoteId>,
    pub view: View,
    pub live: LiveInput,
    /// Selected objects (`/selection`, R-PEN-009).
    pub selection: Selection,
    /// Copied objects (R-UNDO-002; in-app; the system clipboard arrives with M11).
    pub clipboard: Clipboard,
    /// Recently opened notes, most recent first (R-ORG-003; at most `quickopen::RECENT`).
    pub recent: Vec<NoteId>,
    /// Keymap changes against the defaults (R-CMD-002): key → command, `None` = unbound.
    pub keymap_overrides: std::collections::BTreeMap<String, Option<String>>,
    /// The ruler (R-RULER-001).
    pub ruler: commands::shape::Ruler,
    /// Covers revealed by a tap (R-PEN-014); all close again when a note is opened.
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub revealed: std::collections::BTreeSet<vibenote_model::ObjectId>,
    /// Writing blocks prettify would tidy, in mode "suggest" (R-HWR-006).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<hwr::Suggestion>,
    /// The text box being typed into with the text tool (R-TEXT-001/002).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_edit: Option<commands::text::TextEdit>,
    /// Typing into this text box extends the last undo step instead of adding one.
    #[serde(skip)]
    pub(crate) coalesce: Option<(NoteId, u64)>,
    /// Vim's unnamed register: text and whether it holds whole lines (R-VIM-002).
    #[serde(skip)]
    pub(crate) register: Option<(String, bool)>,
    /// A leader-key sequence in progress ("␠", "␠f") and when its last key came (ms; R-VIM-004).
    #[serde(skip)]
    pub(crate) leader: Option<(String, Option<u64>)>,
    /// What the UI shows besides the canvas (panels opened by commands; R-CMD-001).
    pub ui: UiState,
    /// Ids of all registered commands (keymap validation).
    #[serde(skip)]
    pub(crate) command_ids: Vec<&'static str>,
    /// Large assets (PDFs, images, audio; ADR 0005). In memory for engines without a file.
    #[serde(skip)]
    pub blobs: BlobHandle,
    /// PDF worker, page cache and background page renderer (ADR 0004).
    #[serde(skip)]
    pub pdf: pdf::PdfService,
    #[serde(skip)]
    pub(crate) pdf_index: pdf::PdfIndex,
    /// Decoded images for display (R-CANVAS-003).
    #[serde(skip)]
    pub images: images::ImageService,
    /// Search index of an engine without a workspace file (R-SEARCH-001).
    #[serde(skip)]
    pub(crate) mem_search: search::MemSearch,
    /// Handwriting recogniser, classifier and background jobs (ADR 0006–0008).
    #[serde(skip)]
    pub hwr: hwr::HwrService,
    /// The recording in progress and playback (ADR 0009).
    #[serde(skip)]
    pub audio: audio::AudioService,
    /// The writing block being written (R-HWR-001).
    pub open_block: Option<hwr::OpenBlock>,
    /// Strokes of blocks sent to recognition, by (note, first stroke).
    #[serde(skip)]
    pub(crate) block_members: std::collections::BTreeMap<(NoteId, vibenote_model::ObjectId), Vec<u64>>,
    /// Pack version that read each block (missing: not read, or its text was cleared).
    #[serde(skip)]
    pub(crate) block_tags: std::collections::HashMap<(NoteId, vibenote_model::ObjectId), String>,
    /// Notes whose stored blocks are in `block_members` / `block_tags`.
    #[serde(skip)]
    pub(crate) blocks_loaded: BTreeSet<NoteId>,
    /// Opened notes whose blocks are checked for re-recognition once they are complete.
    #[serde(skip)]
    pub(crate) sweep_notes: BTreeSet<NoteId>,
    /// Blocks whose text was cleared because none of their strokes was left, by stroke: if one
    /// comes back (undo), the note is swept again.
    #[serde(skip)]
    pub(crate) cleared_strokes: std::collections::HashMap<vibenote_model::ObjectId, NoteId>,
    /// Derived data to store with the next flush.
    #[serde(skip)]
    pub(crate) derived_pending: Vec<vibenote_storage::Derived>,
    /// PDFs whose text could not be extracted this session (not retried until a restart; their
    /// pages are simply not found by search).
    #[serde(skip)]
    pub(crate) pdf_text_failed: std::collections::HashSet<String>,
    /// The last search ran without waiting and indexing was still in progress.
    #[serde(skip)]
    pub(crate) search_pending: bool,
    /// Changes of prettify parameters (cached display-dependent values key on it).
    #[serde(skip)]
    pub(crate) prettify_rev: u64,
    /// A command asked for blob garbage collection (runs after the command, once storage is
    /// in sync).
    #[serde(skip)]
    pub(crate) gc_requested: bool,
    /// Bounds of the selection for the overlay, cached (review M4 M3).
    #[serde(skip)]
    overlay_cache: OverlayCache,
    /// Named session values changed since the last flush (`recent`, `keymap`).
    #[serde(skip)]
    pub(crate) meta_dirty: std::collections::BTreeSet<&'static str>,
    /// Bumped whenever notes or folders change (quick-open's index follows it).
    #[serde(skip)]
    pub(crate) notes_rev: u64,
    #[serde(skip)]
    pub(crate) quick: quickopen::QuickIndex,
    /// Keyed by [`Scope::key`] ("workspace", "note:<id>") so the state serialises as JSON.
    pub history: std::collections::BTreeMap<String, History>,
    /// Workspace settings (`/settings`; M2.4). Not undoable; persisted with the workspace.
    pub settings: vibenote_model::Settings,
    /// Settings changed since the last flush.
    #[serde(skip)]
    pub settings_dirty: bool,
    /// Ops applied since the last flush to storage (forward direction, in order).
    #[serde(skip)]
    pub pending: Vec<Op>,
    /// History changes since the last flush (persistent sessions only), same batch as `pending`.
    #[serde(skip)]
    pub pending_history: Vec<vibenote_storage::HistoryOp>,
    /// Scopes whose stored history has been loaded into `history`.
    #[serde(skip)]
    loaded: std::collections::BTreeSet<String>,
    #[serde(skip)]
    pub reader: StoreReader,
    /// Incomplete canvases (notes without an entry are complete).
    #[serde(skip)]
    loads: BTreeMap<NoteId, NoteLoad>,
    #[serde(skip)]
    generation: u64,
    /// Background loads to start (note, generation); the engine hands them to its loader.
    #[serde(skip)]
    load_requests: Vec<(NoteId, u64)>,
    /// Per-note change journals for consumers that cache derived data (the renderer, M3.1b).
    #[serde(skip)]
    logs: BTreeMap<NoteId, vibenote_model::ChangeLog>,
    #[serde(skip)]
    next_epoch: u64,
    /// Spatial lookup per note for hit-testing (erasers), kept current via `logs`.
    #[serde(skip)]
    hit: BTreeMap<NoteId, hit::HitIndex>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Scope::Workspace => "workspace".into(),
            Scope::Note(n) => format!("note:{n}"),
        }
    }
}

impl Session {
    pub fn new() -> Self {
        Session {
            ws: Workspace::new(),
            ..Default::default()
        }
    }

    fn persistent(&self) -> bool {
        self.reader.0.is_some()
    }

    fn warn(&mut self, w: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(w);
        }
    }

    /// Loads the stored history of `key` on first use. A read failure leaves an empty history
    /// (new steps are still recorded) and a warning; it never blocks the command.
    pub(crate) fn ensure_loaded(&mut self, key: &str) {
        if !self.persistent() || self.loaded.contains(key) {
            return;
        }
        self.loaded.insert(key.to_string());
        match self.reader.with(|st| st.load_history(key)) {
            Some(Ok(h)) => {
                let mut undo: Vec<Step> = h.undo.into_iter().map(from_record).collect();
                if undo.len() > UNDO_LIMIT {
                    undo.drain(..undo.len().saturating_sub(UNDO_LIMIT));
                }
                let redo = h.redo.into_iter().map(from_record).collect();
                self.history.insert(key.to_string(), History { undo, redo });
            }
            Some(Err(e)) => {
                // Start this scope empty on disk too, so stored steps that no longer match
                // what memory holds cannot come back after a reopen.
                self.warn(format!("history of {key} unreadable, cleared: {e}"));
                for stack in [vibenote_storage::Stack::Undo, vibenote_storage::Stack::Redo] {
                    self.log(vibenote_storage::HistoryOp::Clear {
                        scope: key.to_string(),
                        stack,
                    });
                }
            }
            None => {}
        }
    }

    /// Changes to folders and notes (names, structure, colours, trash): the UI rebuilds its tree
    /// only when this moves.
    pub fn notes_revision(&self) -> u64 {
        self.notes_rev
    }

    pub fn load_state(&self, note: NoteId) -> LoadState {
        self.loads.get(&note).map_or(LoadState::Complete, |l| l.state)
    }

    /// Notes still streaming in.
    pub fn loading_notes(&self) -> Vec<NoteId> {
        self.loads
            .iter()
            .filter(|(_, l)| l.state == LoadState::Partial)
            .map(|(n, _)| *n)
            .collect()
    }

    /// Bounds of `note`'s stored objects that are not in memory yet (lazy open), from the
    /// R-tree; `None` for complete notes.
    pub(crate) fn stored_bounds(&self, note: NoteId) -> Result<Option<Rect>, EngineError> {
        if !self.loads.contains_key(&note) {
            return Ok(None);
        }
        match self.reader.with(|st| st.note_bounds(note)) {
            Some(r) => r.map_err(|e| EngineError::Storage(format!("reading note {note}: {e}"))),
            None => Ok(None),
        }
    }

    /// The live stroke as the selected tool will commit it (the UI draws it before pen-up).
    pub fn live_preview(&self) -> Option<vibenote_model::Stroke> {
        self.live
            .stroke
            .as_ref()
            .filter(|l| l.tool == vibenote_ink::input::PenTool::Pen && self.barrel(l.button).is_none())
            .and_then(|l| tool_stroke(&self.settings, l.samples.clone()))
            .map(|mut st| {
                // Laid along the ruler already while drawing (R-RULER-001).
                commands::shape::ruler_snap(self, &mut st);
                st
            })
    }

    /// What a held barrel button does (`None`: no button, or one set to do nothing).
    pub(crate) fn barrel(&self, button: Option<u32>) -> Option<vibenote_model::settings::BarrelAction> {
        use vibenote_model::settings::BarrelAction;
        let a = match button? {
            BTN_STYLUS => self.settings.barrel1,
            BTN_STYLUS2 => self.settings.barrel2,
            _ => return None,
        };
        (a != BarrelAction::None).then_some(a)
    }

    /// True if the live stroke draws a lasso (a lasso barrel button, or the lasso tool with the
    /// pen tip; R-PEN-009/010).
    pub(crate) fn lassoing(&self, ls: &LiveStroke) -> bool {
        use vibenote_model::settings::{BarrelAction, ToolKind};
        match self.barrel(ls.button) {
            Some(a) => a == BarrelAction::Lasso,
            None => ls.tool == vibenote_ink::input::PenTool::Pen && self.settings.tool == ToolKind::Lasso,
        }
    }

    /// The eraser mode if the live stroke erases (eraser end, an eraser barrel button, or an
    /// eraser tool with the pen tip; R-PEN-006..008/010).
    pub(crate) fn erasing(&self, ls: &LiveStroke) -> Option<vibenote_model::settings::EraserMode> {
        use vibenote_ink::input::PenTool;
        use vibenote_model::settings::{BarrelAction, EraserMode, ToolKind};
        match (ls.tool, self.barrel(ls.button), self.settings.tool) {
            (PenTool::Eraser, _, _) | (_, Some(BarrelAction::Eraser), _) => Some(self.settings.eraser_mode),
            (PenTool::Pen, None, ToolKind::EraserStroke) => Some(EraserMode::Stroke),
            (PenTool::Pen, None, ToolKind::EraserArea) => Some(EraserMode::Area),
            _ => None,
        }
    }

    /// Feedback drawn over the canvas like live ink, `px` screen pixels wide at the current
    /// zoom: the lasso being drawn, the eraser's path, the selection's box with its handles and
    /// the tool's footprint where the pen or mouse hovers.
    /// Display only; never stored.
    pub fn overlay(&self) -> Vec<vibenote_model::Stroke> {
        let zoom = if self.view.zoom.is_finite() && self.view.zoom > 0.0 {
            self.view.zoom
        } else {
            1.0
        };
        #[allow(clippy::cast_possible_truncation)] // a few pixels in canvas units
        let line = |pts: Vec<(f64, f64)>| vibenote_model::Stroke {
            tool: vibenote_model::Tool::Pen,
            color: OVERLAY_COLOR,
            width: (OVERLAY_PX / zoom) as f32,
            samples: pts
                .into_iter()
                .map(|(x, y)| vibenote_model::Sample {
                    x,
                    y,
                    pressure: 1.0,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: 0,
                })
                .collect(),
        };
        // The ruler first: lasso paths and selection boxes stay visible over it.
        let mut out = self.ruler.strokes(zoom);
        if let Some(ls) = self.live.stroke.as_ref().filter(|l| self.lassoing(l)) {
            let mut pts: Vec<(f64, f64)> = ls.samples.iter().map(|p| (p.x, p.y)).collect();
            if let Some(&first) = pts.first() {
                pts.push(first);
            }
            out.push(line(pts));
        }
        // The eraser's swept path, as wide as the eraser (review M4 M6: no blind erasing).
        if let Some(ls) = self.live.stroke.as_ref().filter(|l| self.erasing(l).is_some()) {
            let mut s = line(ls.samples.iter().map(|p| (p.x, p.y)).collect());
            s.color = ERASER_COLOR;
            #[allow(clippy::cast_possible_truncation)] // a few pixels in canvas units
            {
                s.width = (2.0 * ERASER_RADIUS_PX / zoom) as f32;
            }
            out.push(s);
        }
        self.overlay_controls(&mut out);
        out
    }

    /// Objects of `note` whose boxes meet `area`. Completes a partly loaded note first: what
    /// is hit-tested must be all of it.
    pub(crate) fn candidates(
        &mut self,
        note: NoteId,
        area: &Rect,
    ) -> Result<Vec<vibenote_model::ObjectId>, EngineError> {
        self.ensure_area(note, area)?;
        let canvas = self.ws.canvas(note)?;
        let idx = self.hit.entry(note).or_default();
        idx.sync(canvas, self.logs.get(&note));
        Ok(idx.query(area))
    }

    /// Objects of `note` whose boxes come within `radius` of `path` (erasers). Like
    /// [`Session::candidates`], but one small query per segment.
    pub(crate) fn candidates_along(
        &mut self,
        note: NoteId,
        path: &[(f64, f64)],
        radius: f64,
    ) -> Result<Vec<vibenote_model::ObjectId>, EngineError> {
        let mut area = vibenote_geom::Rect::EMPTY;
        for &(x, y) in path {
            area.include(vibenote_geom::Point::new(x, y));
        }
        self.ensure_area(note, &area.inflate(radius))?;
        let canvas = self.ws.canvas(note)?;
        let idx = self.hit.entry(note).or_default();
        idx.sync(canvas, self.logs.get(&note));
        Ok(idx.query_path(path, radius))
    }

    /// The change journal of `note` (`None` until something was recorded or the note opened).
    pub fn change_log(&self, note: NoteId) -> Option<&vibenote_model::ChangeLog> {
        self.logs.get(&note)
    }

    fn log_of(&mut self, note: NoteId) -> &mut vibenote_model::ChangeLog {
        let epoch = &mut self.next_epoch;
        self.logs.entry(note).or_insert_with(|| {
            *epoch = epoch.saturating_add(1);
            vibenote_model::ChangeLog::new(*epoch)
        })
    }

    /// Journals applied ops: object changes per note; a created note starts a new epoch.
    fn log_ops(&mut self, ops: &[Op]) {
        for op in ops {
            self.search_follow(op);
        }
        let mut removed: BTreeMap<NoteId, Vec<vibenote_model::ObjectId>> = BTreeMap::new();
        for op in ops {
            if let Op::RemoveObject { note, object } = op
                && matches!(object.kind, vibenote_model::ObjectKind::Stroke(_))
            {
                removed.entry(*note).or_default().push(object.id);
            }
        }
        for (note, ids) in removed {
            self.ink_strokes_removed(note, &ids);
        }
        if !self.cleared_strokes.is_empty() {
            for op in ops {
                if let Op::InsertObject { note, object } = op
                    && self.cleared_strokes.remove(&object.id).is_some()
                {
                    self.sweep_notes.insert(*note);
                }
            }
        }
        for op in ops {
            match op {
                Op::InsertObject { note, object } | Op::RemoveObject { note, object } => {
                    self.log_of(*note).record(object.id);
                }
                Op::ReplaceObject { note, after } => self.log_of(*note).record(after.id),
                Op::CreateNote { note, .. } | Op::DeleteNote { note } => {
                    self.notes_rev = self.notes_rev.wrapping_add(1);
                    self.logs.remove(&note.id);
                    if matches!(op, Op::CreateNote { .. }) {
                        self.log_of(note.id);
                    }
                }
                Op::UpdateNote { .. }
                | Op::TrashNote { .. }
                | Op::RestoreNote { .. }
                | Op::CreateFolder { .. }
                | Op::DeleteFolder { .. }
                | Op::UpdateFolder { .. }
                | Op::TrashFolder { .. }
                | Op::RestoreFolder { .. } => self.notes_rev = self.notes_rev.wrapping_add(1),
                Op::SetNoteBackground { .. } => {}
                Op::SetPrettify { .. } => self.prettify_rev = self.prettify_rev.wrapping_add(1),
                Op::SetAudioMark { .. } => {}
            }
        }
    }

    /// Adds stored objects that are neither in memory nor removed in memory since the note
    /// was opened. Exact because every object that has been in memory is newer there than on
    /// disk: present ones are kept, removed ones are remembered in `NoteLoad::removed`.
    fn merge(&mut self, note: NoteId, objects: Vec<CanvasObject>) {
        let removed = self.loads.get(&note).map(|l| &l.removed);
        let mut added = Vec::new();
        if let Some(c) = self.ws.canvases.get_mut(&note) {
            for o in objects {
                if removed.is_some_and(|r| r.contains(&o.id)) {
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(v) = c.objects.entry(o.id) {
                    added.push(o.id);
                    v.insert(o);
                }
            }
        }
        if !added.is_empty() {
            let log = self.log_of(note);
            for id in added {
                log.record(id);
            }
        }
    }

    /// Incomplete notes that must be read completely before `ops` apply: removing or replacing
    /// an object that is not in memory (it may be on disk), and deleting a note (its undo keeps
    /// the whole canvas). Removing an object that is in memory needs no read.
    fn needs_complete(&self, ops: &[Op]) -> BTreeSet<NoteId> {
        if self.loads.is_empty() {
            return BTreeSet::new();
        }
        ops.iter()
            .filter_map(|op| {
                let (note, id) = match op {
                    Op::DeleteNote { note } => return Some(note.id),
                    Op::RemoveObject { note, object } => (*note, object.id),
                    Op::ReplaceObject { note, after } => (*note, after.id),
                    _ => return None,
                };
                let present = self.ws.canvases.get(&note).is_some_and(|c| c.objects.contains_key(&id));
                (!present).then_some(note)
            })
            .filter(|n| self.loads.contains_key(n))
            .collect()
    }

    /// Remembers objects removed from incomplete notes (see [`Session::merge`]).
    fn note_removals(&mut self, ops: &[Op]) {
        if self.loads.is_empty() {
            return;
        }
        for op in ops {
            if let Op::RemoveObject { note, object } = op
                && let Some(l) = self.loads.get_mut(note)
            {
                l.removed.insert(object.id);
            }
        }
    }

    /// First viewport of `note` (R-PERF-004): reads the objects in `area` now and queues a
    /// background load of the rest. No-op unless the note is unloaded.
    pub(crate) fn load_view(&mut self, note: NoteId, area: &Rect) -> Result<(), EngineError> {
        if self.load_state(note) != LoadState::Unloaded {
            return Ok(());
        }
        let read = self
            .reader
            .with(|st| Ok((st.top_z(note, None, TOP_Z)?, st.load_area(note, area)?)));
        let (top, objects) = match read {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(EngineError::Storage(format!("loading note {note}: {e}"))),
            None => return Ok(()),
        };
        self.log_of(note);
        self.merge(note, objects);
        self.generation = self.generation.saturating_add(1);
        self.loads.insert(
            note,
            NoteLoad {
                state: LoadState::Partial,
                generation: self.generation,
                top: Some(top),
                removed: self.loads.get(&note).map(|l| l.removed.clone()).unwrap_or_default(),
            },
        );
        self.load_requests.push((note, self.generation));
        Ok(())
    }

    /// Merges a streamed page; stale pages (completed or superseded loads) are dropped.
    fn merge_page(&mut self, page: load::Page) {
        let current = self
            .loads
            .get(&page.note)
            .is_some_and(|l| l.state == LoadState::Partial && l.generation == page.generation);
        if !current {
            return;
        }
        self.merge(page.note, page.objects);
        if let Some(e) = page.error {
            self.load_failed(page.note, &e);
        } else if page.done {
            self.loads.remove(&page.note);
        }
    }

    /// A background load stopped: the note goes back to unloaded (the next `note.open` retries;
    /// commands that need it complete read it synchronously).
    fn load_failed(&mut self, note: NoteId, e: &str) {
        if let Some(l) = self.loads.get_mut(&note) {
            l.state = LoadState::Unloaded;
        }
        self.warn(format!("loading note {note}: {e}"));
    }

    /// Reads the rest of `note` so its canvas is complete (blocking). Removing or replacing
    /// objects and deleting a note do this first, so they never act on a partial canvas.
    pub(crate) fn ensure_complete(&mut self, note: NoteId) -> Result<(), EngineError> {
        if !self.loads.contains_key(&note) {
            return Ok(());
        }
        let mut after = None;
        loop {
            let page = self
                .reader
                .with(|st| st.load_page(note, after, COMPLETE_PAGE))
                .unwrap_or_else(|| Ok(Vec::new()))
                .map_err(|e| EngineError::Storage(format!("loading note {note}: {e}")))?;
            let n = page.len();
            after = page.last().map(|o| o.id).or(after);
            self.merge(note, page);
            if n < usize::try_from(COMPLETE_PAGE).unwrap_or(usize::MAX) {
                break;
            }
        }
        self.loads.remove(&note);
        Ok(())
    }

    /// Makes sure every stored object of `note` whose ink can reach `area` is in memory. On a
    /// note still loading only that area is read (erasers and lassos on pen-up must not wait
    /// for the whole note, review M4 M2); an unloaded note is read completely.
    pub(crate) fn ensure_area(&mut self, note: NoteId, area: &Rect) -> Result<(), EngineError> {
        match self.loads.get(&note).map(|l| l.state) {
            None => Ok(()),
            Some(LoadState::Partial) => {
                // Stored boxes are the samples' box grown by one width; ink reaches up to 1.25
                // widths (tilted highlighter) and widths stay below 10,000 (model bound).
                let area = area.inflate(STORED_BOX_SLACK);
                let objects = self
                    .reader
                    .with(|st| st.load_area(note, &area))
                    .unwrap_or_else(|| Ok(Vec::new()))
                    .map_err(|e| EngineError::Storage(format!("loading note {note}: {e}")))?;
                self.merge(note, objects);
                Ok(())
            }
            Some(_) => self.ensure_complete(note),
        }
    }

    fn complete_for(&mut self, ops: &[Op]) -> Result<(), EngineError> {
        for n in self.needs_complete(ops) {
            self.ensure_complete(n)?;
        }
        Ok(())
    }

    /// The stored form (JSON) of a named session value (`vibenote_storage::META_KEYS`).
    fn meta_value(&self, key: &str) -> Option<String> {
        match key {
            "recent" => serde_json::to_string(&self.recent.iter().map(|n| n.0).collect::<Vec<_>>()).ok(),
            "keymap" => serde_json::to_string(&self.keymap_overrides).ok(),
            _ => None,
        }
    }

    /// A note's background: its own, or the workspace default (R-CANVAS-006).
    pub fn background_of(&self, note: NoteId) -> vibenote_model::settings::Background {
        self.ws
            .notes
            .get(&note)
            .and_then(|n| n.background)
            .unwrap_or(self.settings.background)
    }

    /// z of a new topmost object in `note`: above every object in memory and every stored
    /// object not in memory yet. Exact whatever has been loaded: an object that is (or was) in
    /// memory counts only by its memory state, so removed objects never count; this keeps
    /// command results independent of loading.
    pub(crate) fn next_z(&mut self, note: NoteId) -> Result<i64, EngineError> {
        let mem = self.ws.canvas(note)?.objects.values().map(|o| o.z).max();
        let stored = if self.loads.contains_key(&note) {
            self.unseen_top_z(note, mem)?
        } else {
            None
        };
        Ok(mem.max(stored).map_or(0, |m| m.saturating_add(1)))
    }

    /// Highest z among stored objects of an incomplete note that are not in memory and were not
    /// removed in memory, or `None` if none can exceed `mem` (the in-memory maximum).
    fn unseen_top_z(&mut self, note: NoteId, mem: Option<i64>) -> Result<Option<i64>, EngineError> {
        let read = |s: &Self, below| match s.reader.with(|st| st.top_z(note, below, TOP_Z)) {
            Some(r) => r.map_err(|e| EngineError::Storage(format!("reading note {note}: {e}"))),
            None => Ok(Vec::new()),
        };
        let mut page = match self.loads.get(&note).and_then(|l| l.top.clone()) {
            Some(t) => t,
            None => {
                let t = read(self, None)?;
                if let Some(l) = self.loads.get_mut(&note) {
                    l.top = Some(t.clone());
                }
                t
            }
        };
        let full = usize::try_from(TOP_Z).unwrap_or(usize::MAX);
        loop {
            let canvas = self.ws.canvas(note)?;
            let removed = self.loads.get(&note).map(|l| &l.removed);
            let unseen = |id: &vibenote_model::ObjectId| {
                !canvas.objects.contains_key(id) && !removed.is_some_and(|r| r.contains(id))
            };
            if let Some((z, _)) = page.iter().find(|(_, id)| unseen(id)) {
                return Ok(Some(*z));
            }
            let Some(&last) = page.last() else {
                return Ok(None);
            };
            // Everything unseen is at or below `last`; if memory already reaches that high
            // (the usual case: the visible, most recent strokes are loaded) it cannot matter.
            if page.len() < full || mem.is_some_and(|m| m >= last.0) {
                return Ok(None);
            }
            page = read(self, Some(last))?;
        }
    }

    /// The undo/redo stacks of a scope key ("workspace", "note:<id>"), loading them first.
    pub fn history_of(&mut self, key: &str) -> History {
        self.ensure_loaded(key);
        self.history.get(key).cloned().unwrap_or_default()
    }

    fn log(&mut self, h: vibenote_storage::HistoryOp) {
        if self.persistent() {
            self.pending_history.push(h);
        }
    }

    /// Applies `ops` atomically and records one undo step in `scope`. Clears that scope's redo.
    /// Commits a change to text box `id` while typing: consecutive changes to the same box
    /// are one undo step (the first one's inverse restores the text before typing).
    pub(crate) fn commit_typing(&mut self, note: NoteId, id: u64, ops: Vec<Op>) -> Result<(), EngineError> {
        let scope = Scope::Note(note);
        let key = scope.key();
        let top = self.history.get(&key).and_then(|h| h.undo.last()).is_some();
        if self.coalesce == Some((note, id)) && top {
            self.complete_for(&ops)?;
            let forward = ops.clone();
            self.ws.apply_all(ops)?;
            self.note_removals(&forward);
            self.log_ops(&forward);
            self.pending.extend(forward);
            return Ok(());
        }
        self.commit(scope, "typing", ops)?;
        self.coalesce = Some((note, id));
        Ok(())
    }

    pub fn commit(&mut self, scope: Scope, label: &str, ops: Vec<Op>) -> Result<(), EngineError> {
        use vibenote_storage::{HistoryOp, Stack};
        self.coalesce = None;
        let key = scope.key();
        self.ensure_loaded(&key);
        self.complete_for(&ops)?;
        let forward = ops.clone();
        let inverse = self.ws.apply_all(ops)?;
        self.note_removals(&forward);
        self.log_ops(&forward);
        self.pending.extend(forward);
        let step = Step {
            label: label.to_string(),
            inverse,
        };
        let persistent = self.persistent();
        let h = self.history.entry(key.clone()).or_default();
        let had_redo = !h.redo.is_empty();
        h.redo.clear();
        let rec = persistent.then(|| record(&step));
        h.undo.push(step);
        let trimmed = h.undo.len() > UNDO_LIMIT;
        if trimmed {
            h.undo.remove(0);
        }
        if had_redo {
            self.log(HistoryOp::Clear {
                scope: key.clone(),
                stack: Stack::Redo,
            });
        }
        if let Some(step) = rec {
            self.log(HistoryOp::Push {
                scope: key.clone(),
                stack: Stack::Undo,
                step,
            });
        }
        if trimmed {
            self.log(HistoryOp::DropOldest {
                scope: key,
                stack: Stack::Undo,
            });
        }
        Ok(())
    }

    /// Applies `ops` without an undo step (permanent deletion from the trash). Their undo
    /// history goes too: the workspace history (its steps may refer to what is deleted) and the
    /// histories of `deleted` notes. No canvas is read: nothing needs to be restorable.
    pub(crate) fn apply_permanent(&mut self, ops: Vec<Op>, deleted: &[NoteId]) -> Result<(), EngineError> {
        use vibenote_storage::{HistoryOp, Stack};
        let forward = ops.clone();
        self.ws.apply_all(ops)?;
        self.log_ops(&forward);
        self.pending.extend(forward);
        let keys = std::iter::once(Scope::Workspace)
            .chain(deleted.iter().map(|n| Scope::Note(*n)))
            .map(|sc| sc.key());
        for key in keys {
            // Marked loaded so the stale rows (cleared in the same batch) are never read.
            self.loaded.insert(key.clone());
            self.history.remove(&key);
            for stack in [Stack::Undo, Stack::Redo] {
                self.log(HistoryOp::Clear {
                    scope: key.clone(),
                    stack,
                });
            }
        }
        for n in deleted {
            self.loads.remove(n);
            if self.current_note == Some(*n) {
                self.current_note = None;
            }
        }
        Ok(())
    }

    pub fn undo(&mut self, scope: Scope) -> Result<String, EngineError> {
        self.step_back(scope, true)
    }

    pub fn redo(&mut self, scope: Scope) -> Result<String, EngineError> {
        self.step_back(scope, false)
    }

    /// Undo (`undo = true`) or redo: pops a step from one stack, applies it, and pushes its
    /// inverse onto the other.
    fn step_back(&mut self, scope: Scope, undo: bool) -> Result<String, EngineError> {
        use vibenote_storage::{HistoryOp, Stack};
        self.coalesce = None;
        let (from, to) = if undo {
            (Stack::Undo, Stack::Redo)
        } else {
            (Stack::Redo, Stack::Undo)
        };
        let key = scope.key();
        self.ensure_loaded(&key);
        let notes = self
            .history
            .get(&key)
            .and_then(|h| if undo { h.undo.last() } else { h.redo.last() })
            .map(|s| self.needs_complete(&s.inverse))
            .unwrap_or_default();
        for n in notes {
            self.ensure_complete(n)?;
        }
        let h = self.history.entry(key.clone()).or_default();
        let stack = if undo { &mut h.undo } else { &mut h.redo };
        let step = stack
            .pop()
            .ok_or(EngineError::NothingTo(if undo { "undo" } else { "redo" }))?;
        match self.ws.apply_all(step.inverse.clone()) {
            Ok(back) => {
                self.note_removals(&step.inverse);
                self.log_ops(&step.inverse);
                self.pending.extend(step.inverse.iter().cloned());
                let label = step.label.clone();
                let other = Step {
                    label: step.label,
                    inverse: back,
                };
                let rec = self.persistent().then(|| record(&other));
                let h = self.history.entry(key.clone()).or_default();
                if undo {
                    h.redo.push(other);
                } else {
                    h.undo.push(other);
                }
                self.log(HistoryOp::Pop {
                    scope: key.clone(),
                    stack: from,
                });
                if let Some(step) = rec {
                    self.log(HistoryOp::Push {
                        scope: key,
                        stack: to,
                        step,
                    });
                }
                Ok(label)
            }
            Err(e) => {
                // State unchanged (apply_all rolls back); keep the step so nothing is lost.
                let h = self.history.entry(key).or_default();
                if undo {
                    h.undo.push(step);
                } else {
                    h.redo.push(step);
                }
                Err(e.into())
            }
        }
    }
}

/// Where uncommitted changes go when the disk fails at shutdown: next to the workspace, then
/// in `$XDG_RUNTIME_DIR` (RAM-backed: survives the app, not a reboot).
fn recovery_paths(db: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    let mut side = db.as_os_str().to_owned();
    side.push(".recovery");
    v.push(std::path::PathBuf::from(side));
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        let key = std::fs::canonicalize(db).unwrap_or_else(|_| db.to_path_buf());
        let h = key
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        v.push(std::path::PathBuf::from(run).join(format!("vibenote-recovery-{h:016x}.bin")));
    }
    v
}

/// Applies recovery files left by a failed shutdown and deletes them. Returns a message per
/// file and the id counter it carries. A file that cannot be applied is kept for the owner.
fn recover(store: &mut vibenote_storage::Store, paths: &[std::path::PathBuf]) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for p in paths {
        match vibenote_storage::read_recovery(p) {
            Ok(None) => {}
            Ok(Some(r)) => {
                let n = r.ops.len();
                match store.apply_batch(&r.ops, &r.hist, r.next_id, r.settings.as_ref()) {
                    Ok(()) => {
                        let gone = std::fs::remove_file(p).is_ok();
                        out.push((
                            format!(
                                "recovered {n} unsaved changes from {}{}",
                                p.display(),
                                if gone { "" } else { " (file could not be removed)" }
                            ),
                            r.next_id,
                        ));
                    }
                    Err(e) => out.push((
                        format!("recovery file {} not applied (kept): {e}", p.display()),
                        r.next_id,
                    )),
                }
            }
            Err(e) => out.push((format!("recovery file {} unreadable (kept): {e}", p.display()), 0)),
        }
    }
    out
}

/// Session + registry. The single entry point for UI, CLI and the control channel.
pub struct Engine {
    pub session: Session,
    pub registry: Registry,
    store: Option<persist::Persister>,
    loader: Option<load::Loader>,
}

impl Drop for Engine {
    /// The block being written is closed and its strokes recorded, so it is recognised on the
    /// next start (its job would be lost with this process).
    fn drop(&mut self) {
        if self.session.open_block.is_some() {
            self.session.block_close();
            let _ = self.flush();
        }
    }
}

impl Engine {
    /// In-memory engine (tests, scratch).
    pub fn new() -> Result<Self, EngineError> {
        let mut registry = Registry::default();
        commands::register_all(&mut registry)?;
        let mut session = Session::new();
        session.command_ids = registry.ids().collect();
        Ok(Engine {
            session,
            registry,
            store: None,
            loader: None,
        })
    }

    /// Opens (or creates) a workspace database. Reads folders and notes only; a note's canvas
    /// loads when it opens (viewport first, then the rest in the background, R-PERF-004) or when
    /// a command needs all of it. [`Engine::load_all`] reads everything.
    ///
    /// Changes are written by a storage thread (ADR 0005, R-PERF-009): `execute` never waits for
    /// the disk; [`Engine::sync`] does.
    pub fn open(path: &std::path::Path) -> Result<Self, EngineError> {
        let storage = |x: vibenote_storage::StorageError| EngineError::Storage(x.to_string());
        let mut e = Engine::new()?;
        let mut store = vibenote_storage::Store::open(path).map_err(storage)?;
        let recovered = recover(&mut store, &recovery_paths(path));
        e.session.ws = store.load_meta().map_err(storage)?;
        for (msg, next_id) in recovered {
            e.session.warn(msg);
            // Ids in an unapplied recovery file stay reserved.
            e.session.ws.next_id = e.session.ws.next_id.max(next_id);
        }
        // Unreadable or out-of-range settings fall back to defaults (with a warning); they must
        // never keep the workspace from opening.
        match store.load_settings_lenient() {
            Ok(Some((st, reset))) => {
                e.session.settings = st;
                if !reset.is_empty() {
                    e.session.warn(format!(
                        "settings unreadable or out of range, reset to defaults: {}",
                        reset.join(", ")
                    ));
                }
            }
            Ok(None) => {}
            Err(x) => e.session.warn(format!("settings unreadable, using defaults: {x}")),
        }
        // Keymap overrides (R-CMD-002): invalid entries are dropped with a warning.
        if let Ok(Some(json)) = store.load_meta_value("keymap") {
            match serde_json::from_str::<std::collections::BTreeMap<String, Option<String>>>(&json) {
                Ok(m) => {
                    let dropped = commands::keymap::load_overrides(&mut e.session, m);
                    if !dropped.is_empty() {
                        e.session
                            .warn(format!("keymap: ignored invalid bindings {}", dropped.join(", ")));
                    }
                }
                Err(x) => e.session.warn(format!("keymap unreadable, using defaults: {x}")),
            }
        }
        // Recently opened notes (R-ORG-003): a convenience, so unreadable means none.
        if let Ok(Some(json)) = store.load_meta_value("recent")
            && let Ok(mut ids) = serde_json::from_str::<Vec<u64>>(&json)
        {
            ids.truncate(quickopen::RECENT);
            e.session.recent = ids
                .into_iter()
                .map(NoteId)
                .filter(|n| e.session.ws.notes.contains_key(n))
                .collect();
        }
        e.session.loads = e.session.ws.notes.keys().map(|n| (*n, NoteLoad::unloaded())).collect();
        let recovery = recovery_paths(path);
        e.store = Some(persist::Persister::spawn(store, recovery).map_err(|x| EngineError::Storage(x.to_string()))?);
        e.session.blobs = BlobHandle(std::sync::Arc::new(
            vibenote_storage::BlobStore::open(&vibenote_storage::BlobStore::dir_for(path)).map_err(storage)?,
        ));
        let reader = vibenote_storage::Store::open_reader(path).map_err(storage)?;
        e.session.reader = StoreReader(Some(std::sync::Arc::new(std::sync::Mutex::new(reader))));
        let loader = vibenote_storage::Store::open_reader(path).map_err(storage)?;
        e.loader = Some(load::Loader::spawn(loader).map_err(|x| EngineError::Storage(x.to_string()))?);
        // Recordings interrupted by a crash become audio objects.
        if let Err(x) = e.session.recover_recordings() {
            e.session.warn(format!("recordings could not be recovered: {x}"));
        }
        e.flush()?;
        Ok(e)
    }

    pub fn execute(&mut self, cmd: &str, params: Value) -> Result<Value, EngineError> {
        if cmd.starts_with("search.") || cmd == "finder.query" {
            let wait = params.get("wait").and_then(Value::as_bool).unwrap_or(true);
            self.prepare_search(wait);
        }
        let next_id = self.session.ws.next_id;
        let r = self.registry.execute(&mut self.session, cmd, params);
        if r.is_err() && self.session.pending.is_empty() {
            // A failed command changed nothing, so it uses up no ids either (otherwise a
            // reopened workspace would hand out different ids than this session).
            self.session.ws.next_id = next_id;
        }
        self.start_loads();
        self.flush()?;
        if std::mem::take(&mut self.session.gc_requested) && r.is_ok() {
            self.gc_blobs()?;
        }
        r
    }

    /// Lets time pass for handwriting: the open writing block closes (and is recognised in the
    /// background) once the pen has rested longer than the idle threshold. `now_us` is on the
    /// clock of the pen events (the app: monotonic microseconds).
    pub fn tick(&mut self, now_us: u64) {
        let idle = u64::from(self.session.settings.block_idle_ms).saturating_mul(1000);
        if self
            .session
            .open_block
            .as_ref()
            .is_some_and(|o| now_us.saturating_sub(o.last_end_us) > idle)
        {
            self.session.block_close();
        }
    }

    /// Lets time pass for the pen on the surface: a stroke held still for 500 ms turns into the
    /// shape it looks like (R-SHAPE-002). Call it while the pen is down; `now_us` as for
    /// [`Engine::tick`]. Returns true if the canvas changed.
    pub fn tick_pen(&mut self, now_us: u64) -> Result<bool, EngineError> {
        if self.session.live.stroke.is_none() {
            return Ok(false);
        }
        let mut errors = Vec::new();
        let snapped = crate::commands::input::hold(&mut self.session, now_us, &mut errors);
        let changed = snapped || self.session.live.snapped.is_some();
        if changed {
            self.flush()?;
        }
        match errors.into_iter().next() {
            Some(e) => Err(EngineError::Model(e)),
            None => Ok(changed),
        }
    }

    /// Makes search see everything applied so far (R-PDF-004 included): PDF text extracted in
    /// the background is recorded, text still missing is extracted now, and storage is synced
    /// (queries read the committed FTS5 index). Search is not on the pen path.
    /// Brings the search index up to date before a `search.*` command. With `wait` (the
    /// default) the block being written counts as finished, recognition and PDF text extraction
    /// complete first (bounded) and storage is synced, so results do not depend on timing. Without
    /// it (the UI, on the input thread) nothing blocks: finished results are taken and the rest
    /// is reported as pending. Failures never fail the search; they are warnings.
    fn prepare_search(&mut self, wait: bool) {
        let mut warnings = Vec::new();
        for (blob, pages) in self.session.pdf.take_texts() {
            if let Err(e) = self.session.set_pdf_text(blob, pages) {
                warnings.push(format!("PDF text not recorded: {e}"));
            }
        }
        if wait {
            self.session.block_close();
            self.run_sweeps();
            let t = std::time::Instant::now();
            while self.session.hwr.busy() && t.elapsed() < std::time::Duration::from_secs(30) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        if let Err(e) = self.session.take_recognitions() {
            warnings.push(format!("recognised text not recorded: {e}"));
        }
        if wait && let Err(e) = self.sync() {
            warnings.push(format!("search: storage not in sync: {e}"));
        }
        let missing: Vec<String> = match self.session.pdf_blobs_without_text() {
            Ok(m) => m
                .into_iter()
                .filter(|b| !self.session.pdf_text_failed.contains(b) && !self.session.pdf.text_failed(b))
                .collect(),
            Err(e) => {
                warnings.push(format!("search: {e}"));
                Vec::new()
            }
        };
        self.session.search_pending = !wait && (self.session.hwr.busy() || !missing.is_empty());
        if wait {
            for blob in missing {
                let doc = commands::pdf::doc_of(&self.session, &blob);
                match self.session.pdf.all_text(doc) {
                    Ok(pages) => {
                        if let Err(e) = self.session.set_pdf_text(blob, pages) {
                            warnings.push(format!("PDF text not recorded: {e}"));
                        }
                    }
                    Err(e) => {
                        warnings.push(format!("text of PDF {blob} could not be extracted: {e}"));
                        self.session.pdf_text_failed.insert(blob);
                    }
                }
            }
            if let Err(e) = self.sync() {
                warnings.push(format!("search: storage not in sync: {e}"));
            }
        } else {
            // Extracted in the background; the next search finds it.
            for blob in missing {
                let doc = commands::pdf::doc_of(&self.session, &blob);
                self.session.pdf.want_text(doc);
            }
        }
        for w in warnings {
            self.session.warn(w);
        }
    }

    /// Removes blobs nothing refers to: not a stored object or undo/redo step, and nothing in
    /// memory. Waits for storage to be in sync first, so no reference is missed.
    pub fn gc_blobs(&mut self) -> Result<usize, EngineError> {
        self.sync()?;
        let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(r) = self.session.reader.with(vibenote_storage::Store::blob_refs) {
            keep.extend(r.map_err(EngineError::Storage)?);
        }
        let s = &self.session;
        // Chunks of the recording in progress.
        if let Some(r) = &s.audio.rec {
            keep.extend(r.chunks.iter().cloned());
        }
        for c in s.ws.canvases.values() {
            keep.extend(
                c.objects
                    .values()
                    .flat_map(|o| o.kind.blobs().into_iter().map(String::from)),
            );
        }
        for h in s.history.values() {
            for step in h.undo.iter().chain(&h.redo) {
                for op in &step.inverse {
                    op.for_each_object(&mut |o| {
                        keep.extend(o.kind.blobs().into_iter().map(String::from));
                    });
                }
            }
        }
        s.blobs.0.gc(&keep).map_err(|e| EngineError::Storage(e.to_string()))
    }

    fn start_loads(&mut self) {
        for (note, generation) in std::mem::take(&mut self.session.load_requests) {
            let sent = match &self.loader {
                Some(l) => l.request(note, generation),
                None => Err("no loader".into()),
            };
            if let Err(e) = sent {
                self.session.load_failed(note, &e);
            }
        }
    }

    /// Merges up to `max_pages` pages of notes loading in the background; never waits. Not part
    /// of `execute`, so the pen path never pays for it: the UI pumps between frames. Commands do
    /// not depend on it (they complete a note themselves when they need all of it). Returns the
    /// number merged (the UI redraws when it is non-zero).
    pub fn pump(&mut self, max_pages: usize) -> usize {
        // PDF text extracted in the background: into the search index and storage.
        let texts = self.session.pdf.take_texts();
        let got = texts.len();
        for (blob, pages) in texts {
            // Derived data: if it cannot be recorded now, a search extracts it again.
            let _ = self.session.set_pdf_text(blob, pages);
        }
        // Recognised handwriting finished in the background.
        let recognised = self.session.take_recognitions().unwrap_or(0);
        self.run_sweeps();
        // Chunks a live recording stored meanwhile (journalled for crash recovery).
        if self.session.audio_pump() > 0 {
            let _ = self.flush();
        }
        if got > 0 || recognised > 0 {
            let _ = self.flush();
        }
        let mut n = 0;
        while n < max_pages {
            let Some(page) = self.loader.as_ref().and_then(load::Loader::try_page) else {
                break;
            };
            self.session.merge_page(page);
            n = n.saturating_add(1);
        }
        n
    }

    /// Re-recognition sweeps of opened notes that are complete now (see `Session::sweep_ink`).
    fn run_sweeps(&mut self) {
        let ready: Vec<NoteId> = self
            .session
            .sweep_notes
            .iter()
            .copied()
            .filter(|n| self.session.load_state(*n) == LoadState::Complete)
            .collect();
        for n in ready {
            self.session.sweep_notes.remove(&n);
            self.session.sweep_ink(n);
        }
    }

    /// True while a note is still loading in the background.
    pub fn loading(&self) -> bool {
        self.session.loads.values().any(|l| l.state == LoadState::Partial)
    }

    /// Waits until no note is loading in the background.
    pub fn wait_loaded(&mut self) {
        while self.loading() {
            match self.loader.as_ref().and_then(load::Loader::wait_page) {
                Some(page) => self.session.merge_page(page),
                None => {
                    for n in self.session.loading_notes() {
                        self.session.load_failed(n, "loader stopped");
                    }
                }
            }
        }
    }

    /// Reads every note completely (tools and tests that need the whole workspace in memory).
    pub fn load_all(&mut self) -> Result<(), EngineError> {
        let notes: Vec<NoteId> = self.session.loads.keys().copied().collect();
        for n in notes {
            self.session.ensure_complete(n)?;
        }
        Ok(())
    }

    /// Hands pending ops to the storage thread (drops them without a store). Never blocks.
    pub fn flush(&mut self) -> Result<(), EngineError> {
        let s = &mut self.session;
        if s.pending.is_empty()
            && s.pending_history.is_empty()
            && !s.settings_dirty
            && s.meta_dirty.is_empty()
            && s.derived_pending.is_empty()
        {
            return Ok(());
        }
        let derived = std::mem::take(&mut s.derived_pending);
        let ops = std::mem::take(&mut s.pending);
        let hist = std::mem::take(&mut s.pending_history);
        let settings = std::mem::take(&mut s.settings_dirty).then_some(s.settings);
        let meta = std::mem::take(&mut s.meta_dirty)
            .into_iter()
            .filter_map(|k| s.meta_value(k).map(|v| (k, v)))
            .collect();
        if let Some(st) = self.store.as_mut()
            && let Err(back) = st.submit(ops, hist, self.session.ws.next_id, settings, meta, derived)
        {
            // Keep the changes (in order) for a later flush; the command itself did apply.
            let s = &mut self.session;
            s.pending = back.ops;
            s.pending_history = back.hist;
            s.settings_dirty = back.settings.is_some();
            s.meta_dirty = back.meta.into_iter().map(|(k, _)| k).collect();
            s.derived_pending = back.derived;
            return Err(EngineError::Storage(
                "storage thread stopped; changes are kept in memory, not saved".into(),
            ));
        }
        Ok(())
    }

    /// Waits until every change made so far is durable (tests, CLI, app exit). Returns the
    /// storage error if a commit failed; the changes stay queued and are retried.
    pub fn sync(&mut self) -> Result<(), EngineError> {
        self.flush()?;
        match &self.store {
            Some(st) => st.sync().map_err(EngineError::Storage),
            None => Ok(()),
        }
    }

    /// The command bound to a key in canonical form (R-CMD-002), e.g. "Ctrl+Shift+Z".
    pub fn key_binding(&self, key: &str) -> Option<String> {
        commands::keymap::bindings(&self.session).remove(key)
    }

    /// Storage progress and the last commit error (`/storage` in [`Engine::inspect`]).
    pub fn storage_status(&self) -> Option<StorageStatus> {
        self.store.as_ref().map(persist::Persister::status)
    }

    /// Read-only view of the state as JSON, optionally narrowed by a JSON pointer
    /// (`state.inspect`; agents verify work here first, screenshots second).
    pub fn inspect(&self, pointer: Option<&str>) -> Result<Value, EngineError> {
        let mut v = serde_json::to_value(&self.session).map_err(|e| EngineError::Internal(e.to_string()))?;
        if let Some(obj) = v.as_object_mut() {
            let st = serde_json::to_value(self.storage_status()).map_err(|e| EngineError::Internal(e.to_string()))?;
            obj.insert("storage".into(), st);
            // The selected tool (R-PEN-005), also at `/settings/tool`.
            let tool =
                serde_json::to_value(self.session.settings.tool).map_err(|e| EngineError::Internal(e.to_string()))?;
            obj.insert("tool".into(), tool);
            // The current note's background (R-CANVAS-006): its own or the workspace default.
            let bg = self
                .session
                .current_note
                .map_or(self.session.settings.background, |n| self.session.background_of(n));
            let bg = serde_json::to_value(bg).map_err(|e| EngineError::Internal(e.to_string()))?;
            obj.insert("background".into(), bg);
            let unloaded = self
                .session
                .loads
                .values()
                .filter(|l| l.state == LoadState::Unloaded)
                .count();
            obj.insert(
                "loading".into(),
                serde_json::json!({"partial": self.session.loading_notes(), "unloaded": unloaded}),
            );
        }
        match pointer {
            None | Some("") => Ok(v),
            Some(p) => v
                .pointer(p)
                .cloned()
                .ok_or_else(|| EngineError::Model(format!("no value at {p}"))),
        }
    }
}
