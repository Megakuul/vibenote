//! CPU side of the compact ink format (ADR 0011, design `plan/design-m3-rendering.md` §1).
//!
//! A stroke is split by 2,048-unit cells into **shares**: for each cell, every segment whose
//! drawn footprint (plus a 32-unit margin) can reach the cell, clipped to the cell's box grown by
//! that footprint. Each point is 8 bytes: i16 offsets on the model's 1/1024 grid from a per-run
//! origin (a run starts every 32 units), an 8-bit radius relative to the stroke's largest radius
//! and flags. Decoding reproduces the quantised coordinates exactly; drawing every cell scissored
//! to itself reproduces the per-segment renderer pixel for pixel in z-order (design §3.1).
//!
//! Instance `i` of a share is the segment from point `i - 1` to point `i`:
//! - `START`: the stroke's first sample, drawn as a dot (predecessor = itself);
//! - `CONT`: only the predecessor of the next point (a piece re-enters the cell); no instance;
//! - `SEAM`: the segment is owned by another cell (skipped when drawing unscissored).

use crate::RenderError;
use vibenote_geom::{Point as P, Rect};
use vibenote_ink::tool_width;
use vibenote_model::{Stroke, Tool};

/// Cell (chunk) size in canvas units.
pub(crate) const CELL: f64 = 2048.0;
/// Extra reach of a cell beyond the stroke radius (covers anti-aliasing and the minimum radius
/// down to zoom ≈ 0.027; below that cells are drawn unscissored).
pub(crate) const MARGIN: f64 = 32.0;
/// Model coordinate grid (`vibenote_model::grid::XY`).
const Q: f64 = 1024.0;
/// Largest offset from a run origin, in grid units (i16).
const RUN_SPAN: i64 = 32_767;
/// Largest radius used for cell membership (a hostile width must not blow up chunk-local
/// coordinates; valid strokes have radius < 5,000).
const R_CAP: f32 = 8_192.0;
/// Chunk-local coordinates stay below this many grid units (exact in f32).
const LOCAL_MAX: i64 = 1 << 24;
/// Bounds for hostile strokes.
pub(crate) const MAX_SHARE_POINTS: usize = (1 << 20) - 1;
pub(crate) const MAX_SHARE_RUNS: usize = 65_535;
const MAX_PAIRS: usize = 8_000_000;
const MAX_CELLS: usize = 65_536;

pub(crate) const F_START: u8 = 1;
pub(crate) const F_CONT: u8 = 2;
pub(crate) const F_SEAM: u8 = 4;

/// Highlighter alpha (translucent ink; the layer pass keeps overlaps from darkening).
const HIGHLIGHTER_ALPHA: u32 = 0x59;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Cell {
    pub x: i64,
    pub y: i64,
}

#[allow(clippy::cast_possible_truncation)] // floor of a value bounded by the caller's checks
fn cell_index(v: f64) -> i64 {
    (v / CELL).floor().clamp(-1.0e15, 1.0e15) as i64
}

#[allow(clippy::cast_precision_loss)] // |index| ≤ 1e15 / 2048: exact in f64
fn idx_f64(i: i64) -> f64 {
    i as f64
}

impl Cell {
    pub fn of(x: f64, y: f64) -> Cell {
        Cell {
            x: cell_index(x),
            y: cell_index(y),
        }
    }
    /// Canvas coordinate of the cell's top-left corner (exact).
    pub fn origin(self) -> (f64, f64) {
        (idx_f64(self.x) * CELL, idx_f64(self.y) * CELL)
    }
    pub fn rect(self) -> Rect {
        let (x, y) = self.origin();
        Rect {
            min: P::new(x, y),
            max: P::new(x + CELL, y + CELL),
        }
    }
    /// Origin in grid units.
    fn origin_q(self) -> Option<(i64, i64)> {
        Some((self.x.checked_mul(2048 * 1024)?, self.y.checked_mul(2048 * 1024)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Point {
    pub dx: i16,
    pub dy: i16,
    /// Share-local run index.
    pub run: u32,
    pub r: u8,
    pub flags: u8,
}

/// Run origin, chunk-local canvas units (exact multiples of 1/1024).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Share {
    pub cell: Cell,
    pub runs: Vec<Run>,
    pub points: Vec<Point>,
    /// Canvas-space bounds of what this share draws (for culling).
    pub bounds: Rect,
    // Encoder state.
    last_seg: Option<usize>,
    last_end_clipped: bool,
    run_q: (i64, i64),
}

impl Share {
    fn new(cell: Cell) -> Share {
        Share {
            cell,
            runs: Vec::new(),
            points: Vec::new(),
            bounds: Rect::EMPTY,
            last_seg: None,
            last_end_clipped: false,
            run_q: (0, 0),
        }
    }
}

/// A stroke in the compact format.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Encoded {
    /// 0xRRGGBBAA, highlighter alpha applied.
    pub color: u32,
    /// Largest radius in canvas units (radii are stored relative to it).
    pub r_max: f32,
    pub highlighter: bool,
    /// Shares by cell (a cell can have several when a share hits the per-share limits).
    pub shares: Vec<Share>,
}

#[derive(Clone, Copy)]
struct G {
    /// Grid coordinates.
    qx: i64,
    qy: i64,
    /// Radius, canvas units.
    r: f32,
}

#[allow(clippy::cast_possible_truncation)] // range-checked first
fn to_q(v: f64) -> Option<i64> {
    let k = (v * Q).round();
    (k.is_finite() && k.abs() < 9.0e15).then_some(k as i64)
}

#[allow(clippy::cast_precision_loss)] // |q| < 9e15 < 2^53: exact
fn from_q(q: i64) -> f64 {
    q as f64 / Q
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255
fn radius_byte(r: f32, r_max: f32) -> u8 {
    if r_max > 0.0 {
        (r / r_max * 255.0).round().clamp(0.0, 255.0) as u8
    } else {
        0
    }
}

#[allow(clippy::cast_precision_loss)] // |local| < 2^24: exact in f32
fn local_f32(q: i64) -> f32 {
    q as f32 / 1024.0
}

/// Clips segment a→b to `r` (Liang–Barsky). Returns the parameter range, if any.
fn clip(a: (f64, f64), b: (f64, f64), r: &Rect) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - r.min.x),
        (dx, r.max.x - a.0),
        (-dy, a.1 - r.min.y),
        (dy, r.max.y - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

/// Cells whose grown box (by `w`) the segment a→b can touch, column by column. Bounded by the
/// caller's pair limit.
fn cells_of(a: (f64, f64), b: (f64, f64), w: f64, out: &mut Vec<Cell>) {
    out.clear();
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (cx0, cx1) = (cell_index(x0 - w), cell_index(x1 + w));
    let mut cx = cx0;
    while cx <= cx1 {
        let (sx0, sx1) = (idx_f64(cx) * CELL - w, idx_f64(cx).mul_add(CELL, CELL) + w);
        // y extent of the segment within this column's x-slab
        let (lo, hi) = if (b.0 - a.0).abs() < 1e-12 {
            (a.1.min(b.1), a.1.max(b.1))
        } else {
            let ya = a.1 + (b.1 - a.1) * ((sx0.max(x0) - a.0) / (b.0 - a.0));
            let yb = a.1 + (b.1 - a.1) * ((sx1.min(x1) - a.0) / (b.0 - a.0));
            (ya.min(yb), ya.max(yb))
        };
        let (cy0, cy1) = (cell_index(lo - w), cell_index(hi + w));
        let mut cy = cy0;
        while cy <= cy1 {
            out.push(Cell { x: cx, y: cy });
            if out.len() > MAX_CELLS {
                return;
            }
            cy = cy.saturating_add(1);
        }
        cx = cx.saturating_add(1);
    }
}

fn too_big(what: &str) -> RenderError {
    RenderError::Viewport(format!("stroke too large to draw: {what}"))
}

/// Encodes a stroke. Errors (never panics) for strokes beyond the hostile-input bounds.
#[allow(clippy::cast_possible_truncation)] // clip parameter t ∈ [0, 1] to f32 for radius interpolation
pub(crate) fn encode(s: &Stroke) -> Result<Encoded, RenderError> {
    let (color, highlighter) = match s.tool {
        Tool::Highlighter => ((s.color.0 & 0xffff_ff00) | HIGHLIGHTER_ALPHA, true),
        Tool::Pen => (s.color.0, false),
    };
    let mut g = Vec::with_capacity(s.samples.len());
    let mut r_max = 0.0f32;
    for smp in &s.samples {
        let (Some(qx), Some(qy)) = (to_q(smp.x), to_q(smp.y)) else {
            return Err(too_big("coordinate out of range"));
        };
        let r = tool_width(s.tool, s.width, smp.pressure, smp.tilt_x, smp.tilt_y) * 0.5;
        let r = if r.is_finite() { r.clamp(0.0, R_CAP) } else { 0.0 };
        r_max = r_max.max(r);
        g.push(G { qx, qy, r });
    }
    let w = f64::from(r_max) + MARGIN;
    if let Some(e) = encode_inside_one_cell(&g, r_max, w, color, highlighter)? {
        return Ok(e);
    }
    let mut shares: Vec<Share> = Vec::new();
    // Index of the open (not full) share per cell.
    let mut open: std::collections::HashMap<Cell, usize> = std::collections::HashMap::new();
    let mut cells = Vec::new();
    let mut pairs = 0usize;
    let mut prev: Option<G> = None;
    for (j, b) in g.iter().copied().enumerate() {
        let a = prev.unwrap_or(b);
        prev = Some(b);
        let (pa, pb) = ((from_q(a.qx), from_q(a.qy)), (from_q(b.qx), from_q(b.qy)));
        let owner = Cell::of(pb.0, pb.1);
        cells_of(pa, pb, w, &mut cells);
        pairs = pairs.saturating_add(cells.len());
        if pairs > MAX_PAIRS || cells.len() > MAX_CELLS {
            return Err(too_big("too many cells"));
        }
        for &cell in &cells {
            let r = cell.rect();
            let grown = Rect {
                min: P::new(r.min.x - w, r.min.y - w),
                max: P::new(r.max.x + w, r.max.y + w),
            };
            let Some((t0, t1)) = clip(pa, pb, &grown) else {
                continue;
            };
            let at = |t: f64| -> Option<G> {
                if t <= 0.0 {
                    return Some(a);
                }
                if t >= 1.0 {
                    return Some(b);
                }
                Some(G {
                    qx: to_q(pa.0 + (pb.0 - pa.0) * t)?,
                    qy: to_q(pa.1 + (pb.1 - pa.1) * t)?,
                    r: a.r + (b.r - a.r) * t as f32,
                })
            };
            let (Some(ca), Some(cb)) = (at(t0), at(t1)) else {
                return Err(too_big("coordinate out of range"));
            };
            let idx = match open.get(&cell) {
                Some(&i) => i,
                None => {
                    if open.len() >= MAX_CELLS {
                        return Err(too_big("too many cells"));
                    }
                    shares.push(Share::new(cell));
                    let i = shares.len().saturating_sub(1);
                    open.insert(cell, i);
                    i
                }
            };
            let Some(sh) = shares.get_mut(idx) else {
                continue;
            };
            let cont = j > 0 && sh.last_seg == Some(j.saturating_sub(1)) && !sh.last_end_clipped && t0 <= 0.0;
            let seam = if cell == owner { 0 } else { F_SEAM };
            if !cont {
                if j == 0 {
                    push(sh, cb, r_max, F_START | seam, true)?;
                } else {
                    push(sh, ca, r_max, F_CONT, true)?;
                    push(sh, cb, r_max, seam, false)?;
                }
            } else {
                push(sh, cb, r_max, seam, false)?;
            }
            sh.last_seg = Some(j);
            sh.last_end_clipped = t1 < 1.0;
            let rr = f64::from(ca.r.max(cb.r));
            for p in [ca, cb] {
                let (x, y) = (from_q(p.qx), from_q(p.qy));
                sh.bounds.include(P::new(x - rr, y - rr));
                sh.bounds.include(P::new(x + rr, y + rr));
            }
            // A full share is closed; the next segment in this cell opens a new one and starts
            // with its predecessor (`CONT`).
            if sh.points.len() >= MAX_SHARE_POINTS.saturating_sub(2)
                || sh.runs.len() >= MAX_SHARE_RUNS.saturating_sub(2)
            {
                open.remove(&cell);
            }
        }
    }
    shares.sort_by_key(|s| s.cell);
    Ok(Encoded {
        color,
        r_max,
        highlighter,
        shares,
    })
}

/// The common case: the whole stroke (grown by its reach) lies inside one cell, so it is one
/// share without clipping. `None` if not (or if it would exceed the share limits).
fn encode_inside_one_cell(
    g: &[G],
    r_max: f32,
    w: f64,
    color: u32,
    highlighter: bool,
) -> Result<Option<Encoded>, RenderError> {
    let Some(first) = g.first() else {
        return Ok(Some(Encoded {
            color,
            r_max,
            highlighter,
            shares: Vec::new(),
        }));
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.qx, first.qy, first.qx, first.qy);
    for p in g {
        x0 = x0.min(p.qx);
        y0 = y0.min(p.qy);
        x1 = x1.max(p.qx);
        y1 = y1.max(p.qy);
    }
    let (fx0, fy0, fx1, fy1) = (from_q(x0) - w, from_q(y0) - w, from_q(x1) + w, from_q(y1) + w);
    let cell = Cell::of(fx0, fy0);
    if Cell::of(fx1, fy1) != cell || g.len() > MAX_SHARE_POINTS {
        return Ok(None);
    }
    let (ox, oy) = cell.origin_q().ok_or_else(|| too_big("cell out of range"))?;
    let mut sh = Share::new(cell);
    sh.points.reserve(g.len());
    let mut run = (i64::MAX, i64::MAX);
    for (j, p) in g.iter().enumerate() {
        // Inside the cell: |local| < 2^21 grid units, no overflow.
        let (lx, ly) = (p.qx.saturating_sub(ox), p.qy.saturating_sub(oy));
        if j == 0 || lx.abs_diff(run.0) > RUN_SPAN.unsigned_abs() || ly.abs_diff(run.1) > RUN_SPAN.unsigned_abs() {
            if sh.runs.len() >= MAX_SHARE_RUNS {
                return Ok(None);
            }
            sh.runs.push(Run {
                x: local_f32(lx),
                y: local_f32(ly),
            });
            run = (lx, ly);
        }
        sh.points.push(Point {
            dx: i16::try_from(lx.saturating_sub(run.0)).unwrap_or(0),
            dy: i16::try_from(ly.saturating_sub(run.1)).unwrap_or(0),
            run: u32::try_from(sh.runs.len().saturating_sub(1)).unwrap_or(0),
            r: radius_byte(p.r, r_max),
            flags: if j == 0 { F_START } else { 0 },
        });
    }
    let rr = f64::from(r_max);
    sh.bounds = Rect {
        min: P::new(from_q(x0) - rr, from_q(y0) - rr),
        max: P::new(from_q(x1) + rr, from_q(y1) + rr),
    };
    Ok(Some(Encoded {
        color,
        r_max,
        highlighter,
        shares: vec![sh],
    }))
}

/// Appends one point; `new_piece` forces a new run.
fn push(sh: &mut Share, p: G, r_max: f32, flags: u8, new_piece: bool) -> Result<(), RenderError> {
    let (ox, oy) = sh.cell.origin_q().ok_or_else(|| too_big("cell out of range"))?;
    let (lx, ly) = (
        p.qx.checked_sub(ox).ok_or_else(|| too_big("offset"))?,
        p.qy.checked_sub(oy).ok_or_else(|| too_big("offset"))?,
    );
    if lx.abs() >= LOCAL_MAX || ly.abs() >= LOCAL_MAX {
        return Err(too_big("chunk-local coordinate"));
    }
    let far = |a: i64, b: i64| a.checked_sub(b).is_none_or(|d| d.abs() > RUN_SPAN);
    if new_piece || sh.runs.is_empty() || far(lx, sh.run_q.0) || far(ly, sh.run_q.1) {
        sh.runs.push(Run {
            x: local_f32(lx),
            y: local_f32(ly),
        });
        sh.run_q = (lx, ly);
    }
    let d = |l: i64, o: i64| i16::try_from(l.saturating_sub(o)).map_err(|_| too_big("run offset"));
    sh.points.push(Point {
        dx: d(lx, sh.run_q.0)?,
        dy: d(ly, sh.run_q.1)?,
        run: u32::try_from(sh.runs.len().saturating_sub(1)).map_err(|_| too_big("runs"))?,
        r: radius_byte(p.r, r_max),
        flags,
    });
    Ok(())
}

/// Decodes a point to canvas coordinates (the shader does the same in chunk-local f32).
#[cfg(test)]
pub(crate) fn decode(sh: &Share, p: &Point) -> Option<(f64, f64)> {
    let run = sh.runs.get(usize::try_from(p.run).ok()?)?;
    let (ox, oy) = sh.cell.origin();
    Some((
        ox + f64::from(run.x) + f64::from(p.dx) / Q,
        oy + f64::from(run.y) + f64::from(p.dy) / Q,
    ))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use vibenote_model::{Color, Sample};

    fn stroke(pts: &[(f64, f64, f32)], width: f32, tool: Tool) -> Stroke {
        Stroke {
            tool,
            color: Color(0x1020_30ff),
            width,
            samples: pts
                .iter()
                .enumerate()
                .map(|(i, &(x, y, p))| {
                    Sample {
                        x,
                        y,
                        pressure: p,
                        tilt_x: 0.0,
                        tilt_y: 0.0,
                        t_us: i as u64,
                    }
                    .quantised()
                })
                .collect(),
        }
    }

    /// Instances (segment owner copies) of the encoding, as (from, to) canvas points.
    fn owned_segments(e: &Encoded) -> Vec<((f64, f64), (f64, f64))> {
        let mut v = Vec::new();
        for sh in &e.shares {
            for (i, p) in sh.points.iter().enumerate() {
                if p.flags & (F_CONT | F_SEAM) != 0 {
                    continue;
                }
                let to = decode(sh, p).unwrap();
                let from = if p.flags & F_START != 0 {
                    to
                } else {
                    decode(sh, &sh.points[i - 1]).unwrap()
                };
                v.push((from, to));
            }
        }
        v
    }

    fn arb_stroke() -> impl Strategy<Value = Stroke> {
        (
            prop_oneof![Just(0.0f64), Just(1.0e7), Just(-1.0e9 + 1.0e5), Just(2047.5)],
            prop::collection::vec((-3000.0f64..3000.0, -3000.0f64..3000.0, 0.0f32..=1.0), 1..60).prop_map(|v| {
                // mostly short segments (real pen input), sometimes long jumps
                let mut acc = (0.0, 0.0);
                v.into_iter()
                    .enumerate()
                    .map(|(i, (x, y, p))| {
                        if i % 7 == 3 {
                            acc = (x, y)
                        } else {
                            acc = (acc.0 + x / 200.0, acc.1 + y / 200.0)
                        }
                        (acc.0, acc.1, p)
                    })
                    .collect::<Vec<_>>()
            }),
            0.1f32..80.0,
        )
            .prop_map(|(off, pts, w)| {
                let pts: Vec<_> = pts.into_iter().map(|(x, y, p)| (x + off, y - off, p)).collect();
                stroke(&pts, w, Tool::Pen)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]
        /// Every point decodes exactly to the quantised model coordinates, and every segment is
        /// drawn exactly once by its owner cell (unclipped, since the owner contains its end
        /// and the grown box contains the whole segment when it is short).
        #[test]
        fn encoding_is_lossless_and_owned_once(s in arb_stroke()) {
            let e = encode(&s).unwrap();
            let want: Vec<((f64, f64), (f64, f64))> = s.samples.iter().enumerate().map(|(i, b)| {
                let a = if i == 0 { b } else { &s.samples[i - 1] };
                ((a.x, a.y), (b.x, b.y))
            }).collect();
            let mut got = owned_segments(&e);
            // A segment shorter than the margin lies inside its owner's grown box, so it is never
            // clipped there; longer ones may be (their pixels are covered per cell).
            let fits = |((ax, ay), (bx, by)): ((f64, f64), (f64, f64))| (ax - bx).abs() < 30.0 && (ay - by).abs() < 30.0;
            let mut want: Vec<_> = want.into_iter().filter(|s| fits(*s)).collect();
            got.retain(|s| fits(*s));
            let key = |s: &((f64, f64), (f64, f64))| (s.1 .0.to_bits(), s.1 .1.to_bits(), s.0 .0.to_bits(), s.0 .1.to_bits());
            want.sort_by_key(key);
            got.sort_by_key(key);
            prop_assert_eq!(got, want);
            for sh in &e.shares {
                prop_assert!(sh.runs.len() <= MAX_SHARE_RUNS && sh.points.len() <= MAX_SHARE_POINTS);
            }
        }

        #[test]
        fn radius_error_is_within_one_step(s in arb_stroke()) {
            let e = encode(&s).unwrap();
            for sh in &e.shares {
                for p in &sh.points {
                    let r = e.r_max * f32::from(p.r) / 255.0;
                    prop_assert!(r <= e.r_max + 1e-3 && r >= 0.0);
                }
            }
            // The first sample's radius round-trips within r_max / 510.
            let first = s.samples[0];
            let want = vibenote_ink::pressure_width(s.width, first.pressure) * 0.5;
            let sh = e.shares.iter().find(|sh| sh.points.iter().any(|p| p.flags & F_START != 0 && p.flags & F_SEAM == 0)).unwrap();
            let p = sh.points.iter().find(|p| p.flags & F_START != 0).unwrap();
            prop_assert!((e.r_max * f32::from(p.r) / 255.0 - want).abs() <= e.r_max / 510.0 + 1e-4);
        }

        #[test]
        fn hostile_strokes_never_panic(pts in prop::collection::vec((-2.0e9f64..2.0e9, -2.0e9f64..2.0e9, -1.0f32..2.0), 1..6), w in prop_oneof![Just(f32::NAN), Just(1e30f32), Just(-3.0f32), Just(9999.0f32)]) {
            let _ = encode(&stroke(&pts, w, Tool::Highlighter));
        }
    }

    #[test]
    fn a_short_stroke_is_one_share_with_one_run() {
        let s = stroke(
            &[(1010.0, 1010.0, 0.5), (1012.0, 1011.0, 0.6), (1020.0, 1015.0, 0.7)],
            2.0,
            Tool::Pen,
        );
        let e = encode(&s).unwrap();
        assert_eq!(e.shares.len(), 1);
        assert_eq!(e.shares[0].runs.len(), 1);
        assert_eq!(e.shares[0].points.len(), 3);
        assert_eq!(e.shares[0].points[0].flags, F_START);
    }

    #[test]
    fn strokes_near_a_seam_go_to_both_cells() {
        let s = stroke(&[(2040.0, 1000.0, 0.5), (2060.0, 1001.0, 0.5)], 2.0, Tool::Pen);
        let e = encode(&s).unwrap();
        let cells: Vec<_> = e.shares.iter().map(|s| s.cell).collect();
        assert_eq!(cells, vec![Cell { x: 0, y: 0 }, Cell { x: 1, y: 0 }]);
        // the second segment is owned by cell (1, 0)
        let c0 = e.shares.iter().find(|s| s.cell == Cell { x: 0, y: 0 }).unwrap();
        assert_eq!(c0.points.last().unwrap().flags & F_SEAM, F_SEAM);
    }

    #[test]
    fn long_strokes_start_new_runs_every_32_units() {
        let pts: Vec<_> = (0..200).map(|i| (900.0 + f64::from(i), 1000.0, 0.5)).collect();
        let e = encode(&stroke(&pts, 2.0, Tool::Pen)).unwrap();
        assert_eq!(e.shares.len(), 1);
        assert!(e.shares[0].runs.len() >= 6 && e.shares[0].runs.len() <= 8);
    }

    #[test]
    fn a_huge_segment_is_clipped_not_blown_up() {
        let s = stroke(&[(-5.0e8, 0.0, 0.5), (5.0e8, 10.0, 0.5)], 2.0, Tool::Pen);
        // 1e9 units / 2048 ≈ 488k cells: beyond the per-stroke bound, so an error, quickly.
        assert!(encode(&s).is_err());
        let s = stroke(&[(-50_000.0, 1000.0, 0.5), (50_000.0, 1010.0, 0.5)], 2.0, Tool::Pen);
        let e = encode(&s).unwrap();
        // one row of cells along the segment (≈ 100,000 / 2,048 + the margins)
        assert!((49..=52).contains(&e.shares.len()), "{}", e.shares.len());
        for sh in &e.shares {
            assert_eq!(sh.cell.y, 0);
            assert!(sh.points.len() <= 3);
        }
    }
}
