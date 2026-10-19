//! Search index (ADR 0005 decisions 1 and 5; R-SEARCH-001..003, R-PDF-004, R-TEXT-003): an FTS5
//! table with the trigram tokenizer over our two normalised forms of each text, next to a row
//! table that says where each text comes from. It is written in the same transaction as the
//! objects it describes (the storage thread), so it is never out of step with the canvas.
//!
//! Rows: a text box gives one row, a PDF object one row per page with extracted text, a link one
//! row (its label follows the target's name, so links are matched at query time).

use crate::{StorageError, i, object_row, u};
use rusqlite::{Connection, params};
use vibenote_geom::Rect;
use vibenote_model::search::{forms, text_box};
use vibenote_model::{CanvasObject, LinkTarget, NoteId, ObjectId, ObjectKind, pdf_layout};

/// Schema of the index (v6 → v7); also used by in-memory indexes.
pub(crate) const SCHEMA: &str = "
     CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(a, b, tokenize = 'trigram');
     CREATE TABLE IF NOT EXISTS search_row(rowid INTEGER PRIMARY KEY, object INTEGER NOT NULL,
         note INTEGER NOT NULL, source TEXT NOT NULL, page INTEGER, region TEXT, raw TEXT NOT NULL);
     CREATE INDEX IF NOT EXISTS search_row_object ON search_row(object);";

/// v7 → v8: the version tag of the recogniser that produced an ink row.
pub(crate) const MIGRATE_V8: &str = "BEGIN;
     ALTER TABLE search_row ADD COLUMN tag TEXT;
     PRAGMA user_version = 8;
     COMMIT;";

/// A matching row.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchRow {
    pub object: ObjectId,
    pub note: NoteId,
    /// "text", "pdf" (and "ink" once handwriting is recognised).
    pub source: String,
    /// 1-based PDF page.
    pub page: Option<u32>,
    pub region: Option<Rect>,
    pub raw: String,
}

/// A link object (matched by the caller against its target's current name).
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRow {
    pub object: ObjectId,
    pub note: NoteId,
    pub target: LinkTarget,
    pub region: Option<Rect>,
}

fn region_json(r: Option<Rect>) -> Option<String> {
    r.map(|r| format!("[{},{},{},{}]", r.min.x, r.min.y, r.max.x, r.max.y))
}

fn region_of(s: Option<String>) -> Option<Rect> {
    let v: [f64; 4] = serde_json::from_str(&s?).ok()?;
    v.iter().all(|x| x.is_finite()).then(|| Rect {
        min: vibenote_geom::Point::new(v[0], v[1]),
        max: vibenote_geom::Point::new(v[2], v[3]),
    })
}

fn insert(
    c: &Connection,
    (object, note): (ObjectId, NoteId),
    source: &str,
    page: Option<u32>,
    region: Option<Rect>,
    raw: &str,
    text: bool,
) -> Result<(), StorageError> {
    c.execute(
        "INSERT INTO search_row(object, note, source, page, region, raw) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![i(object.0)?, i(note.0)?, source, page, region_json(region), raw],
    )?;
    if text {
        let (a, b) = forms(raw);
        c.execute(
            "INSERT INTO search_fts(rowid, a, b) VALUES(last_insert_rowid(), ?1, ?2)",
            params![a, b],
        )?;
    }
    Ok(())
}

/// Removes an object's rows.
/// Removes an object's own rows. Recognised handwriting rows are keyed by a block's first
/// stroke but belong to the block: they stay when that stroke is moved or erased (the engine
/// clears them when none of the block's strokes is left).
pub(crate) fn unindex(c: &Connection, id: ObjectId) -> Result<(), StorageError> {
    c.execute(
        "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM search_row WHERE object = ?1 AND source <> 'ink')",
        params![i(id.0)?],
    )?;
    c.execute(
        "DELETE FROM search_row WHERE object = ?1 AND source <> 'ink'",
        params![i(id.0)?],
    )?;
    Ok(())
}

/// Removes every row of a note (it was deleted).
pub(crate) fn unindex_note(c: &Connection, note: NoteId) -> Result<(), StorageError> {
    c.execute(
        "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM search_row WHERE note = ?1)",
        params![i(note.0)?],
    )?;
    c.execute("DELETE FROM search_row WHERE note = ?1", params![i(note.0)?])?;
    Ok(())
}

/// (Re)writes an object's rows (none for ink and kinds without text).
pub(crate) fn index(c: &Connection, note: NoteId, o: &CanvasObject) -> Result<(), StorageError> {
    unindex(c, o.id)?;
    match &o.kind {
        ObjectKind::Text {
            origin,
            width,
            content,
            marks,
            size,
        } => insert(
            c,
            (o.id, note),
            "text",
            None,
            Some(text_box(*origin, *width, *size, content, marks)),
            content,
            true,
        )?,
        ObjectKind::Link { target, origin } => {
            let raw = serde_json::to_string(target).map_err(|e| StorageError::Corrupt(e.to_string()))?;
            insert(
                c,
                (o.id, note),
                "link",
                None,
                Some(Rect {
                    min: *origin,
                    max: *origin,
                }),
                &raw,
                false,
            )?;
        }
        ObjectKind::Pdf {
            origin,
            scale,
            gap,
            blob,
            pages,
        } => {
            let mut st = c.prepare_cached("SELECT text FROM pdf_text WHERE blob = ?1 AND page = ?2")?;
            for (r, p) in pdf_layout(*origin, *scale, *gap, pages).into_iter().zip(pages) {
                let text: Option<String> = st
                    .query_row(params![blob, i64::from(p.index)], |row| row.get(0))
                    .map(Some)
                    .or_else(|e| {
                        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                            Ok(None)
                        } else {
                            Err(e)
                        }
                    })?;
                if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
                    insert(
                        c,
                        (o.id, note),
                        "pdf",
                        Some(p.index.saturating_add(1)),
                        Some(r),
                        &t,
                        true,
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Re-indexes every PDF object showing `blob` (its text just arrived).
pub(crate) fn reindex_blob(c: &Connection, blob: &str) -> Result<(), StorageError> {
    let objs = non_stroke(c)?;
    for (note, o) in objs {
        if o.kind.blob() == Some(blob) && matches!(o.kind, ObjectKind::Pdf { .. }) {
            index(c, note, &o)?;
        }
    }
    Ok(())
}

pub(crate) fn non_stroke(c: &Connection) -> Result<Vec<(NoteId, CanvasObject)>, StorageError> {
    let mut st =
        c.prepare_cached("SELECT id, z, kind, data, note FROM object INDEXED BY object_kind WHERE kind <> 'stroke'")?;
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let note: i64 = r.get(4)?;
        out.push((NoteId(u(note)?), object_row(r)?));
    }
    Ok(out)
}

/// A GLOB pattern matching texts that contain `s` (the trigram index serves GLOB, and LIKE
/// only without an ESCAPE clause). Both sides are normalised to lower case, so GLOB's case
/// sensitivity changes nothing; its wildcards are escaped as one-character classes.
fn glob(s: &str) -> String {
    let mut out = String::from("*");
    for ch in s.chars() {
        match ch {
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            c => out.push(c),
        }
    }
    out.push('*');
    out
}

/// Rows whose text contains the query (in either normalised form), optionally of one note.
pub fn query(
    c: &Connection,
    q: &(String, String),
    limit: usize,
    only: Option<NoteId>,
) -> Result<Vec<SearchRow>, StorageError> {
    let mut st = c.prepare_cached(
        "SELECT r.object, r.note, r.source, r.page, r.region, r.raw FROM search_fts f JOIN search_row r ON r.rowid = f.rowid
         WHERE (f.a GLOB ?1 OR f.b GLOB ?2) AND (?3 IS NULL OR r.note = ?3)
         ORDER BY r.note, r.object, r.page LIMIT ?4",
    )?;
    let only = only.map(|n| i(n.0)).transpose()?;
    let lim = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut rows = st.query(params![glob(&q.0), glob(&q.1), only, lim])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let (object, note): (i64, i64) = (r.get(0)?, r.get(1)?);
        let page: Option<i64> = r.get(3)?;
        out.push(SearchRow {
            object: ObjectId(u(object)?),
            note: NoteId(u(note)?),
            source: r.get(2)?,
            page: page.and_then(|p| u32::try_from(p).ok()),
            region: region_of(r.get(4)?),
            raw: r.get(5)?,
        });
    }
    Ok(out)
}

/// Every row of typed text, recognised handwriting and PDF text (sources "text", "ink",
/// "pdf"), at most `limit`, for the finder's fuzzy matching (R-VIM-005).
pub fn written(c: &Connection, limit: usize) -> Result<Vec<SearchRow>, StorageError> {
    let mut st = c.prepare_cached(
        "SELECT object, note, source, page, region, raw FROM search_row WHERE source IN ('text', 'ink', 'pdf')
         ORDER BY note, object LIMIT ?1",
    )?;
    let lim = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut rows = st.query(params![lim])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let (object, note): (i64, i64) = (r.get(0)?, r.get(1)?);
        let page: Option<i64> = r.get(3)?;
        out.push(SearchRow {
            object: ObjectId(u(object)?),
            note: NoteId(u(note)?),
            source: r.get(2)?,
            page: page.and_then(|p| u32::try_from(p).ok()),
            region: region_of(r.get(4)?),
            raw: r.get(5)?,
        });
    }
    Ok(out)
}

/// All link objects (optionally of one note).
pub fn links(c: &Connection, only: Option<NoteId>) -> Result<Vec<LinkRow>, StorageError> {
    let mut st = c.prepare_cached(
        "SELECT object, note, region, raw FROM search_row WHERE source = 'link' AND (?1 IS NULL OR note = ?1)",
    )?;
    let only = only.map(|n| i(n.0)).transpose()?;
    let mut rows = st.query(params![only])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let (object, note): (i64, i64) = (r.get(0)?, r.get(1)?);
        let raw: String = r.get(3)?;
        // A row that does not parse is skipped (derived data; the object itself is checked).
        if let Ok(target) = serde_json::from_str(&raw) {
            out.push(LinkRow {
                object: ObjectId(u(object)?),
                note: NoteId(u(note)?),
                target,
                region: region_of(r.get(2)?),
            });
        }
    }
    Ok(out)
}

/// PDF blobs shown by some object whose text has not been extracted yet.
pub fn pdf_blobs_without_text(c: &Connection) -> Result<Vec<String>, StorageError> {
    let mut out: Vec<String> = Vec::new();
    let mut has = c.prepare_cached("SELECT 1 FROM pdf_text WHERE blob = ?1 LIMIT 1")?;
    for (_, o) in non_stroke(c)? {
        if let ObjectKind::Pdf { blob, .. } = &o.kind
            && !out.contains(blob)
            && !has.exists(params![blob])?
        {
            out.push(blob.clone());
        }
    }
    Ok(out)
}

/// Replaces the recognised text of a handwriting block (rows keyed by its first stroke; they
/// go when that stroke is erased or changed).
pub(crate) fn set_ink_text(
    c: &Connection,
    note: NoteId,
    block: ObjectId,
    region: Rect,
    text: &str,
    tag: &str,
) -> Result<(), StorageError> {
    c.execute(
        "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM search_row WHERE object = ?1 AND source = 'ink')",
        params![i(block.0)?],
    )?;
    c.execute(
        "DELETE FROM search_row WHERE object = ?1 AND source = 'ink'",
        params![i(block.0)?],
    )?;
    if text.trim().is_empty() {
        return Ok(());
    }
    insert(c, (block, note), "ink", None, Some(region), text, true)?;
    c.execute(
        "UPDATE search_row SET tag = ?1 WHERE rowid = last_insert_rowid()",
        params![tag],
    )?;
    Ok(())
}

/// Stores a PDF's page texts and re-indexes the objects that show it.
pub(crate) fn set_pdf_text(c: &Connection, blob: &str, pages: &[String]) -> Result<(), StorageError> {
    if !crate::blobs::is_hash(blob) {
        return Err(StorageError::Corrupt(format!("not a blob hash: {blob:?}")));
    }
    for (k, text) in pages.iter().enumerate() {
        c.execute(
            "INSERT OR REPLACE INTO pdf_text(blob, page, text) VALUES(?1, ?2, ?3)",
            params![blob, i64::try_from(k).unwrap_or(i64::MAX), text],
        )?;
    }
    reindex_blob(c, blob)
}

/// An index for engines without a workspace file: the same schema and SQL on a private
/// in-memory database that mirrors the session's non-stroke objects.
pub struct MemoryIndex {
    conn: Connection,
}

impl MemoryIndex {
    pub fn new() -> Result<MemoryIndex, StorageError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE object(id INTEGER PRIMARY KEY, note INTEGER NOT NULL, z INTEGER NOT NULL, kind TEXT NOT NULL, data BLOB NOT NULL);
             CREATE INDEX object_kind ON object(kind) WHERE kind <> 'stroke';
             CREATE TABLE pdf_text(blob TEXT NOT NULL, page INTEGER NOT NULL, text TEXT NOT NULL, PRIMARY KEY(blob, page)) WITHOUT ROWID;",
        )?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch("ALTER TABLE search_row ADD COLUMN tag TEXT;")?;
        Ok(MemoryIndex { conn })
    }

    /// Mirrors an object (ink is ignored).
    pub fn put(&mut self, note: NoteId, o: &CanvasObject) -> Result<(), StorageError> {
        if matches!(o.kind, ObjectKind::Stroke(_)) {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM object WHERE id = ?1", params![i(o.id.0)?])?;
        let data = serde_json::to_vec(&o.kind).map_err(|e| StorageError::Corrupt(e.to_string()))?;
        tx.execute(
            "INSERT INTO object(id, note, z, kind, data) VALUES(?1, ?2, ?3, 'json', ?4)",
            params![i(o.id.0)?, i(note.0)?, o.z, data],
        )?;
        index(&tx, note, o)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, id: ObjectId) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM object WHERE id = ?1", params![i(id.0)?])?;
        unindex(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets every object of a deleted note.
    pub fn remove_note(&mut self, note: NoteId) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM search_row WHERE note = ?1)",
            params![i(note.0)?],
        )?;
        tx.execute("DELETE FROM search_row WHERE note = ?1", params![i(note.0)?])?;
        tx.execute("DELETE FROM object WHERE note = ?1", params![i(note.0)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_ink_text(
        &mut self,
        note: NoteId,
        block: ObjectId,
        region: Rect,
        text: &str,
        tag: &str,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        set_ink_text(&tx, note, block, region, text, tag)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_pdf_text(&mut self, blob: &str, pages: &[String]) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        set_pdf_text(&tx, blob, pages)?;
        tx.commit()?;
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}
