//! Invertible model operations.

use crate::{
    Canvas, CanvasObject, Folder, FolderId, ModelError, Note, NoteId, ObjectKind, TextStyle, Workspace, limits,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    CreateFolder {
        folder: Folder,
    },
    DeleteFolder {
        folder: Folder,
    },
    UpdateFolder {
        after: Folder,
    },
    /// Creates a note with the given canvas (empty for a new note; populated when undoing a delete).
    CreateNote {
        note: Note,
        canvas: Canvas,
    },
    DeleteNote {
        note: Note,
    },
    UpdateNote {
        after: Note,
    },
    InsertObject {
        note: NoteId,
        object: CanvasObject,
    },
    RemoveObject {
        note: NoteId,
        object: CanvasObject,
    },
    ReplaceObject {
        note: NoteId,
        after: CanvasObject,
    },
    /// Moves a note to the trash (R-ORG-004); its canvas stays.
    TrashNote {
        note: NoteId,
    },
    RestoreNote {
        note: NoteId,
    },
    /// Moves a folder to the trash; only folders whose notes and subfolders are all trashed.
    TrashFolder {
        folder: FolderId,
    },
    RestoreFolder {
        folder: FolderId,
    },
    /// Sets a note's background (R-CANVAS-006; `None` = the workspace default). The only op that
    /// changes it: `UpdateNote` keeps the current background, so undoing a rename never
    /// reverts a background change and vice versa.
    SetNoteBackground {
        note: NoteId,
        background: Option<crate::settings::Background>,
    },
    /// Sets (or with `None` removes) a block's prettify parameters (ADR 0008).
    SetPrettify {
        note: NoteId,
        block: crate::ObjectId,
        after: Option<crate::Prettify>,
    },
    /// Sets (or with `None` removes) when a stroke was written during a recording (R-AUDIO-002).
    SetAudioMark {
        note: NoteId,
        stroke: crate::ObjectId,
        after: Option<crate::AudioMark>,
    },
}

impl Op {
    /// Calls `f` on every canvas object the op carries (blob reference scans).
    pub fn for_each_object(&self, f: &mut dyn FnMut(&CanvasObject)) {
        match self {
            Op::InsertObject { object, .. } | Op::RemoveObject { object, .. } => f(object),
            Op::ReplaceObject { after, .. } => f(after),
            Op::CreateNote { canvas, .. } => canvas.objects.values().for_each(f),
            _ => {}
        }
    }
}

fn check_name(s: &str) -> Result<(), ModelError> {
    if s.chars().count() > limits::MAX_NAME_LEN {
        return Err(ModelError::Invalid(format!(
            "name longer than {} characters",
            limits::MAX_NAME_LEN
        )));
    }
    Ok(())
}

fn check_object(o: &CanvasObject) -> Result<(), ModelError> {
    if let ObjectKind::Stroke(s) = &o.kind {
        if s.samples.is_empty() || s.samples.len() > limits::MAX_SAMPLES_PER_STROKE {
            return Err(ModelError::Invalid(format!(
                "stroke must have 1..={} samples",
                limits::MAX_SAMPLES_PER_STROKE
            )));
        }
        if !(s.width.is_finite() && s.width > 0.0 && s.width < 10_000.0) {
            return Err(ModelError::Invalid("stroke width out of range".into()));
        }
        for p in &s.samples {
            let ok = p.x.is_finite()
                && p.y.is_finite()
                && p.x.abs() <= limits::MAX_COORD
                && p.y.abs() <= limits::MAX_COORD
                && (0.0..=1.0).contains(&p.pressure)
                && (-90.0..=90.0).contains(&p.tilt_x)
                && (-90.0..=90.0).contains(&p.tilt_y);
            if !ok {
                return Err(ModelError::Invalid("stroke sample out of range".into()));
            }
        }
    }
    if let ObjectKind::Text {
        origin,
        width,
        content,
        marks,
        size,
    } = &o.kind
    {
        let bad = |m: &str| Err(ModelError::Invalid(format!("text: {m}")));
        let within = |v: f64| v.is_finite() && v.abs() <= limits::MAX_COORD;
        if !(within(origin.x) && within(origin.y) && width.is_finite() && *width > 0.0 && *width <= 1.0e6) {
            return bad("position or width out of range");
        }
        if !(size.is_finite() && (1.0..=10_000.0).contains(size)) {
            return bad("size out of range");
        }
        let chars = content.chars().count();
        if chars > limits::MAX_TEXT_CHARS || marks.len() > limits::MAX_TEXT_MARKS {
            return bad("too long");
        }
        let n = u32::try_from(chars).unwrap_or(u32::MAX);
        for m in marks {
            if m.start >= m.end || m.end > n {
                return bad("mark outside the text");
            }
            match m.style {
                TextStyle::Size { size } if !(size.is_finite() && (1.0..=10_000.0).contains(&size)) => {
                    return bad("mark size out of range");
                }
                _ => {}
            }
        }
    }
    let within = |v: f64| v.is_finite() && v.abs() <= limits::MAX_COORD;
    let hash_ok = |h: &str| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    match &o.kind {
        ObjectKind::Shape { points, width, .. } => {
            if points.len() < 2 || points.len() > limits::MAX_SHAPE_POINTS {
                return Err(ModelError::Invalid("shape: 2.. points".into()));
            }
            if !points.iter().all(|p| within(p.x) && within(p.y))
                || !(width.is_finite() && *width > 0.0 && *width < 10_000.0)
            {
                return Err(ModelError::Invalid("shape: out of range".into()));
            }
        }
        ObjectKind::Image {
            origin,
            width,
            height,
            blob,
            pixels,
        } => {
            let size_ok = |v: f64| v.is_finite() && v > 0.0 && v <= 1.0e7;
            if !(within(origin.x) && within(origin.y) && size_ok(*width) && size_ok(*height))
                || !hash_ok(blob)
                || !pixels.iter().all(|p| (1..=limits::MAX_IMAGE_SIDE).contains(p))
            {
                return Err(ModelError::Invalid("image: out of range".into()));
            }
        }
        ObjectKind::Audio {
            origin,
            blobs,
            duration_ms,
            ..
        } => {
            if !(within(origin.x) && within(origin.y))
                || blobs.is_empty()
                || blobs.len() > limits::MAX_AUDIO_BLOBS
                || !blobs.iter().all(|b| hash_ok(b))
                || *duration_ms > limits::MAX_AUDIO_MS
            {
                return Err(ModelError::Invalid("audio: out of range".into()));
            }
        }
        ObjectKind::Link { origin, target } => {
            if !(within(origin.x) && within(origin.y)) {
                return Err(ModelError::Invalid("link: out of range".into()));
            }
            match target {
                crate::LinkTarget::Url { url } if !crate::url_ok(url) => {
                    return Err(ModelError::Invalid("link: only http, https and mailto URLs".into()));
                }
                crate::LinkTarget::Note { view: Some(v), .. }
                    if !(within(v.x) && within(v.y) && v.zoom.is_finite() && v.zoom > 0.0) =>
                {
                    return Err(ModelError::Invalid("link: view out of range".into()));
                }
                _ => {}
            }
        }
        _ => {}
    }
    if let ObjectKind::Pdf {
        origin,
        scale,
        gap,
        blob,
        pages,
    } = &o.kind
    {
        let bad = |m: &str| Err(ModelError::Invalid(format!("pdf: {m}")));
        if pages.is_empty() || pages.len() > limits::MAX_PDF_PAGES {
            return bad("page count out of range");
        }
        if !(blob.len() == 64 && blob.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            return bad("not a blob hash");
        }
        if !(scale.is_finite() && (1e-3..=1e3).contains(scale) && gap.is_finite() && (0.0..=1e4).contains(gap)) {
            return bad("scale or gap out of range");
        }
        let size_ok = |v: f64| v.is_finite() && v > 0.0 && v <= limits::MAX_PDF_PAGE_PT;
        if !pages.iter().all(|p| size_ok(p.width) && size_ok(p.height)) {
            return bad("page size out of range");
        }
        let within = |v: f64| v.is_finite() && v.abs() <= limits::MAX_COORD;
        let extent = o.kind.bounds();
        if !(within(origin.x) && within(origin.y))
            || !extent.is_some_and(|r| within(r.min.x) && within(r.max.x) && within(r.min.y) && within(r.max.y))
        {
            return bad("position out of range");
        }
    }
    Ok(())
}

/// Snaps stroke samples to the storage grid (model invariant; see `Sample::quantised`).
fn quantise(mut o: CanvasObject) -> CanvasObject {
    if let ObjectKind::Stroke(s) = &mut o.kind {
        for smp in &mut s.samples {
            *smp = smp.quantised();
        }
    }
    o
}

fn check_canvas(c: &Canvas) -> Result<(), ModelError> {
    for (id, o) in &c.objects {
        if *id != o.id {
            return Err(ModelError::Invalid("canvas object id mismatch".into()));
        }
        check_object(o)?;
    }
    Ok(())
}

impl Workspace {
    /// `f` must exist and not be in the trash (nothing new goes into a trashed folder).
    fn live_folder(&self, f: FolderId) -> Result<(), ModelError> {
        self.folder(f)?;
        if self.folder_in_trash(f) {
            return Err(ModelError::Trashed(format!("folder {f}")));
        }
        Ok(())
    }

    /// Applies `op` and returns its inverse. On error the workspace is unchanged.
    pub fn apply(&mut self, op: Op) -> Result<Op, ModelError> {
        match op {
            Op::CreateFolder { folder } => {
                check_name(&folder.name)?;
                if self.folders.contains_key(&folder.id) {
                    return Err(ModelError::DuplicateId(folder.id.0));
                }
                if let Some(p) = folder.parent {
                    self.live_folder(p)?;
                }
                self.folders.insert(folder.id, folder.clone());
                Ok(Op::DeleteFolder { folder })
            }
            Op::DeleteFolder { folder } => {
                let cur = self.folder(folder.id)?.clone();
                let has_children = self.folders.values().any(|f| f.parent == Some(folder.id))
                    || self.notes.values().any(|n| n.folder == Some(folder.id));
                if has_children {
                    return Err(ModelError::FolderNotEmpty(folder.id));
                }
                self.folders.remove(&folder.id);
                self.trashed_folders.remove(&folder.id);
                Ok(Op::CreateFolder { folder: cur })
            }
            Op::UpdateFolder { after } => {
                check_name(&after.name)?;
                let cur = self.folder(after.id)?.clone();
                if let Some(p) = after.parent {
                    // Only live folders are kept out of the trash; a trashed one may move.
                    if after.parent != cur.parent && !self.trashed_folders.contains(&after.id) {
                        self.live_folder(p)?;
                    } else {
                        self.folder(p)?;
                    }
                    if self.is_ancestor_or_self(after.id, p) {
                        return Err(ModelError::FolderCycle(after.id, p));
                    }
                }
                self.folders.insert(after.id, after);
                Ok(Op::UpdateFolder { after: cur })
            }
            Op::CreateNote { note, canvas } => {
                check_name(&note.name)?;
                check_canvas(&canvas)?;
                let canvas = Canvas {
                    objects: canvas.objects.into_iter().map(|(k, o)| (k, quantise(o))).collect(),
                };
                if self.notes.contains_key(&note.id) {
                    return Err(ModelError::DuplicateId(note.id.0));
                }
                if let Some(f) = note.folder {
                    self.live_folder(f)?;
                }
                self.notes.insert(note.id, note.clone());
                self.canvases.insert(note.id, canvas);
                Ok(Op::DeleteNote { note })
            }
            Op::DeleteNote { note } => {
                let cur = self.note(note.id)?.clone();
                if self.trashed_notes.contains(&note.id) {
                    // Permanent delete from the trash (not undoable: the inverse would bring
                    // it back outside the trash).
                    self.trashed_notes.remove(&note.id);
                }
                let canvas = self.canvases.remove(&note.id).unwrap_or_default();
                self.notes.remove(&note.id);
                Ok(Op::CreateNote { note: cur, canvas })
            }
            Op::TrashNote { note } => {
                let n = self.note(note)?;
                if let Some(f) = n.folder
                    && self.folder_in_trash(f)
                {
                    return Err(ModelError::Trashed(format!("folder {f}")));
                }
                if !self.trashed_notes.insert(note) {
                    return Err(ModelError::Trashed(format!("note {note}")));
                }
                Ok(Op::RestoreNote { note })
            }
            Op::RestoreNote { note } => {
                let n = self.note(note)?;
                if let Some(f) = n.folder
                    && self.folder_in_trash(f)
                {
                    return Err(ModelError::Trashed(format!("folder {f}")));
                }
                if !self.trashed_notes.remove(&note) {
                    return Err(ModelError::NotTrashed(format!("note {note}")));
                }
                Ok(Op::TrashNote { note })
            }
            Op::TrashFolder { folder } => {
                let f = self.folder(folder)?;
                if let Some(p) = f.parent
                    && self.folder_in_trash(p)
                {
                    return Err(ModelError::Trashed(format!("folder {p}")));
                }
                let live_child = self
                    .folders
                    .values()
                    .any(|c| c.parent == Some(folder) && !self.trashed_folders.contains(&c.id))
                    || self
                        .notes
                        .values()
                        .any(|n| n.folder == Some(folder) && !self.trashed_notes.contains(&n.id));
                if live_child {
                    return Err(ModelError::FolderNotEmpty(folder));
                }
                if !self.trashed_folders.insert(folder) {
                    return Err(ModelError::Trashed(format!("folder {folder}")));
                }
                Ok(Op::RestoreFolder { folder })
            }
            Op::RestoreFolder { folder } => {
                let f = self.folder(folder)?;
                if let Some(p) = f.parent
                    && self.folder_in_trash(p)
                {
                    return Err(ModelError::Trashed(format!("folder {p}")));
                }
                if !self.trashed_folders.remove(&folder) {
                    return Err(ModelError::NotTrashed(format!("folder {folder}")));
                }
                Ok(Op::TrashFolder { folder })
            }
            Op::UpdateNote { after } => {
                check_name(&after.name)?;
                let cur = self.note(after.id)?.clone();
                if let Some(f) = after.folder {
                    if after.folder != cur.folder && !self.trashed_notes.contains(&after.id) {
                        self.live_folder(f)?;
                    } else {
                        self.folder(f)?;
                    }
                }
                let after = Note {
                    background: cur.background,
                    ..after
                };
                self.notes.insert(after.id, after);
                Ok(Op::UpdateNote { after: cur })
            }
            Op::SetAudioMark { note, stroke, after } => {
                if after.is_some_and(|m| m.offset_ms > limits::MAX_AUDIO_MS) {
                    return Err(ModelError::Invalid("audio mark: offset out of range".into()));
                }
                self.note(note)?;
                let map = self.audio_marks.entry(note).or_default();
                let before = match after {
                    Some(m) => map.insert(stroke, m),
                    None => map.remove(&stroke),
                };
                if map.is_empty() {
                    self.audio_marks.remove(&note);
                }
                Ok(Op::SetAudioMark {
                    note,
                    stroke,
                    after: before,
                })
            }
            Op::SetPrettify { note, block, after } => {
                if let Some(p) = &after {
                    let n = p.strokes.len();
                    if n == 0 || n != p.transforms.len() || n > limits::MAX_SHAPE_POINTS {
                        return Err(ModelError::Invalid("prettify: one transform per stroke".into()));
                    }
                    if !p
                        .transforms
                        .iter()
                        .flatten()
                        .all(|v| v.is_finite() && v.abs() <= limits::MAX_COORD)
                    {
                        return Err(ModelError::Invalid("prettify: transform out of range".into()));
                    }
                }
                self.note(note)?;
                let map = self.prettified.entry(note).or_default();
                let before = match after {
                    Some(p) => map.insert(block, p),
                    None => map.remove(&block),
                };
                if map.is_empty() {
                    self.prettified.remove(&note);
                }
                Ok(Op::SetPrettify {
                    note,
                    block,
                    after: before,
                })
            }
            Op::SetNoteBackground { note, background } => {
                if let Some(b) = &background {
                    b.validate()?;
                }
                let n = self.notes.get_mut(&note).ok_or(ModelError::NoteNotFound(note))?;
                let old = std::mem::replace(&mut n.background, background);
                Ok(Op::SetNoteBackground { note, background: old })
            }
            Op::InsertObject { note, object } => {
                check_object(&object)?;
                let object = quantise(object);
                let c = self.canvases.get_mut(&note).ok_or(ModelError::NoteNotFound(note))?;
                if c.objects.contains_key(&object.id) {
                    return Err(ModelError::DuplicateId(object.id.0));
                }
                c.objects.insert(object.id, object.clone());
                Ok(Op::RemoveObject { note, object })
            }
            Op::RemoveObject { note, object } => {
                let c = self.canvases.get_mut(&note).ok_or(ModelError::NoteNotFound(note))?;
                let cur = c
                    .objects
                    .remove(&object.id)
                    .ok_or(ModelError::ObjectNotFound(note, object.id))?;
                Ok(Op::InsertObject { note, object: cur })
            }
            Op::ReplaceObject { note, after } => {
                check_object(&after)?;
                let after = quantise(after);
                let c = self.canvases.get_mut(&note).ok_or(ModelError::NoteNotFound(note))?;
                let cur = c
                    .objects
                    .get(&after.id)
                    .cloned()
                    .ok_or(ModelError::ObjectNotFound(note, after.id))?;
                c.objects.insert(after.id, after);
                Ok(Op::ReplaceObject { note, after: cur })
            }
        }
    }

    /// Applies ops in order. On the first error, rolls back the applied prefix and returns the
    /// error (workspace unchanged). On success returns the inverse sequence, to apply in order to undo.
    pub fn apply_all(&mut self, ops: Vec<Op>) -> Result<Vec<Op>, ModelError> {
        let mut inv: Vec<Op> = Vec::with_capacity(ops.len());
        for op in ops {
            match self.apply(op) {
                Ok(i) => inv.push(i),
                Err(e) => {
                    for i in inv.into_iter().rev() {
                        // Inverses of just-applied ops are valid by construction; a failure here
                        // would be a model bug and is reported rather than ignored.
                        if let Err(re) = self.apply(i) {
                            return Err(ModelError::Invalid(format!("rollback failed after {e}: {re}")));
                        }
                    }
                    return Err(e);
                }
            }
        }
        inv.reverse();
        Ok(inv)
    }
}
