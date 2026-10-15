//! Spatial lookup for hit-testing (erasers, later lasso): a grid of object boxes per note,
//! kept current through the note's change journal (M3.1b), so finding candidates near a path
//! never walks the whole canvas.

use std::collections::{BTreeSet, HashMap};
use vibenote_geom::Rect;
use vibenote_model::{Canvas, CanvasObject, ChangeLog, ObjectId, Since};

/// Grid cell size in canvas units.
const CELL: f64 = 1024.0;
/// Objects covering more cells than this are kept in a list that every query returns.
const MAX_CELLS: i64 = 1024;

type Key = (i64, i64);

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HitIndex {
    synced: Option<(u64, u64)>,
    grid: HashMap<Key, BTreeSet<ObjectId>>,
    boxes: HashMap<ObjectId, (Rect, Vec<Key>)>,
    huge: BTreeSet<ObjectId>,
}

#[allow(clippy::cast_possible_truncation)] // clamped to ±1e15 first
fn key(v: f64) -> i64 {
    (v / CELL).floor().clamp(-1.0e15, 1.0e15) as i64
}

/// Box of everything an object draws (strokes: their full ink extent).
fn bounds(o: &CanvasObject) -> Option<Rect> {
    o.kind.bounds()
}

impl HitIndex {
    fn insert(&mut self, o: &CanvasObject) {
        let Some(b) = bounds(o) else { return };
        let (x0, x1, y0, y1) = (key(b.min.x), key(b.max.x), key(b.min.y), key(b.max.y));
        let span = x1
            .saturating_sub(x0)
            .saturating_add(1)
            .saturating_mul(y1.saturating_sub(y0).saturating_add(1));
        let mut keys = Vec::new();
        if span > MAX_CELLS {
            self.huge.insert(o.id);
        } else {
            for x in x0..=x1 {
                for y in y0..=y1 {
                    self.grid.entry((x, y)).or_default().insert(o.id);
                    keys.push((x, y));
                }
            }
        }
        self.boxes.insert(o.id, (b, keys));
    }

    fn remove(&mut self, id: ObjectId) {
        if let Some((_, keys)) = self.boxes.remove(&id) {
            for k in keys {
                if let Some(set) = self.grid.get_mut(&k) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.grid.remove(&k);
                    }
                }
            }
        }
        self.huge.remove(&id);
    }

    fn rebuild(&mut self, canvas: &Canvas) {
        *self = HitIndex::default();
        for o in canvas.objects.values() {
            self.insert(o);
        }
    }

    /// Brings the index up to date (O(changes) with a journal; a full rebuild without one).
    pub fn sync(&mut self, canvas: &Canvas, log: Option<&ChangeLog>) {
        let Some(log) = log else {
            self.rebuild(canvas);
            return;
        };
        match self.synced {
            Some((epoch, rev)) if epoch == log.epoch() => match log.since(rev) {
                Since::Current => {}
                Since::Changed(ids) => {
                    for id in ids {
                        self.remove(id);
                        if let Some(o) = canvas.objects.get(&id) {
                            self.insert(o);
                        }
                    }
                }
                Since::TooOld => self.rebuild(canvas),
            },
            _ => self.rebuild(canvas),
        }
        self.synced = Some((log.epoch(), log.rev()));
    }

    /// Objects whose boxes come within `radius` of a polyline (ascending ids). Segment boxes
    /// are grouped by grid cell; each object in a touched cell is tested once, against that
    /// cell's segments only, so a long path over a dense area stays cheap.
    pub fn query_path(&self, path: &[(f64, f64)], radius: f64) -> Vec<ObjectId> {
        let first = path.first().map(|&p| (p, p));
        let segs = first
            .into_iter()
            .chain(path.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))));
        let meets =
            |a: &Rect, b: &Rect| a.min.x <= b.max.x && a.max.x >= b.min.x && a.min.y <= b.max.y && a.max.y >= b.min.y;
        let mut out = BTreeSet::new();
        let mut cells: HashMap<Key, Vec<Rect>> = HashMap::new();
        let mut boxes = Vec::new();
        for (a, b) in segs {
            let r = Rect {
                min: vibenote_geom::Point::new(a.0.min(b.0) - radius, a.1.min(b.1) - radius),
                max: vibenote_geom::Point::new(a.0.max(b.0) + radius, a.1.max(b.1) + radius),
            };
            let (x0, x1, y0, y1) = (key(r.min.x), key(r.max.x), key(r.min.y), key(r.max.y));
            let span = x1
                .saturating_sub(x0)
                .saturating_add(1)
                .saturating_mul(y1.saturating_sub(y0).saturating_add(1));
            if span > MAX_CELLS {
                out.extend(self.query(&r)); // a huge segment (or radius): plain box query
            } else {
                for x in x0..=x1 {
                    for y in y0..=y1 {
                        cells.entry((x, y)).or_default().push(r);
                    }
                }
            }
            boxes.push(r);
        }
        for (cell, segs) in &cells {
            for id in self.grid.get(cell).into_iter().flatten() {
                if !out.contains(id)
                    && self
                        .boxes
                        .get(id)
                        .is_some_and(|(b, _)| segs.iter().any(|s| meets(b, s)))
                {
                    out.insert(*id);
                }
            }
        }
        for id in &self.huge {
            if self
                .boxes
                .get(id)
                .is_some_and(|(b, _)| boxes.iter().any(|s| meets(b, s)))
            {
                out.insert(*id);
            }
        }
        out.into_iter().collect()
    }

    /// Objects whose boxes meet `area` (ascending ids).
    pub fn query(&self, area: &Rect) -> Vec<ObjectId> {
        let mut out: BTreeSet<ObjectId> = self.huge.clone();
        let (x0, x1, y0, y1) = (key(area.min.x), key(area.max.x), key(area.min.y), key(area.max.y));
        let span = x1
            .saturating_sub(x0)
            .saturating_add(1)
            .saturating_mul(y1.saturating_sub(y0).saturating_add(1));
        if span > self.grid.len().try_into().unwrap_or(i64::MAX) {
            // A query larger than the occupied grid: walk the occupied cells instead.
            for (k, ids) in &self.grid {
                if (x0..=x1).contains(&k.0) && (y0..=y1).contains(&k.1) {
                    out.extend(ids);
                }
            }
        } else {
            for x in x0..=x1 {
                for y in y0..=y1 {
                    if let Some(ids) = self.grid.get(&(x, y)) {
                        out.extend(ids);
                    }
                }
            }
        }
        out.into_iter()
            .filter(|id| {
                self.boxes.get(id).is_some_and(|(b, _)| {
                    b.min.x <= area.max.x && b.max.x >= area.min.x && b.min.y <= area.max.y && b.max.y >= area.min.y
                })
            })
            .collect()
    }
}
