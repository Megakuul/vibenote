//! Search (R-SEARCH-001..003, R-PDF-004, R-TEXT-003): note and folder names (live), text boxes,
//! link labels (matched against the target's current name) and PDF text, through the FTS5 index
//! of ADR 0005 (`vibenote_storage::search`), which the storage thread keeps in step with the
//! objects. Engines without a workspace file use the same schema in memory.
//!
//! Normalisation (R-SEARCH-003, ADR 0005 decision 5): lower case, accents stripped, umlauts in
//! both forms; a query matches if either of its forms occurs in the same form of the text.

use vibenote_geom::Rect;
use vibenote_model::NoteId;

/// Longest query considered (characters).
pub const MAX_QUERY: usize = 256;
/// Most results returned.
pub const MAX_RESULTS: usize = 500;

pub use vibenote_model::search::forms;

/// Where a hit comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Folder,
    Note,
    Text,
    Link,
    Pdf,
    Ink,
}

/// A query in both forms (`None` if nothing searchable remains).
pub(crate) fn query_forms(q: &str) -> Option<(String, String)> {
    let q: String = q.chars().take(MAX_QUERY).collect();
    let (a, b) = forms(q.trim());
    (!a.trim().is_empty()).then_some((a, b))
}

/// True if the query (both forms) occurs in the text (both forms).
pub(crate) fn matches(q: &(String, String), a: &str, b: &str) -> bool {
    a.contains(q.0.as_str()) || b.contains(q.1.as_str())
}

/// A short excerpt of `raw` around the first match (whole words, about 80 characters).
pub(crate) fn snippet(raw: &str, q: &(String, String)) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= 80 {
        return raw.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    // The first character whose suffix starts with the query (bounded scan).
    let mut at = 0usize;
    for i in 0..chars.len().min(20_000) {
        let tail: String = chars
            .iter()
            .skip(i)
            .take(q.0.chars().count().saturating_add(8))
            .collect();
        let (a, b) = forms(&tail);
        if a.starts_with(q.0.as_str()) || b.starts_with(q.1.as_str()) {
            at = i;
            break;
        }
    }
    let start = at.saturating_sub(30);
    let end = start.saturating_add(80).min(chars.len());
    let s: String = chars.get(start..end).unwrap_or_default().iter().collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        s,
        if end < chars.len() { "…" } else { "" }
    )
}

/// A search hit.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Hit {
    pub source: Source,
    /// The note the hit is in (for a folder: none).
    pub note: Option<u64>,
    /// Folder hits: the folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Canvas region `[x0, y0, x1, y1]` to show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<[f64; 4]>,
    pub snippet: String,
}

fn region(r: Option<Rect>) -> Option<[f64; 4]> {
    r.map(|r| [r.min.x, r.min.y, r.max.x, r.max.y])
}

/// The search index of an engine without a workspace file (ADR 0005's FTS5 schema on a private
/// in-memory database; created on the first search, then kept current by every op).
#[derive(Default)]
pub(crate) struct MemSearch(pub std::sync::Mutex<Option<vibenote_storage::search::MemoryIndex>>);

impl Clone for MemSearch {
    fn clone(&self) -> Self {
        MemSearch::default()
    }
}
impl PartialEq for MemSearch {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl std::fmt::Debug for MemSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemSearch")
    }
}

fn storage(e: vibenote_storage::StorageError) -> crate::EngineError {
    crate::EngineError::Storage(e.to_string())
}

fn source_of(s: &str) -> Source {
    match s {
        "pdf" => Source::Pdf,
        "ink" => Source::Ink,
        "link" => Source::Link,
        _ => Source::Text,
    }
}

impl crate::Session {
    pub(crate) fn mem_index(&self) -> std::sync::MutexGuard<'_, Option<vibenote_storage::search::MemoryIndex>> {
        self.mem_search
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps an in-memory index current for an applied op (once it exists).
    pub(crate) fn search_follow(&mut self, op: &vibenote_model::Op) {
        use vibenote_model::Op;
        let mut g = self.mem_index();
        let Some(ix) = g.as_mut() else { return };
        // Derived data: a failure only makes search miss this object until it changes again.
        let _ = match op {
            Op::InsertObject { note, object } | Op::ReplaceObject { note, after: object } => ix.put(*note, object),
            Op::RemoveObject { object, .. } => ix.remove(object.id),
            Op::CreateNote { note, canvas } => canvas.objects.values().try_for_each(|o| ix.put(note.id, o)),
            Op::DeleteNote { note } => ix.remove_note(note.id),
            _ => Ok(()),
        };
    }

    /// The in-memory index, built from the (complete) in-memory workspace on first use.
    fn ensure_mem_index(&self) -> Result<(), crate::EngineError> {
        let mut g = self.mem_index();
        if g.is_some() {
            return Ok(());
        }
        let mut ix = vibenote_storage::search::MemoryIndex::new().map_err(storage)?;
        for (note, c) in &self.ws.canvases {
            for o in c.objects.values() {
                ix.put(*note, o).map_err(storage)?;
            }
        }
        *g = Some(ix);
        Ok(())
    }

    /// Records extracted PDF text: stored with the next flush, or in the in-memory index.
    pub(crate) fn set_pdf_text(&mut self, blob: String, pages: Vec<String>) -> Result<(), crate::EngineError> {
        if self.persistent() {
            self.derived_pending
                .push(vibenote_storage::Derived::PdfText { blob, pages });
            return Ok(());
        }
        self.ensure_mem_index()?;
        if let Some(ix) = self.mem_index().as_mut() {
            ix.set_pdf_text(&blob, &pages).map_err(storage)?;
        }
        Ok(())
    }

    /// Records the recognised text of a handwriting block (stored with the next flush, or in
    /// the in-memory index).
    pub(crate) fn set_ink_text(
        &mut self,
        note: NoteId,
        block: vibenote_model::ObjectId,
        region: Rect,
        r: &crate::hwr::BlockText,
    ) -> Result<(), crate::EngineError> {
        if self.persistent() {
            self.derived_pending.push(vibenote_storage::Derived::InkText {
                note,
                block,
                region,
                text: r.text.clone(),
                tag: r.tag.clone(),
            });
            return Ok(());
        }
        self.ensure_mem_index()?;
        if let Some(ix) = self.mem_index().as_mut() {
            ix.set_ink_text(note, block, region, &r.text, &r.tag).map_err(storage)?;
        }
        Ok(())
    }

    /// PDF blobs shown somewhere whose text is not extracted yet (storage must be in sync).
    pub(crate) fn pdf_blobs_without_text(&self) -> Result<Vec<String>, crate::EngineError> {
        if self.persistent() {
            return match self.reader.with(vibenote_storage::Store::pdf_blobs_without_text) {
                Some(r) => r.map_err(crate::EngineError::Storage),
                None => Ok(Vec::new()),
            };
        }
        self.ensure_mem_index()?;
        match self.mem_index().as_ref() {
            Some(ix) => vibenote_storage::search::pdf_blobs_without_text(ix.conn()).map_err(storage),
            None => Ok(Vec::new()),
        }
    }

    fn rows(
        &self,
        q: &(String, String),
        limit: usize,
        only: Option<NoteId>,
    ) -> Result<
        (
            Vec<vibenote_storage::search::SearchRow>,
            Vec<vibenote_storage::search::LinkRow>,
        ),
        crate::EngineError,
    > {
        if self.persistent() {
            let r = self
                .reader
                .with(|st| Ok((st.search(q, limit, only)?, st.search_links(only)?)));
            return match r {
                Some(r) => r.map_err(crate::EngineError::Storage),
                None => Ok((Vec::new(), Vec::new())),
            };
        }
        self.ensure_mem_index()?;
        let g = self.mem_index();
        let Some(ix) = g.as_ref() else {
            return Ok((Vec::new(), Vec::new()));
        };
        let rows = vibenote_storage::search::query(ix.conn(), q, limit, only).map_err(storage)?;
        let links = vibenote_storage::search::links(ix.conn(), only).map_err(storage)?;
        Ok((rows, links))
    }

    /// Every row of typed text and recognised handwriting (the finder, R-VIM-005).
    pub(crate) fn written_rows(
        &self,
        limit: usize,
    ) -> Result<Vec<vibenote_storage::search::SearchRow>, crate::EngineError> {
        if self.persistent() {
            return match self.reader.with(|st| st.written(limit)) {
                Some(r) => r.map_err(crate::EngineError::Storage),
                None => Ok(Vec::new()),
            };
        }
        self.ensure_mem_index()?;
        match self.mem_index().as_ref() {
            Some(ix) => vibenote_storage::search::written(ix.conn(), limit).map_err(storage),
            None => Ok(Vec::new()),
        }
    }

    /// Searches everything (R-SEARCH-001): names, then content grouped by note; trashed items
    /// are left out. The engine brings storage in sync before (see `Engine::prepare_search`).
    pub(crate) fn search(
        &mut self,
        text: &str,
        limit: usize,
        only: Option<NoteId>,
    ) -> Result<Vec<Hit>, crate::EngineError> {
        let Some(q) = query_forms(text) else {
            return Ok(Vec::new());
        };
        let limit = limit.min(MAX_RESULTS);
        let (rows, links) = self.rows(&q, limit.saturating_mul(2), only)?;
        let ws = &self.ws;
        let live = |n: NoteId| ws.notes.get(&n).is_some_and(|x| !crate::quickopen::trashed(ws, x));
        let mut hits = Vec::new();
        let name_hit = |source, note: Option<u64>, folder: Option<u64>, name: &str| Hit {
            source,
            note,
            folder,
            object: None,
            page: None,
            region: None,
            snippet: name.to_string(),
        };
        if only.is_none() {
            for f in ws.folders.values().filter(|f| !ws.folder_in_trash(f.id)) {
                let (a, b) = forms(&f.name);
                if matches(&q, &a, &b) {
                    hits.push(name_hit(Source::Folder, None, Some(f.id.0), &f.name));
                }
            }
            for n in ws.notes.values().filter(|n| live(n.id)) {
                let (a, b) = forms(&n.name);
                if matches(&q, &a, &b) {
                    hits.push(name_hit(Source::Note, Some(n.id.0), None, &n.name));
                }
            }
        }
        for r in rows.into_iter().filter(|r| live(r.note)) {
            hits.push(Hit {
                source: source_of(&r.source),
                note: Some(r.note.0),
                folder: None,
                object: Some(r.object.0),
                page: r.page,
                region: region(r.region),
                snippet: snippet(&r.raw, &q),
            });
        }
        for l in links.into_iter().filter(|l| live(l.note)) {
            let label = self.link_label(&l.target);
            let (a, b) = forms(&label);
            if matches(&q, &a, &b) {
                hits.push(Hit {
                    source: Source::Link,
                    note: Some(l.note.0),
                    folder: None,
                    object: Some(l.object.0),
                    page: None,
                    region: region(l.region),
                    snippet: label,
                });
            }
        }
        // Grouped by note (names first within a note), stable otherwise.
        hits.sort_by_key(|h| (h.note.is_some(), h.note, h.source, h.object, h.page));
        hits.truncate(limit);
        Ok(hits)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn umlauts_match_in_both_forms_and_case_and_accents_fold() {
        let (a, b) = forms("Größe Übung Café");
        assert_eq!(a, "groesse uebung cafe");
        assert_eq!(b, "grosse ubung cafe");
        for q in ["groesse", "GRÖSSE", "uebung", "Ubung", "größe", "CAFÉ", "cafe"] {
            assert!(matches(&query_forms(q).unwrap(), &a, &b), "{q}");
        }
        assert!(!matches(&query_forms("grösser").unwrap(), &a, &b));
        assert!(query_forms("   ").is_none());
        assert_eq!(forms("a\n\n  b").0, "a b");
    }

    #[test]
    fn snippets_show_the_match() {
        let raw = format!("{} Zebrafisch {}", "vorher ".repeat(20), "nachher ".repeat(20));
        let s = snippet(&raw, &query_forms("zebra").unwrap());
        assert!(
            s.contains("Zebrafisch") && s.starts_with('…') && s.ends_with('…'),
            "{s}"
        );
    }
}
