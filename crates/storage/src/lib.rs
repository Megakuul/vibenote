//! Workspace storage (ADR 0005): one SQLite database per workspace (WAL, `synchronous=FULL`),
//! strokes as compact lossless blobs, R-tree spatial index. Content-addressed blob store for
//! PDFs/images/audio arrives with those milestones.
//!
//! Durability contract: when [`Store::apply`] returns `Ok`, the change survives a process
//! crash (and, given an fsync-honouring disk, power loss). A failed apply leaves the database
//! unchanged.

pub mod blobs;
mod codec;
pub mod history;
pub mod recovery;
pub mod search;

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use vibenote_geom::Rect;
use vibenote_model::{
    Canvas, CanvasObject, Color, Folder, FolderId, Note, NoteId, ObjectId, ObjectKind, Op, Settings, Workspace,
};

pub use blobs::BlobStore;
pub use codec::{decode_stroke, encode_stroke};
pub use history::{HistoryOp, Stack, StepRecord, StoredHistory};
pub use recovery::{Recovery, read_recovery, write_recovery};

/// Schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 10;

/// Schema v1 (2026-10-12): tree, objects, R-tree.
const SCHEMA_V1: &str = "BEGIN;
     CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS folder(id INTEGER PRIMARY KEY, parent INTEGER, name TEXT NOT NULL, color INTEGER, sort INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS note(id INTEGER PRIMARY KEY, folder INTEGER, name TEXT NOT NULL, color INTEGER, sort INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS object(id INTEGER PRIMARY KEY, note INTEGER NOT NULL, z INTEGER NOT NULL, kind TEXT NOT NULL, data BLOB NOT NULL);
     CREATE INDEX IF NOT EXISTS object_note ON object(note);
     CREATE VIRTUAL TABLE IF NOT EXISTS object_rt USING rtree(id, minx, maxx, miny, maxy);
     INSERT OR IGNORE INTO meta VALUES('next_id', '1');
     PRAGMA user_version = 1;
     COMMIT;";

/// v1 → v2 (M2.2): persistent per-scope undo/redo log.
const MIGRATE_V2: &str = "BEGIN;
     CREATE TABLE IF NOT EXISTS history(scope TEXT NOT NULL, kind INTEGER NOT NULL, seq INTEGER NOT NULL,
         label TEXT NOT NULL, ops BLOB NOT NULL, PRIMARY KEY(scope, kind, seq)) WITHOUT ROWID;
     PRAGMA user_version = 2;
     COMMIT;";

/// v2 → v3 (M2.3, lazy open): the R-tree gains a note dimension so a viewport query touches only
/// the open note, every object gets an entry (objects without bounds cover the whole plane), and
/// `(note, z)` is indexed for the topmost z of an unloaded note.
const MIGRATE_V3: &str = "BEGIN;
     CREATE VIRTUAL TABLE object_rt3 USING rtree(id, minx, maxx, miny, maxy, nlo, nhi);
     INSERT INTO object_rt3 SELECT r.id, r.minx, r.maxx, r.miny, r.maxy, o.note, o.note
         FROM object_rt r JOIN object o ON o.id = r.id;
     INSERT INTO object_rt3 SELECT o.id, -3.0e38, 3.0e38, -3.0e38, 3.0e38, o.note, o.note
         FROM object o WHERE o.id NOT IN (SELECT id FROM object_rt);
     DROP TABLE object_rt;
     ALTER TABLE object_rt3 RENAME TO object_rt;
     CREATE INDEX IF NOT EXISTS object_note_z ON object(note, z);
     PRAGMA user_version = 3;
     COMMIT;";

/// v3 → v4 (M5.1): trash (R-ORG-004); kind 0 = note, 1 = folder.
const MIGRATE_V4: &str = "BEGIN;
     CREATE TABLE IF NOT EXISTS trash(kind INTEGER NOT NULL, id INTEGER NOT NULL, PRIMARY KEY(kind, id)) WITHOUT ROWID;
     PRAGMA user_version = 4;
     COMMIT;";

/// v4 → v5 (M4.8): per-note background (R-CANVAS-006), JSON; NULL = workspace default.
const MIGRATE_V5: &str = "BEGIN;
     ALTER TABLE note ADD COLUMN background TEXT;
     PRAGMA user_version = 5;
     COMMIT;";

/// v5 → v6 (M8): non-stroke objects are found without scanning ink (search seeding), and PDF
/// text extracted by the worker is kept per blob (derived data, R-PDF-004).
const MIGRATE_V6: &str = "BEGIN;
     CREATE INDEX IF NOT EXISTS object_kind ON object(kind) WHERE kind <> 'stroke';
     CREATE TABLE IF NOT EXISTS pdf_text(blob TEXT NOT NULL, page INTEGER NOT NULL, text TEXT NOT NULL,
         PRIMARY KEY(blob, page)) WITHOUT ROWID;
     PRAGMA user_version = 6;
     COMMIT;";

/// Data derived from content (recomputable, so not part of recovery files).
#[derive(Clone, Debug, PartialEq)]
pub enum Derived {
    /// Text of every page of a PDF blob (page index order).
    PdfText { blob: String, pages: Vec<String> },
    /// Recognised handwriting of a block (R-HWR-003, R-SEARCH-001), keyed by its first stroke.
    InkText {
        note: NoteId,
        block: ObjectId,
        region: Rect,
        text: String,
        tag: String,
    },
    /// A written block's strokes (its first stroke is its key).
    InkBlock {
        note: NoteId,
        block: ObjectId,
        members: Vec<ObjectId>,
    },
    /// None of a block's strokes is left: its text leaves the index and it counts as not read.
    InkClear { note: NoteId, block: ObjectId },
    /// A recording in progress (its audio object as it would be now, as JSON): after a crash it
    /// becomes an audio object with the chunks written so far.
    Recording { id: ObjectId, note: NoteId, data: String },
    /// The recording was stopped (its object is stored with the same batch) or discarded.
    RecordingDone { id: ObjectId },
}

/// v8 → v9 (M9.7): prettify parameters per block (ADR 0008); strokes are never changed.
const MIGRATE_V9: &str = "BEGIN;
     CREATE TABLE IF NOT EXISTS prettify(note INTEGER NOT NULL, block INTEGER NOT NULL, data TEXT NOT NULL,
         PRIMARY KEY(note, block)) WITHOUT ROWID;
     PRAGMA user_version = 9;
     COMMIT;";

/// A migration step's statements without its own `BEGIN;` / `COMMIT;`.
fn unwrap_tx(step: &str) -> &str {
    let s = step.trim();
    let s = s.strip_prefix("BEGIN;").unwrap_or(s);
    s.strip_suffix("COMMIT;").unwrap_or(s)
}

/// v9 → v10: written blocks (their strokes) and the pack version that last read each one, so
/// blocks are recognised again after an undo, an interrupted job or a pack upgrade; audio marks
/// (R-AUDIO-002) and recordings in progress (their chunks so far, for crash recovery).
const MIGRATE_V10: &str = "BEGIN;
     CREATE TABLE IF NOT EXISTS ink_block(note INTEGER NOT NULL, block INTEGER NOT NULL, members TEXT NOT NULL,
         tag TEXT, PRIMARY KEY(note, block)) WITHOUT ROWID;
     CREATE TABLE IF NOT EXISTS audio_mark(note INTEGER NOT NULL, stroke INTEGER NOT NULL, recording INTEGER NOT NULL,
         offset_ms INTEGER NOT NULL, PRIMARY KEY(note, stroke)) WITHOUT ROWID;
     CREATE TABLE IF NOT EXISTS recording(id INTEGER PRIMARY KEY, note INTEGER NOT NULL, data TEXT NOT NULL);
     PRAGMA user_version = 10;
     COMMIT;";

/// Most blocks read per note and strokes per block (hostile files).
pub const MAX_BLOCKS_PER_NOTE: usize = 100_000;
pub const MAX_BLOCK_STROKES: usize = 10_000;

/// Session values stored next to the settings: recently opened notes (R-ORG-003) and keymap
/// overrides (R-CMD-002).
pub const META_KEYS: &[&str] = &["recent", "keymap"];

/// R-tree box of objects without bounds (inside the f32 range the R-tree stores).
const WHOLE_PLANE: f64 = 3.0e38;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("corrupt data: {0}")]
    Corrupt(String),
    #[error("unsupported schema version {0} (this build supports up to {SCHEMA_VERSION})")]
    Version(i64),
}

pub struct Store {
    conn: Connection,
}

pub(crate) fn i(v: u64) -> Result<i64, StorageError> {
    i64::try_from(v).map_err(|_| StorageError::Corrupt(format!("id {v} out of range")))
}
pub(crate) fn u(v: i64) -> Result<u64, StorageError> {
    u64::try_from(v).map_err(|_| StorageError::Corrupt(format!("negative id {v}")))
}

impl Store {
    /// Opens (creating if needed) the workspace database at `path`.
    pub fn open(path: &Path) -> Result<Store, StorageError> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ver > SCHEMA_VERSION {
            return Err(StorageError::Version(ver));
        }
        if ver == 0 {
            // A new database: every step in one transaction (one fsync instead of one per step).
            let tx = conn.transaction()?;
            for step in [
                SCHEMA_V1,
                MIGRATE_V2,
                MIGRATE_V3,
                MIGRATE_V4,
                MIGRATE_V5,
                MIGRATE_V6,
                search::SCHEMA,
                search::MIGRATE_V8,
                MIGRATE_V9,
                MIGRATE_V10,
            ] {
                tx.execute_batch(unwrap_tx(step))?;
            }
            tx.commit()?;
            return Ok(Store { conn });
        }
        // Each step is its own transaction; a crash between steps leaves a valid older version
        // that the next open continues from.
        if ver < 1 {
            conn.execute_batch(SCHEMA_V1)?;
        }
        if ver < 2 {
            conn.execute_batch(MIGRATE_V2)?;
        }
        if ver < 3 {
            conn.execute_batch(MIGRATE_V3)?;
        }
        if ver < 4 {
            conn.execute_batch(MIGRATE_V4)?;
        }
        if ver < 5 {
            conn.execute_batch(MIGRATE_V5)?;
        }
        if ver < 6 {
            conn.execute_batch(MIGRATE_V6)?;
        }
        if ver < 7 {
            // v6 → v7 (M8, ADR 0005): the FTS5 search index, filled from the existing objects.
            let tx = conn.transaction()?;
            tx.execute_batch(search::SCHEMA)?;
            for (note, o) in search::non_stroke(&tx)? {
                search::index(&tx, note, &o)?;
            }
            tx.execute_batch("PRAGMA user_version = 7;")?;
            tx.commit()?;
        }
        if ver < 8 {
            // v7 → v8 (M9): recognised ink rows carry the pack version that read them.
            conn.execute_batch(search::MIGRATE_V8)?;
        }
        if ver < 9 {
            conn.execute_batch(MIGRATE_V9)?;
        }
        if ver < 10 {
            conn.execute_batch(MIGRATE_V10)?;
        }
        Ok(Store { conn })
    }

    /// Opens an existing workspace read-only (a second connection next to the writer; WAL lets
    /// it read while the storage thread writes). Never creates or migrates.
    pub fn open_reader(path: &Path) -> Result<Store, StorageError> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if ver != SCHEMA_VERSION {
            return Err(StorageError::Version(ver));
        }
        Ok(Store { conn })
    }

    /// Applies ops (already validated by the model) and the matching history changes in one
    /// transaction, and records `next_id`.
    pub fn apply(&mut self, ops: &[Op], hist: &[HistoryOp], next_id: u64) -> Result<(), StorageError> {
        self.apply_batch(ops, hist, next_id, None)
    }

    /// [`Store::apply`] plus, when given, the new workspace settings, all in one transaction.
    pub fn apply_batch(
        &mut self,
        ops: &[Op],
        hist: &[HistoryOp],
        next_id: u64,
        settings: Option<&Settings>,
    ) -> Result<(), StorageError> {
        self.apply_batch_meta(ops, hist, next_id, settings, &[], &[])
    }

    /// [`Store::apply_batch`] plus named session values (JSON; e.g. recently opened notes,
    /// keymap overrides; see [`META_KEYS`]), all in one transaction.
    pub fn apply_batch_meta(
        &mut self,
        ops: &[Op],
        hist: &[HistoryOp],
        next_id: u64,
        settings: Option<&Settings>,
        meta: &[(&str, &str)],
        derived: &[Derived],
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        for d in derived {
            match d {
                Derived::PdfText { blob, pages } => search::set_pdf_text(&tx, blob, pages)?,
                Derived::InkText {
                    note,
                    block,
                    region,
                    text,
                    tag,
                } => {
                    search::set_ink_text(&tx, *note, *block, *region, text, tag)?;
                    tx.execute(
                        "UPDATE ink_block SET tag = ?3 WHERE note = ?1 AND block = ?2",
                        params![i(note.0)?, i(block.0)?, tag],
                    )?;
                }
                Derived::InkBlock { note, block, members } => {
                    let ids: Vec<u64> = members.iter().take(MAX_BLOCK_STROKES).map(|m| m.0).collect();
                    let json = serde_json::to_string(&ids).map_err(|e| StorageError::Corrupt(e.to_string()))?;
                    tx.execute(
                        "INSERT INTO ink_block(note, block, members, tag) VALUES(?1, ?2, ?3, NULL)
                         ON CONFLICT(note, block) DO UPDATE SET members = excluded.members",
                        params![i(note.0)?, i(block.0)?, json],
                    )?;
                }
                Derived::Recording { id, note, data } => {
                    tx.execute(
                        "INSERT OR REPLACE INTO recording(id, note, data) VALUES(?1, ?2, ?3)",
                        params![i(id.0)?, i(note.0)?, data],
                    )?;
                }
                Derived::RecordingDone { id } => {
                    tx.execute("DELETE FROM recording WHERE id = ?1", params![i(id.0)?])?;
                }
                Derived::InkClear { note, block } => {
                    search::set_ink_text(&tx, *note, *block, Rect::EMPTY, "", "")?;
                    tx.execute(
                        "UPDATE ink_block SET tag = NULL WHERE note = ?1 AND block = ?2",
                        params![i(note.0)?, i(block.0)?],
                    )?;
                }
            }
        }
        for (k, v) in meta {
            if !META_KEYS.contains(k) {
                return Err(StorageError::Corrupt(format!("unknown meta key {k}")));
            }
            tx.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )?;
        }
        if let Some(st) = settings {
            let json = serde_json::to_string(st).map_err(|e| StorageError::Corrupt(e.to_string()))?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES('settings', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![json],
            )?;
        }
        for op in ops {
            write_op(&tx, op)?;
        }
        for h in hist {
            history::write(&tx, h)?;
        }
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
            params![next_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Writes the whole workspace (initial save / export to a new file).
    pub fn save_all(&mut self, ws: &Workspace) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        // History describes the replaced state, so it goes too.
        tx.execute_batch(
            "DELETE FROM folder; DELETE FROM note; DELETE FROM object; DELETE FROM object_rt; DELETE FROM history; DELETE FROM trash;",
        )?;
        for f in ws.folders.values() {
            write_op(&tx, &Op::CreateFolder { folder: f.clone() })?;
        }
        for n in ws.notes.values() {
            let canvas = ws.canvases.get(&n.id).cloned().unwrap_or_default();
            write_op(
                &tx,
                &Op::CreateNote {
                    note: n.clone(),
                    canvas,
                },
            )?;
        }
        for n in &ws.trashed_notes {
            write_op(&tx, &Op::TrashNote { note: *n })?;
        }
        for f in &ws.trashed_folders {
            write_op(&tx, &Op::TrashFolder { folder: *f })?;
        }
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
            params![ws.next_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Loads the whole workspace (tools, tests; the app opens lazily with [`Store::load_meta`]).
    pub fn load(&self) -> Result<Workspace, StorageError> {
        let mut ws = self.load_meta()?;
        let mut st = self.conn.prepare("SELECT id, z, kind, data, note FROM object")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let o = object_row(r)?;
            let note: i64 = r.get(4)?;
            let c = ws
                .canvases
                .get_mut(&NoteId(u(note)?))
                .ok_or_else(|| StorageError::Corrupt(format!("object {} of missing note {note}", o.id)))?;
            c.objects.insert(o.id, o);
        }
        Ok(ws)
    }

    /// Loads folders, notes and `next_id`; every canvas is empty (R-PERF-004: canvases load
    /// when a note opens, see [`Store::load_area`] and [`Store::load_page`]).
    pub fn load_meta(&self) -> Result<Workspace, StorageError> {
        let mut ws = Workspace::new();
        let next: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='next_id'", [], |r| r.get(0))
            .optional()?;
        ws.next_id = next
            .unwrap_or_else(|| "1".into())
            .parse()
            .map_err(|_| StorageError::Corrupt("next_id".into()))?;
        let mut st = self.conn.prepare("SELECT id, parent, name, color, sort FROM folder")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (id, parent, name, color, sort) = row?;
            let f = Folder {
                id: FolderId(u(id)?),
                parent: parent.map(u).transpose()?.map(FolderId),
                name,
                color: color.map(color_of).transpose()?,
                sort,
            };
            ws.folders.insert(f.id, f);
        }
        let mut st = self
            .conn
            .prepare("SELECT id, folder, name, color, sort, background FROM note")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, folder, name, color, sort, background) = row?;
            let n = Note {
                id: NoteId(u(id)?),
                folder: folder.map(u).transpose()?.map(FolderId),
                name,
                color: color.map(color_of).transpose()?,
                sort,
                background: background.as_deref().and_then(background_of),
            };
            ws.canvases.insert(n.id, Canvas::default());
            ws.notes.insert(n.id, n);
        }
        // Prettified blocks (small; unreadable rows are skipped: display parameters only).
        let mut st = self.conn.prepare("SELECT note, block, data FROM prettify")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let (note, block, data): (i64, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let (Ok(note), Ok(block)) = (u(note), u(block)) else {
                continue;
            };
            if let Ok(p) = serde_json::from_str::<vibenote_model::Prettify>(&data)
                && ws.notes.contains_key(&NoteId(note))
                && p.strokes.len() == p.transforms.len()
                && p.transforms.iter().flatten().all(|v| v.is_finite())
            {
                ws.prettified
                    .entry(NoteId(note))
                    .or_default()
                    .insert(ObjectId(block), p);
            }
        }
        drop(rows);
        drop(st);
        // Audio marks (unreadable rows are skipped: they only link ink to audio).
        let mut st = self
            .conn
            .prepare("SELECT note, stroke, recording, offset_ms FROM audio_mark")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let (note, stroke, rec, off): (i64, i64, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            if let (Ok(note), Ok(stroke), Ok(rec), Ok(off)) = (u(note), u(stroke), u(rec), u(off))
                && ws.notes.contains_key(&NoteId(note))
            {
                ws.audio_marks.entry(NoteId(note)).or_default().insert(
                    ObjectId(stroke),
                    vibenote_model::AudioMark {
                        recording: ObjectId(rec),
                        offset_ms: off,
                    },
                );
            }
        }
        drop(rows);
        drop(st);
        let mut st = self.conn.prepare("SELECT kind, id FROM trash")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (kind, id) = row?;
            match kind {
                0 if ws.notes.contains_key(&NoteId(u(id)?)) => {
                    ws.trashed_notes.insert(NoteId(u(id)?));
                }
                1 if ws.folders.contains_key(&FolderId(u(id)?)) => {
                    ws.trashed_folders.insert(FolderId(u(id)?));
                }
                _ => return Err(StorageError::Corrupt(format!("trash entry ({kind}, {id})"))),
            }
        }
        Ok(ws)
    }

    /// Stored workspace settings (`None` if never saved). Missing fields take defaults; the
    /// caller validates the values ([`Settings::sanitised`]).
    pub fn load_settings(&self) -> Result<Option<Settings>, StorageError> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'settings'", [], |r| r.get(0))
            .optional()?;
        v.map(|v| serde_json::from_str(&v).map_err(|e| StorageError::Corrupt(format!("settings: {e}"))))
            .transpose()
    }

    /// Stored settings read field by field: a field that is missing takes its default, one that
    /// cannot be read (e.g. a value from a newer build) or is out of range is reset to its
    /// default and named in the returned list; every other field keeps its stored value.
    /// Every blob hash that a stored object or undo/redo step refers to (blob garbage
    /// collection keeps exactly these). Unreadable rows are errors: collecting with a partial
    /// reference set could delete data.
    pub fn blob_refs(&self) -> Result<std::collections::HashSet<String>, StorageError> {
        let mut out = std::collections::HashSet::new();
        let mut st = self
            .conn
            .prepare("SELECT id, z, kind, data FROM object WHERE kind <> 'stroke'")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            out.extend(object_row(r)?.kind.blobs().into_iter().map(String::from));
        }
        // Chunks of recordings in progress (or left by a crash, recovered at the next open).
        let mut st = self.conn.prepare("SELECT data FROM recording")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let data: String = r.get(0)?;
            if let Ok(kind) = serde_json::from_str::<ObjectKind>(&data) {
                out.extend(kind.blobs().into_iter().map(String::from));
            }
        }
        let mut st = self.conn.prepare("SELECT ops FROM history")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let bytes: Vec<u8> = r.get(0)?;
            for op in history::decode_ops(&bytes)? {
                op.for_each_object(&mut |o| {
                    out.extend(o.kind.blobs().into_iter().map(String::from));
                });
            }
        }
        Ok(out)
    }

    /// Recordings left in progress (a crash): (id, note, audio object JSON).
    pub fn recordings(&self) -> Result<Vec<(ObjectId, NoteId, String)>, StorageError> {
        let mut st = self
            .conn
            .prepare("SELECT id, note, data FROM recording ORDER BY id LIMIT 1000")?;
        let mut rows = st.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let (id, note, data): (i64, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            out.push((ObjectId(u(id)?), NoteId(u(note)?), data));
        }
        Ok(out)
    }

    /// The written blocks of a note: (block, strokes, pack version that read it).
    #[allow(clippy::type_complexity)]
    pub fn ink_blocks(&self, note: NoteId) -> Result<Vec<(ObjectId, Vec<ObjectId>, Option<String>)>, StorageError> {
        let mut st = self
            .conn
            .prepare_cached("SELECT block, members, tag FROM ink_block WHERE note = ?1 ORDER BY block LIMIT ?2")?;
        let lim = i64::try_from(MAX_BLOCKS_PER_NOTE).unwrap_or(i64::MAX);
        let mut rows = st.query(params![i(note.0)?, lim])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let block: i64 = r.get(0)?;
            let members: String = r.get(1)?;
            // Derived data: an unreadable row is skipped (the block is simply not re-read).
            let Ok(ids) = serde_json::from_str::<Vec<u64>>(&members) else {
                continue;
            };
            out.push((
                ObjectId(u(block)?),
                ids.into_iter().take(MAX_BLOCK_STROKES).map(ObjectId).collect(),
                r.get(2)?,
            ));
        }
        Ok(out)
    }

    /// Search rows matching a query in both forms (see [`search::query`]).
    pub fn search(
        &self,
        q: &(String, String),
        limit: usize,
        only: Option<NoteId>,
    ) -> Result<Vec<search::SearchRow>, StorageError> {
        search::query(&self.conn, q, limit, only)
    }

    /// Link objects (see [`search::links`]).
    /// Every typed-text and handwriting row (see [`search::written`]).
    pub fn written(&self, limit: usize) -> Result<Vec<search::SearchRow>, StorageError> {
        search::written(&self.conn, limit)
    }

    pub fn search_links(&self, only: Option<NoteId>) -> Result<Vec<search::LinkRow>, StorageError> {
        search::links(&self.conn, only)
    }

    /// PDF blobs without extracted text (see [`search::pdf_blobs_without_text`]).
    pub fn pdf_blobs_without_text(&self) -> Result<Vec<String>, StorageError> {
        search::pdf_blobs_without_text(&self.conn)
    }

    /// A named session value (JSON text) as stored, if any. Callers parse it leniently.
    pub fn load_meta_value(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn load_settings_lenient(&self) -> Result<Option<(Settings, Vec<String>)>, StorageError> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'settings'", [], |r| r.get(0))
            .optional()?;
        Ok(v.map(|v| settings_lenient(&v)))
    }

    /// Objects of `note` whose bounds intersect `area` (R-tree; objects without bounds always
    /// match). The first viewport of a note opening.
    pub fn load_area(&self, note: NoteId, area: &Rect) -> Result<Vec<CanvasObject>, StorageError> {
        let mut st = self.conn.prepare_cached(
            // CROSS JOIN keeps the R-tree as the outer loop; otherwise SQLite walks every
            // object of the note and probes the R-tree per row (134 ms vs 4 ms on the
            // reference note).
            "SELECT o.id, o.z, o.kind, o.data FROM object_rt r CROSS JOIN object o ON o.id = r.id
             WHERE r.nlo <= ?1 AND r.nhi >= ?1 AND r.maxx >= ?2 AND r.minx <= ?3
               AND r.maxy >= ?4 AND r.miny <= ?5 AND o.note = ?1",
        )?;
        let mut rows = st.query(params![i(note.0)?, area.min.x, area.max.x, area.min.y, area.max.y])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(object_row(r)?);
        }
        Ok(out)
    }

    /// Up to `limit` objects of `note` with id greater than `after`, in id order (keyset paging
    /// for streaming the rest of a note; each page is its own read).
    pub fn load_page(
        &self,
        note: NoteId,
        after: Option<ObjectId>,
        limit: u32,
    ) -> Result<Vec<CanvasObject>, StorageError> {
        let after = match after {
            Some(a) => i(a.0)?,
            None => -1,
        };
        let mut st = self
            .conn
            .prepare_cached("SELECT id, z, kind, data FROM object WHERE note = ?1 AND id > ?2 ORDER BY id LIMIT ?3")?;
        let mut rows = st.query(params![i(note.0)?, after, limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(object_row(r)?);
        }
        Ok(out)
    }

    /// Bounds of a note's stored objects with finite boxes (R-tree; objects without bounds,
    /// stored as the whole plane, are left out). `None` if there are none.
    pub fn note_bounds(&self, note: NoteId) -> Result<Option<Rect>, StorageError> {
        let r: (Option<f64>, Option<f64>, Option<f64>, Option<f64>) = self
            .conn
            .prepare_cached(
                // The R-tree stores note ids as f32 (rounded outwards): the join keeps exactly this
                // note; CROSS JOIN keeps the R-tree as the outer loop.
                "SELECT MIN(r.minx), MAX(r.maxx), MIN(r.miny), MAX(r.maxy)
             FROM object_rt r CROSS JOIN object o ON o.id = r.id
             WHERE r.nlo <= ?1 AND r.nhi >= ?1 AND o.note = ?1 AND r.minx > ?2",
            )?
            .query_row(params![i(note.0)?, -WHOLE_PLANE / 2.0], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
        Ok(match r {
            (Some(x0), Some(x1), Some(y0), Some(y1)) => Some(Rect {
                min: vibenote_geom::Point::new(x0, y0),
                max: vibenote_geom::Point::new(x1, y1),
            }),
            _ => None,
        })
    }

    /// Up to `limit` stored objects of a note as (z, id), highest first, strictly below
    /// `below` in that order (keyset paging; `None` = from the top).
    pub fn top_z(
        &self,
        note: NoteId,
        below: Option<(i64, ObjectId)>,
        limit: u32,
    ) -> Result<Vec<(i64, ObjectId)>, StorageError> {
        let (bz, bid) = match below {
            Some((z, id)) => (z, i(id.0)?),
            None => (i64::MAX, i64::MAX),
        };
        let mut st = self.conn.prepare_cached(
            "SELECT z, id FROM object WHERE note = ?1 AND (z < ?2 OR (z = ?2 AND id < ?3))
             ORDER BY z DESC, id DESC LIMIT ?4",
        )?;
        let mut rows = st.query(params![i(note.0)?, bz, bid, limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push((r.get(0)?, ObjectId(u(r.get(1)?)?)));
        }
        Ok(out)
    }

    /// Topmost z of a note's stored objects (new objects go above it even before the note is
    /// fully loaded).
    pub fn max_z(&self, note: NoteId) -> Result<Option<i64>, StorageError> {
        Ok(self
            .conn
            .prepare_cached("SELECT MAX(z) FROM object WHERE note = ?1")?
            .query_row(params![i(note.0)?], |r| r.get(0))?)
    }

    /// The stored undo/redo stacks of one scope ("workspace", "note:<id>"), oldest first.
    pub fn load_history(&self, scope: &str) -> Result<StoredHistory, StorageError> {
        history::load(&self.conn, scope)
    }

    /// Journal mode and synchronous level in effect (expected: "wal", 2 = FULL).
    pub fn durability(&self) -> Result<(String, i64), StorageError> {
        let jm: String = self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let sync: i64 = self.conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        Ok((jm, sync))
    }

    /// Format version recorded in the database (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// `PRAGMA integrity_check` (crash tests).
    pub fn integrity_check(&self) -> Result<String, StorageError> {
        Ok(self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?)
    }
}

fn color_of(v: i64) -> Result<Color, StorageError> {
    u32::try_from(v)
        .map(Color)
        .map_err(|_| StorageError::Corrupt(format!("colour {v}")))
}

fn settings_lenient(json: &str) -> (Settings, Vec<String>) {
    use serde_json::Value;
    let mut st = Settings::default();
    let mut bad = Vec::new();
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(json) else {
        return (st, vec!["settings".into()]);
    };
    fn field<T: serde::de::DeserializeOwned>(
        m: &serde_json::Map<String, Value>,
        k: &str,
        out: &mut T,
        bad: &mut Vec<String>,
        name: &str,
    ) {
        if let Some(v) = m.get(k) {
            match serde_json::from_value(v.clone()) {
                Ok(x) => *out = x,
                Err(_) => bad.push(name.to_string()),
            }
        }
    }
    field(&m, "tool", &mut st.tool, &mut bad, "tool");
    field(&m, "eraser_mode", &mut st.eraser_mode, &mut bad, "eraser_mode");
    field(&m, "prettify_mode", &mut st.prettify_mode, &mut bad, "prettify_mode");
    field(&m, "block_idle_ms", &mut st.block_idle_ms, &mut bad, "block_idle_ms");
    field(&m, "prediction_ms", &mut st.prediction_ms, &mut bad, "prediction_ms");
    field(&m, "barrel1", &mut st.barrel1, &mut bad, "barrel1");
    field(&m, "barrel2", &mut st.barrel2, &mut bad, "barrel2");
    field(&m, "shape", &mut st.shape, &mut bad, "shape");
    field(&m, "snap_on_lift", &mut st.snap_on_lift, &mut bad, "snap_on_lift");
    field(&m, "vim", &mut st.vim, &mut bad, "vim");
    field(&m, "pen_color", &mut st.pen_color, &mut bad, "pen_color");
    field(
        &m,
        "highlighter_color",
        &mut st.highlighter_color,
        &mut bad,
        "highlighter_color",
    );
    match m.get("background") {
        Some(Value::Object(b)) => {
            field(b, "kind", &mut st.background.kind, &mut bad, "background.kind");
            field(b, "spacing", &mut st.background.spacing, &mut bad, "background.spacing");
        }
        Some(_) => bad.push("background".into()),
        None => {}
    }
    let (st, fixed) = st.sanitised();
    bad.extend(fixed);
    (st, bad)
}

/// Decodes an `(id, z, kind, data)` row.
pub(crate) fn object_row(r: &rusqlite::Row<'_>) -> Result<CanvasObject, StorageError> {
    let id: i64 = r.get(0)?;
    let kind: String = r.get(2)?;
    let data: Vec<u8> = r.get(3)?;
    let kind = match kind.as_str() {
        "stroke" => ObjectKind::Stroke(decode_stroke(&data)?),
        _ => serde_json::from_slice(&data).map_err(|e| StorageError::Corrupt(format!("object {id}: {e}")))?,
    };
    Ok(CanvasObject {
        id: ObjectId(u(id)?),
        z: r.get(1)?,
        kind,
    })
}

fn insert_object(tx: &rusqlite::Transaction<'_>, note: NoteId, o: &CanvasObject) -> Result<(), StorageError> {
    let (kind, data) = match &o.kind {
        ObjectKind::Stroke(s) => ("stroke", encode_stroke(s)?),
        other => (
            "json",
            serde_json::to_vec(other).map_err(|e| StorageError::Corrupt(e.to_string()))?,
        ),
    };
    tx.execute(
        "INSERT INTO object(id, note, z, kind, data) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![i(o.id.0)?, i(note.0)?, o.z, kind, data],
    )?;
    let b = o.kind.bounds().unwrap_or(Rect::EMPTY);
    let (minx, maxx, miny, maxy) = if b.is_empty() {
        (-WHOLE_PLANE, WHOLE_PLANE, -WHOLE_PLANE, WHOLE_PLANE)
    } else {
        (b.min.x, b.max.x, b.min.y, b.max.y)
    };
    tx.execute(
        "INSERT INTO object_rt VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![i(o.id.0)?, minx, maxx, miny, maxy, i(note.0)?],
    )?;
    search::index(tx, note, o)?;
    Ok(())
}

fn delete_object(tx: &rusqlite::Transaction<'_>, id: ObjectId) -> Result<(), StorageError> {
    tx.execute("DELETE FROM object WHERE id = ?1", params![i(id.0)?])?;
    tx.execute("DELETE FROM object_rt WHERE id = ?1", params![i(id.0)?])?;
    search::unindex(tx, id)?;
    Ok(())
}

fn write_op(tx: &rusqlite::Transaction<'_>, op: &Op) -> Result<(), StorageError> {
    let col = |c: Option<Color>| c.map(|c| i64::from(c.0));
    match op {
        Op::CreateFolder { folder: f } | Op::UpdateFolder { after: f } => {
            tx.execute(
                "INSERT INTO folder(id, parent, name, color, sort) VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET parent=excluded.parent, name=excluded.name, color=excluded.color, sort=excluded.sort",
                params![i(f.id.0)?, f.parent.map(|p| i(p.0)).transpose()?, f.name, col(f.color), f.sort],
            )?;
        }
        Op::DeleteFolder { folder } => {
            tx.execute("DELETE FROM folder WHERE id = ?1", params![i(folder.id.0)?])?;
            tx.execute("DELETE FROM trash WHERE kind = 1 AND id = ?1", params![i(folder.id.0)?])?;
        }
        Op::CreateNote { note: n, canvas } => {
            write_note(tx, n)?;
            for o in canvas.objects.values() {
                insert_object(tx, n.id, o)?;
            }
        }
        Op::UpdateNote { after: n } => write_note(tx, n)?,
        Op::DeleteNote { note } => {
            let ids: Vec<i64> = {
                let mut st = tx.prepare("SELECT id FROM object WHERE note = ?1")?;
                st.query_map(params![i(note.id.0)?], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            for id in ids {
                delete_object(tx, ObjectId(u(id)?))?;
            }
            tx.execute("DELETE FROM note WHERE id = ?1", params![i(note.id.0)?])?;
            tx.execute("DELETE FROM prettify WHERE note = ?1", params![i(note.id.0)?])?;
            tx.execute("DELETE FROM ink_block WHERE note = ?1", params![i(note.id.0)?])?;
            tx.execute("DELETE FROM audio_mark WHERE note = ?1", params![i(note.id.0)?])?;
            search::unindex_note(tx, note.id)?;
            tx.execute("DELETE FROM trash WHERE kind = 0 AND id = ?1", params![i(note.id.0)?])?;
        }
        Op::InsertObject { note, object } => insert_object(tx, *note, object)?,
        Op::RemoveObject { note: _, object } => delete_object(tx, object.id)?,
        Op::ReplaceObject { note, after } => {
            delete_object(tx, after.id)?;
            insert_object(tx, *note, after)?;
        }
        Op::TrashNote { note } => {
            tx.execute("INSERT INTO trash(kind, id) VALUES(0, ?1)", params![i(note.0)?])?;
        }
        Op::RestoreNote { note } => {
            tx.execute("DELETE FROM trash WHERE kind = 0 AND id = ?1", params![i(note.0)?])?;
        }
        Op::TrashFolder { folder } => {
            tx.execute("INSERT INTO trash(kind, id) VALUES(1, ?1)", params![i(folder.0)?])?;
        }
        Op::RestoreFolder { folder } => {
            tx.execute("DELETE FROM trash WHERE kind = 1 AND id = ?1", params![i(folder.0)?])?;
        }
        Op::SetAudioMark { note, stroke, after } => match after {
            Some(m) => {
                tx.execute(
                    "INSERT OR REPLACE INTO audio_mark(note, stroke, recording, offset_ms) VALUES(?1, ?2, ?3, ?4)",
                    params![i(note.0)?, i(stroke.0)?, i(m.recording.0)?, i(m.offset_ms)?],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM audio_mark WHERE note = ?1 AND stroke = ?2",
                    params![i(note.0)?, i(stroke.0)?],
                )?;
            }
        },
        Op::SetPrettify { note, block, after } => match after {
            Some(p) => {
                let json = serde_json::to_string(p).map_err(|e| StorageError::Corrupt(format!("prettify: {e}")))?;
                tx.execute(
                    "INSERT OR REPLACE INTO prettify(note, block, data) VALUES(?1, ?2, ?3)",
                    params![i(note.0)?, i(block.0)?, json],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM prettify WHERE note = ?1 AND block = ?2",
                    params![i(note.0)?, i(block.0)?],
                )?;
            }
        },
        Op::SetNoteBackground { note, background } => {
            let json = background
                .map(|b| serde_json::to_string(&b))
                .transpose()
                .map_err(|e| StorageError::Corrupt(format!("note background: {e}")))?;
            tx.execute(
                "UPDATE note SET background = ?1 WHERE id = ?2",
                params![json, i(note.0)?],
            )?;
        }
    }
    Ok(())
}

/// A stored note background; unreadable or out-of-range values fall back to the workspace
/// default (stored data is hostile input and must not stop the workspace from opening).
fn background_of(json: &str) -> Option<vibenote_model::settings::Background> {
    serde_json::from_str::<vibenote_model::settings::Background>(json)
        .ok()
        .filter(|b| b.validate().is_ok())
}

fn write_note(tx: &rusqlite::Transaction<'_>, n: &Note) -> Result<(), StorageError> {
    let background = n
        .background
        .map(|b| serde_json::to_string(&b))
        .transpose()
        .map_err(|e| StorageError::Corrupt(format!("note background: {e}")))?;
    tx.execute(
        "INSERT INTO note(id, folder, name, color, sort, background) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET folder=excluded.folder, name=excluded.name, color=excluded.color,
             sort=excluded.sort, background=excluded.background",
        params![
            i(n.id.0)?,
            n.folder.map(|f| i(f.0)).transpose()?,
            n.name,
            n.color.map(|c| i64::from(c.0)),
            n.sort,
            background
        ],
    )?;
    Ok(())
}
