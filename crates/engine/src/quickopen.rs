//! Quick-open (R-ORG-003, R-PERF-008): fuzzy note search by name over 100k notes within one
//! keystroke budget. The index keeps case-folded names; a query that extends the previous one
//! (the usual typing case) only re-checks the previous matches, since a subsequence match of
//! the longer query implies one of the shorter.
//!
//! Scoring (higher is better): a contiguous substring beats a scattered subsequence; matches at
//! the start of the name or of a word, consecutive runs and short names score higher; gaps cost.
//! Ties go to recently opened notes, then by name and id, so results are deterministic.

use vibenote_model::{Note, NoteId, Workspace};

/// Most results per query.
pub const MAX_LIMIT: usize = 200;
/// Longest query considered (characters).
pub const MAX_QUERY: usize = 256;
/// Notes remembered as recently opened.
pub const RECENT: usize = 50;

#[derive(Clone, Default)]
pub(crate) struct QuickIndex {
    /// Revision of the workspace's notes the index was built from.
    rev: Option<u64>,
    names: Vec<(NoteId, Vec<char>)>,
    /// Previous query and the indices into `names` it matched.
    last: Option<(Vec<char>, Vec<usize>)>,
}

// A derived cache: never part of a session's identity.
impl PartialEq for QuickIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for QuickIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "QuickIndex({} names)", self.names.len())
    }
}

/// A note in the trash, directly or through a trashed folder.
pub(crate) fn trashed(ws: &Workspace, n: &Note) -> bool {
    ws.trashed_notes.contains(&n.id) || n.folder.is_some_and(|f| ws.folder_in_trash(f))
}

pub(crate) fn fold(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

fn word_start(name: &[char], i: usize) -> bool {
    i == 0
        || i.checked_sub(1)
            .and_then(|j| name.get(j))
            .is_some_and(|c| !c.is_alphanumeric())
}

/// Score of `q` in `name` (both case-folded), or `None` if `q` is not a subsequence.
pub(crate) fn score(q: &[char], name: &[char]) -> Option<i64> {
    if q.is_empty() {
        return Some(0);
    }
    let len_penalty = i64::try_from(name.len()).unwrap_or(i64::MAX).min(1000);
    // Contiguous substring: best at the start, then at a word start.
    if let Some(pos) = name.windows(q.len()).position(|w| w == q) {
        let p = i64::try_from(pos).unwrap_or(1000).min(1000);
        let bonus = if pos == 0 {
            3000
        } else if word_start(name, pos) {
            2000
        } else {
            1000
        };
        return Some(
            10_000i64
                .saturating_add(bonus)
                .saturating_sub(p.saturating_mul(4))
                .saturating_sub(len_penalty),
        );
    }
    // Greedy subsequence, preferring word starts: each query char takes the next word-start
    // occurrence before the next plain one when both exist before the following query char.
    let mut s: i64 = 0;
    let mut at = 0usize;
    let mut prev: Option<usize> = None;
    for &c in q {
        let rest = name.get(at..)?;
        let first = rest.iter().position(|&x| x == c)?.checked_add(at)?;
        let ws = rest
            .iter()
            .enumerate()
            .filter(|&(k, &x)| x == c && word_start(name, k.saturating_add(at)))
            .map(|(k, _)| k.saturating_add(at))
            .next();
        let i = match ws {
            Some(w) if w.saturating_sub(first) <= 8 => w,
            _ => first,
        };
        if word_start(name, i) {
            s = s.saturating_add(60);
        }
        match prev {
            Some(p) if p.checked_add(1) == Some(i) => s = s.saturating_add(40),
            Some(p) => {
                let gap = i64::try_from(i.saturating_sub(p)).unwrap_or(1000).min(1000);
                s = s.saturating_sub(gap.saturating_mul(2));
            }
            None => s = s.saturating_sub(i64::try_from(i).unwrap_or(1000).min(1000)),
        }
        prev = Some(i);
        at = i.checked_add(1)?;
    }
    Some(s.saturating_sub(len_penalty))
}

impl QuickIndex {
    fn sync(&mut self, ws: &Workspace, rev: u64) {
        if self.rev == Some(rev) {
            return;
        }
        self.names = ws
            .notes
            .values()
            .filter(|n| !trashed(ws, n))
            .map(|n| (n.id, fold(&n.name)))
            .collect();
        self.rev = Some(rev);
        self.last = None;
    }

    /// The best `limit` notes for `query`: (note, score), best first. `recent` (most recent
    /// first) breaks ties.
    pub(crate) fn query(
        &mut self,
        ws: &Workspace,
        rev: u64,
        query: &str,
        limit: usize,
        recent: &[NoteId],
    ) -> Vec<(NoteId, i64)> {
        self.sync(ws, rev);
        let q: Vec<char> = fold(query).into_iter().take(MAX_QUERY).collect();
        let candidates: Vec<usize> = match &self.last {
            Some((lq, hits)) if q.starts_with(lq) => hits.clone(),
            _ => (0..self.names.len()).collect(),
        };
        let mut scored: Vec<(usize, i64)> = candidates
            .into_iter()
            .filter_map(|i| {
                let (_, name) = self.names.get(i)?;
                score(&q, name).map(|s| (i, s))
            })
            .collect();
        self.last = Some((q, scored.iter().map(|&(i, _)| i).collect()));
        let rank = |id: NoteId| recent.iter().position(|r| *r == id).unwrap_or(usize::MAX);
        let key = |&(i, s): &(usize, i64)| {
            let (id, name) = self
                .names
                .get(i)
                .map_or((NoteId(0), &[][..]), |(id, n)| (*id, n.as_slice()));
            (std::cmp::Reverse(s), rank(id), name, id)
        };
        let limit = limit.min(MAX_LIMIT);
        if scored.len() > limit && limit > 0 {
            scored.select_nth_unstable_by(limit.saturating_sub(1), |a, b| key(a).cmp(&key(b)));
            scored.truncate(limit);
        }
        scored.sort_by(|a, b| key(a).cmp(&key(b)));
        scored.truncate(limit);
        scored
            .into_iter()
            .filter_map(|(i, s)| self.names.get(i).map(|(id, _)| (*id, s)))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn f(s: &str) -> Vec<char> {
        fold(s)
    }

    #[test]
    fn substrings_beat_scattered_matches_and_prefixes_beat_the_middle() {
        let q = f("not");
        let prefix = score(&q, &f("Notiz 1")).unwrap();
        let word = score(&q, &f("Meine Notiz")).unwrap();
        let middle = score(&q, &f("Banknote")).unwrap();
        let scattered = score(&q, &f("Neues Objekt Test")).unwrap();
        assert!(
            prefix > word && word > middle && middle > scattered,
            "{prefix} {word} {middle} {scattered}"
        );
        assert!(score(&q, &f("Tonne")).is_none());
        assert_eq!(score(&[], &f("x")), Some(0));
    }

    #[test]
    fn case_and_umlauts_fold() {
        assert!(score(&f("ÜBUNG"), &f("Übungsblatt")).is_some());
        assert!(score(&f("vl"), &f("Vorlesung Lineare Algebra")).is_some());
    }

    #[test]
    fn word_starts_are_preferred_in_subsequences() {
        let q = f("sa");
        // "Seminar Analysis" (both at word starts) beats "Skizzen Haus" ('a' inside a word).
        assert!(score(&q, &f("Seminar Analysis")).unwrap() > score(&q, &f("Skizzen Haus")).unwrap());
    }
}
