//! Tile cache bookkeeping (M3.3, ADR 0003 decision 2, design `plan/design-m3-rendering.md`
//! §4.2). Zoomed out (zoom < 0.25 or more than 500k visible instances), the committed ink is
//! composited from cached 256 px tiles rendered at power-of-two zooms. A tile is stale when any
//! chunk under it changed (chunk versions); stale or missing tiles are refilled within a
//! per-frame budget, nearest the viewport centre first, and covered meanwhile by the nearest
//! coarser cached tile. The coarsest levels that hold the whole content in a few tiles are kept
//! filled, so there is always something to show. New tiles fade in over a few frames.
//! Live ink is never tiled.

use std::collections::HashMap;
use vibenote_geom::{Point, Rect};

/// Tile edge in pixels.
pub(crate) const TILE: u32 = 256;
/// Atlas: 4096² RGBA8 layers of 16 × 16 tiles.
pub(crate) const ATLAS: u32 = 4096;
pub(crate) const PER_ROW: u32 = ATLAS / TILE;
pub(crate) const LAYERS: u32 = 2;
pub(crate) const SLOTS: u32 = PER_ROW * PER_ROW * LAYERS;
/// Frames a new tile takes to fade in over its fallback.
pub(crate) const FADE_FRAMES: u8 = 8;
/// Tiles at the overview levels (the whole content fits in at most this many per axis).
const OVERVIEW_TILES: i64 = 2;
/// Levels: tile zoom 2^level.
pub(crate) const LEVEL_MIN: i32 = -14;
pub(crate) const LEVEL_MAX: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Key {
    pub level: i32,
    pub tx: i64,
    pub ty: i64,
}

#[allow(clippy::cast_possible_truncation)] // clamped to ±1e15
fn idx(v: f64) -> i64 {
    v.floor().clamp(-1.0e15, 1.0e15) as i64
}

#[allow(clippy::cast_precision_loss)] // |i| ≤ 1e15: exact
fn f(i: i64) -> f64 {
    i as f64
}

impl Key {
    /// Tile zoom (pixels per canvas unit).
    pub fn zoom(self) -> f64 {
        2f64.powi(self.level)
    }
    /// Canvas size of a tile edge.
    pub fn span(self) -> f64 {
        f64::from(TILE) / self.zoom()
    }
    pub fn rect(self) -> Rect {
        let s = self.span();
        Rect {
            min: Point::new(f(self.tx) * s, f(self.ty) * s),
            max: Point::new(f(self.tx) * s + s, f(self.ty) * s + s),
        }
    }
    /// The tile at `level` containing this tile's top-left corner.
    pub fn at_level(self, level: i32) -> Key {
        let s = Key { level, tx: 0, ty: 0 }.span();
        let r = self.rect();
        Key {
            level,
            tx: idx(r.min.x / s),
            ty: idx(r.min.y / s),
        }
    }
}

/// Level for drawing at `zoom`: tiles are shown at scale 0.5–1 (minified, never magnified).
#[allow(clippy::cast_possible_truncation)] // clamped to the level range
pub(crate) fn level_for(zoom: f64) -> i32 {
    zoom.log2().ceil().clamp(f64::from(LEVEL_MIN), f64::from(LEVEL_MAX)) as i32
}

/// Tiles covering `view` at `level`.
pub(crate) fn tiles_in(view: &Rect, level: i32) -> Vec<Key> {
    let s = Key { level, tx: 0, ty: 0 }.span();
    let (x0, x1) = (idx(view.min.x / s), idx(view.max.x / s));
    let (y0, y1) = (idx(view.min.y / s), idx(view.max.y / s));
    let mut v = Vec::new();
    // Bounded: the view is at most 8192 px wide and tiles are ≥ 128 px on screen.
    for ty in y0..=y1.min(y0.saturating_add(128)) {
        for tx in x0..=x1.min(x0.saturating_add(128)) {
            v.push(Key { level, tx, ty });
        }
    }
    v
}

/// The coarsest level at which `content` spans at most OVERVIEW_TILES tiles per axis.
pub(crate) fn overview_level(content: &Rect) -> i32 {
    let mut level = LEVEL_MAX;
    while level > LEVEL_MIN {
        let s = Key { level, tx: 0, ty: 0 }.span();
        let n = |a: f64, b: f64| idx(b / s).saturating_sub(idx(a / s)).saturating_add(1);
        if n(content.min.x, content.max.x) <= OVERVIEW_TILES && n(content.min.y, content.max.y) <= OVERVIEW_TILES {
            return level;
        }
        level = level.saturating_sub(1);
    }
    LEVEL_MIN
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Tile {
    /// Atlas slot (`None`: no ink under the tile).
    pub slot: Option<u32>,
    /// Hash of the versions of the chunks under it when filled.
    pub sig: u64,
    pub last_used: u64,
    /// Frames of fade-in left.
    pub fade: u8,
}

#[derive(Default)]
pub(crate) struct TileCache {
    pub tiles: HashMap<Key, Tile>,
    free: Vec<u32>,
    used: u32,
}

impl TileCache {
    pub fn clear(&mut self) {
        *self = TileCache::default();
    }

    /// A free slot, evicting the least recently used tile not in `keep` if needed.
    pub fn slot(&mut self, keep: &[Key]) -> Option<u32> {
        if let Some(s) = self.free.pop() {
            return Some(s);
        }
        if self.used < SLOTS {
            self.used = self.used.saturating_add(1);
            return Some(self.used.saturating_sub(1));
        }
        let victim = self
            .tiles
            .iter()
            .filter(|(k, t)| t.slot.is_some() && !keep.contains(k))
            .min_by_key(|(k, t)| (t.last_used, **k))
            .map(|(k, _)| *k)?;
        self.tiles.remove(&victim).and_then(|t| t.slot)
    }

    /// Frees the slot of a tile that turned out empty or is dropped.
    pub fn release(&mut self, slot: u32) {
        self.free.push(slot);
    }
}

/// Atlas position of a slot: layer and top-left pixel.
pub(crate) fn slot_pos(slot: u32) -> (u32, u32, u32) {
    const PER_LAYER: u32 = PER_ROW * PER_ROW;
    let i = slot % PER_LAYER;
    (
        slot / PER_LAYER,
        (i % PER_ROW).saturating_mul(TILE),
        (i / PER_ROW).saturating_mul(TILE),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn levels_minify_and_tiles_nest() {
        for z in [0.24, 0.1, 0.037, 0.02, 0.01, 0.0001] {
            let l = level_for(z);
            let k = Key { level: l, tx: 0, ty: 0 };
            let scale = z / k.zoom();
            assert!((0.5..=1.0).contains(&scale) || l == LEVEL_MIN, "{z}: scale {scale}");
        }
        let k = Key {
            level: -3,
            tx: 5,
            ty: -7,
        };
        let c = k.at_level(-5);
        let (r, rc) = (k.rect(), c.rect());
        assert!(rc.min.x <= r.min.x && rc.max.x >= r.max.x && rc.min.y <= r.min.y && rc.max.y >= r.max.y);
    }

    #[test]
    fn slots_are_recycled_least_recently_used_first() {
        let mut c = TileCache::default();
        for i in 0..SLOTS {
            let s = c.slot(&[]).unwrap();
            c.tiles.insert(
                Key {
                    level: 0,
                    tx: i64::from(i),
                    ty: 0,
                },
                Tile {
                    slot: Some(s),
                    sig: 0,
                    last_used: u64::from(i),
                    fade: 0,
                },
            );
        }
        let keep = [Key { level: 0, tx: 0, ty: 0 }];
        let s = c.slot(&keep).unwrap();
        assert!(
            !c.tiles.contains_key(&Key { level: 0, tx: 1, ty: 0 }),
            "oldest not kept is evicted"
        );
        assert!(c.tiles.contains_key(&keep[0]));
        assert!(s < SLOTS);
        let (layer, x, y) = slot_pos(SLOTS - 1);
        assert_eq!((layer, x, y), (LAYERS - 1, ATLAS - TILE, ATLAS - TILE));
    }

    #[test]
    fn overview_level_fits_the_content() {
        let content = Rect {
            min: Point::new(0.0, 0.0),
            max: Point::new(22_400.0, 30_800.0),
        };
        let l = overview_level(&content);
        let s = Key { level: l, tx: 0, ty: 0 }.span();
        assert!(30_800.0 / s <= 2.0);
        assert!(30_800.0 / (s / 2.0) > 2.0 - 1e-9, "the next finer level would not fit");
    }
}
