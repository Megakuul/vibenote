//! Change journal of one canvas (M3.1b, design `plan/design-m3-rendering.md` §5.2): which
//! objects were inserted, replaced or removed since a revision. Kept by the owner of the canvas
//! (the engine) and read by consumers that cache derived data (the renderer's GPU chunks), so
//! they never walk the whole canvas to find what changed. Bounded: a reader that falls too far
//! behind gets [`Since::TooOld`] and resynchronises.

use crate::ObjectId;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct ChangeLog {
    /// Identity of the canvas instance; a new epoch means "start over" (note recreated).
    epoch: u64,
    rev: u64,
    /// Revision before the oldest entry still held.
    floor: u64,
    entries: VecDeque<(u64, ObjectId)>,
}

/// What changed since a revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Since {
    Current,
    /// Changed objects (each once, in first-change order); look them up in the canvas: present
    /// means inserted or replaced, absent means removed.
    Changed(Vec<ObjectId>),
    /// The log no longer reaches back that far.
    TooOld,
}

impl ChangeLog {
    /// Entries kept (older ones are dropped; readers behind them get `TooOld`).
    pub const CAPACITY: usize = 65_536;

    pub fn new(epoch: u64) -> Self {
        ChangeLog {
            epoch,
            rev: 0,
            floor: 0,
            entries: VecDeque::new(),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Records that object `id` was inserted, replaced or removed.
    pub fn record(&mut self, id: ObjectId) {
        self.rev = self.rev.saturating_add(1);
        self.entries.push_back((self.rev, id));
        while self.entries.len() > Self::CAPACITY {
            if let Some((r, _)) = self.entries.pop_front() {
                self.floor = r;
            }
        }
    }

    pub fn since(&self, rev: u64) -> Since {
        if rev == self.rev {
            return Since::Current;
        }
        if rev < self.floor || rev > self.rev {
            return Since::TooOld;
        }
        let mut seen = std::collections::HashSet::new();
        // Entries are in revision order: find the first newer one by binary search.
        let from = self.entries.partition_point(|(r, _)| *r <= rev);
        let ids = self
            .entries
            .range(from..)
            .filter(|(_, id)| seen.insert(*id))
            .map(|(_, id)| *id)
            .collect();
        Since::Changed(ids)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn reports_changes_once_and_overflow_as_too_old() {
        let mut l = ChangeLog::new(7);
        assert_eq!(l.since(0), Since::Current);
        l.record(ObjectId(1));
        l.record(ObjectId(2));
        l.record(ObjectId(1));
        assert_eq!(l.since(0), Since::Changed(vec![ObjectId(1), ObjectId(2)]));
        assert_eq!(l.since(2), Since::Changed(vec![ObjectId(1)]));
        assert_eq!(l.since(3), Since::Current);
        assert_eq!(l.since(4), Since::TooOld, "a revision from the future");
        for k in 0..ChangeLog::CAPACITY as u64 {
            l.record(ObjectId(100 + k));
        }
        assert_eq!(l.since(0), Since::TooOld);
        assert!(matches!(l.since(l.rev() - 1), Since::Changed(v) if v.len() == 1));
        assert_eq!(l.epoch(), 7);
    }
}
