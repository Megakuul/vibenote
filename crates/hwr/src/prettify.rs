//! Prettify stage 1 (ADR 0008 decision 1): per line, rotate the writing to horizontal and move
//! each word's baseline toward the line's baseline. Words move rigidly, so no glyph changes
//! shape; the result is a display transform per stroke (the strokes themselves never change).

use crate::raster::lines;

/// Share of a word's baseline offset that is corrected, and the largest move (× x-height).
const BASE_K: f64 = 0.8;
const MAX_MOVE_XH: f64 = 0.3;
/// Lines tilted more than this are left alone (likely not a writing line).
const MAX_ANGLE_DEG: f64 = 30.0;
/// A horizontal gap wider than this (× x-height) separates words.
const WORD_GAP_XH: f64 = 0.6;

/// Display transforms (affine `[a, b, c, d, e, f]`: x' = a·x + c·y + e, y' = b·x + d·y + f)
/// for each input stroke, and what was done.
#[derive(Clone, Debug, PartialEq)]
pub struct Prettified {
    pub transforms: Vec<[f64; 6]>,
    /// Mean line rotation (degrees).
    pub angle_deg: f64,
    pub words: usize,
    /// Largest point displacement after removing each word's own translation, × x-height
    /// (shape change; R-HWR-004 allows ≤ 0.35).
    pub max_shape_displacement_xh: f64,
}

const ID: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn apply(t: &[f64; 6], p: (f64, f64)) -> (f64, f64) {
    (t[0] * p.0 + t[2] * p.1 + t[4], t[1] * p.0 + t[3] * p.1 + t[5])
}

fn percentile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )] // index into v
    let k = (v.len().saturating_sub(1) as f64 * q).round() as usize;
    v.get(k).copied().unwrap_or(0.0)
}

/// Theil–Sen slope through points (all pairs, bounded).
fn theil_sen(p: &[(f64, f64)]) -> f64 {
    let mut slopes = Vec::new();
    let step = (p.len() / 200).max(1);
    for (i, a) in p.iter().enumerate().step_by(step) {
        for b in p.iter().skip(i.saturating_add(1)).step_by(step) {
            let dx = b.0 - a.0;
            if dx.abs() > 1e-9 {
                slopes.push((b.1 - a.1) / dx);
            }
        }
    }
    percentile(&mut slopes, 0.5)
}

/// Stage-1 prettify of a block (any number of lines).
pub fn stage1(strokes: &[Vec<(f64, f64)>]) -> Option<Prettified> {
    if strokes.is_empty()
        || strokes
            .iter()
            .any(|s| s.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()))
    {
        return None;
    }
    let mut transforms = vec![ID; strokes.len()];
    let (mut angles, mut words_n, mut worst) = (Vec::new(), 0usize, 0.0f64);
    for line in lines(strokes) {
        let ls: Vec<&Vec<(f64, f64)>> = line.iter().filter_map(|k| strokes.get(*k)).collect();
        let mut ys: Vec<f64> = ls.iter().flat_map(|s| s.iter().map(|p| p.1)).collect();
        let xh = (percentile(&mut ys.clone(), 0.8) - percentile(&mut ys, 0.2)).max(1e-3);
        // Baseline through each stroke's lowest point (y grows downward).
        let bottoms: Vec<(f64, f64)> = ls
            .iter()
            .filter_map(|s| s.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)))
            .collect();
        let slope = if bottoms.len() >= 2 { theil_sen(&bottoms) } else { 0.0 };
        let mut angle = slope.atan();
        if angle.to_degrees().abs() > MAX_ANGLE_DEG {
            angle = 0.0;
        }
        angles.push(angle.to_degrees());
        let all: Vec<(f64, f64)> = ls.iter().flat_map(|s| s.iter().copied()).collect();
        #[allow(clippy::cast_precision_loss)] // point counts
        let n = all.len().max(1) as f64;
        let (cx, cy) = (
            all.iter().map(|p| p.0).sum::<f64>() / n,
            all.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let (s, c) = (-angle).sin_cos();
        let rot = [c, s, -s, c, cx - c * cx + s * cy, cy - s * cx - c * cy];
        let rotated: Vec<Vec<(f64, f64)>> = ls
            .iter()
            .map(|st| st.iter().map(|p| apply(&rot, *p)).collect())
            .collect();
        // Words: strokes ordered by left edge, split at wide gaps.
        let mut order: Vec<usize> = (0..rotated.len()).collect();
        let left = |k: usize| {
            rotated
                .get(k)
                .map_or(0.0, |s| s.iter().map(|p| p.0).fold(f64::INFINITY, f64::min))
        };
        let right = |k: usize| {
            rotated
                .get(k)
                .map_or(0.0, |s| s.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max))
        };
        order.sort_by(|a, b| left(*a).total_cmp(&left(*b)));
        let mut words: Vec<Vec<usize>> = Vec::new();
        let mut edge = f64::NEG_INFINITY;
        for k in order {
            if words.is_empty() || left(k) - edge > WORD_GAP_XH * xh {
                words.push(vec![k]);
            } else if let Some(w) = words.last_mut() {
                w.push(k);
            }
            edge = edge.max(right(k));
        }
        words_n = words_n.saturating_add(words.len());
        let base_of = |w: &Vec<usize>| {
            let mut v: Vec<f64> = w
                .iter()
                .filter_map(|k| rotated.get(*k))
                .flat_map(|s| s.iter().map(|p| p.1))
                .collect();
            percentile(&mut v, 0.85)
        };
        let mut bases: Vec<f64> = words.iter().map(base_of).collect();
        let line_base = percentile(&mut bases.clone(), 0.5);
        for (w, base) in words.iter().zip(bases.iter_mut()) {
            let dy = (BASE_K * (line_base - *base)).clamp(-MAX_MOVE_XH * xh, MAX_MOVE_XH * xh);
            for k in w {
                let Some(&global) = line.get(*k) else { continue };
                let mut t = rot;
                t[5] += dy;
                if let Some(slot) = transforms.get_mut(global) {
                    *slot = t;
                }
            }
            // Shape check: displacement beyond the word's own translation.
            let mut d = Vec::new();
            for k in w {
                let (Some(&global), Some(r)) = (line.get(*k), rotated.get(*k)) else {
                    continue;
                };
                let (Some(t), Some(orig)) = (transforms.get(global), strokes.get(global)) else {
                    continue;
                };
                for (p, q) in orig.iter().zip(r) {
                    let a = apply(t, *p);
                    d.push((a.0 - q.0, a.1 - q.1));
                }
            }
            #[allow(clippy::cast_precision_loss)] // point counts
            let m = d.len().max(1) as f64;
            let (mx, my) = (
                d.iter().map(|v| v.0).sum::<f64>() / m,
                d.iter().map(|v| v.1).sum::<f64>() / m,
            );
            for v in &d {
                worst = worst.max((v.0 - mx).hypot(v.1 - my) / xh);
            }
        }
    }
    #[allow(clippy::cast_precision_loss)] // line counts
    let angle_deg = if angles.is_empty() {
        0.0
    } else {
        angles.iter().sum::<f64>() / angles.len() as f64
    };
    Some(Prettified {
        transforms,
        angle_deg,
        words: words_n,
        max_shape_displacement_xh: worst,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn a_tilted_line_becomes_level_without_changing_shapes() {
        // Three "words" of two strokes each, on a line tilted by 10°, one word sagging.
        let tilt = 10f64.to_radians();
        let mut strokes = Vec::new();
        for w in 0..3 {
            for k in 0..2 {
                let x0 = f64::from(w) * 100.0 + f64::from(k) * 20.0;
                let sag = if w == 1 { 6.0 } else { 0.0 };
                let s: Vec<(f64, f64)> = (0..10)
                    .map(|i| {
                        let (x, y) = (x0 + f64::from(i) * 1.5, f64::from(i % 5) * 4.0 + sag);
                        (x * tilt.cos() - y * tilt.sin(), x * tilt.sin() + y * tilt.cos())
                    })
                    .collect();
                strokes.push(s);
            }
        }
        let p = stage1(&strokes).unwrap();
        assert!((p.angle_deg - 10.0).abs() < 1.5, "{}", p.angle_deg);
        assert_eq!(p.words, 3);
        assert!(p.max_shape_displacement_xh < 1e-9);
        // After the transform, every word's bottom is at about the same height.
        let bottoms: Vec<f64> = (0..3)
            .map(|w| {
                (0..2)
                    .flat_map(|k| {
                        strokes[w * 2 + k]
                            .iter()
                            .map(|q| apply(&p.transforms[w * 2 + k], *q).1)
                            .collect::<Vec<_>>()
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        let spread = bottoms.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
            - bottoms.iter().fold(f64::INFINITY, |a, b| a.min(*b));
        assert!(spread < 3.0, "{bottoms:?}");
        assert!(stage1(&[]).is_none());
    }
}
