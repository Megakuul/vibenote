//! Order-invariant geometric features of an ink block (ADR 0007's writing-vs-drawing baseline,
//! ported from `spikes/hwr/classify.py`). The classifier is trained on exactly these values
//! (`tools/hwrtrain` calls `vibenote-cli hwr features`), so this code is the definition.
//!
//! Everything is measured in units of the block's median stroke extent, so the features do not
//! depend on zoom or handwriting size.

/// Number of features.
pub const N: usize = 23;
/// Most points resampled per stroke (bounded cost for hostile input).
const MAX_RESAMPLE: f64 = 2000.0;

fn hypot(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// Points along the polyline every `step` (plus the start).
fn resample(p: &[(f64, f64)], step: f64) -> Vec<(f64, f64)> {
    if p.len() < 2 {
        return p.to_vec();
    }
    let mut d = Vec::with_capacity(p.len());
    d.push(0.0);
    for w in p.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else {
            continue;
        };
        let last = d.last().copied().unwrap_or(0.0);
        d.push(last + hypot(a, b));
    }
    let total = d.last().copied().unwrap_or(0.0);
    if total < 1e-9 {
        return p.get(..1).unwrap_or_default().to_vec();
    }
    let step = step.max(total / MAX_RESAMPLE);
    let mut out = Vec::new();
    let mut seg = 0usize;
    let mut t = 0.0;
    while t < total {
        while d.get(seg.saturating_add(1)).is_some_and(|&x| x < t) {
            seg = seg.saturating_add(1);
        }
        let (Some(&a), Some(&b), Some(&da), Some(&db)) = (
            p.get(seg),
            p.get(seg.saturating_add(1)),
            d.get(seg),
            d.get(seg.saturating_add(1)),
        ) else {
            break;
        };
        let f = if db > da { (t - da) / (db - da) } else { 0.0 };
        out.push((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f));
        t += step;
    }
    out
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let m = n / 2;
    if n % 2 == 1 {
        v.get(m).copied().unwrap_or(0.0)
    } else {
        (v.get(m.saturating_sub(1)).copied().unwrap_or(0.0) + v.get(m).copied().unwrap_or(0.0)) / 2.0
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)] // counts
        let n = v.len() as f64;
        v.iter().sum::<f64>() / n
    }
}

fn ptp(v: impl Iterator<Item = f64>) -> f64 {
    let (lo, hi) = v.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)));
    if hi >= lo { hi - lo } else { 0.0 }
}

/// The feature vector of a block, or `None` for an empty one.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // counts and bin indices
pub fn features(strokes: &[Vec<(f64, f64)>]) -> Option<[f64; N]> {
    let strokes: Vec<&Vec<(f64, f64)>> = strokes
        .iter()
        .filter(|s| !s.is_empty() && s.iter().all(|p| p.0.is_finite() && p.1.is_finite()))
        .collect();
    if strokes.is_empty() {
        return None;
    }
    let all = strokes.iter().flat_map(|s| s.iter());
    let (w, h) = (ptp(all.clone().map(|p| p.0)).max(1e-6), ptp(all.map(|p| p.1)).max(1e-6));
    let sh: Vec<f64> = strokes.iter().map(|s| ptp(s.iter().map(|p| p.1)).max(1e-6)).collect();
    let sw: Vec<f64> = strokes.iter().map(|s| ptp(s.iter().map(|p| p.0)).max(1e-6)).collect();
    let mut ext: Vec<f64> = sh
        .iter()
        .zip(&sw)
        .map(|(a, b)| a.max(*b))
        .filter(|e| *e > 1e-3)
        .collect();
    let unit = median(&mut ext).max(w.max(h) / 100.0).max(1e-3);
    let (mut lens, mut straight, mut closed, mut turns) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut dirs = [0.0f64; 8];
    let mut ys = Vec::new();
    for s in &strokes {
        let r = resample(s, unit / 8.0);
        ys.extend(r.iter().map(|p| p.1));
        if r.len() < 3 {
            lens.push(0.0);
            straight.push(1.0);
            closed.push(0.0);
            turns.push(0.0);
            continue;
        }
        let segs: Vec<(f64, f64)> = r
            .windows(2)
            .filter_map(|w| Some((w.get(1)?.0 - w.first()?.0, w.get(1)?.1 - w.first()?.1)))
            .collect();
        let l: f64 = segs.iter().map(|d| d.0.hypot(d.1)).sum();
        let (first, last) = (
            r.first().copied().unwrap_or((0.0, 0.0)),
            r.last().copied().unwrap_or((0.0, 0.0)),
        );
        let chord = hypot(first, last);
        lens.push(l / unit);
        straight.push(chord / l.max(1e-9));
        closed.push(if chord < 0.15 * l.max(1e-9) && l > unit {
            1.0
        } else {
            0.0
        });
        let ang: Vec<f64> = segs.iter().map(|d| d.1.atan2(d.0)).collect();
        let pi = std::f64::consts::PI;
        let da: f64 = ang
            .windows(2)
            .filter_map(|w| Some(((w.get(1)? - w.first()? + pi).rem_euclid(2.0 * pi) - pi).abs()))
            .sum();
        turns.push(da / (2.0 * pi) / (l / unit).max(1e-6));
        for (a, d) in ang.iter().zip(&segs) {
            let v = (a + pi).rem_euclid(pi);
            let bin = ((v / pi * 8.0) as usize).min(7);
            if let Some(slot) = dirs.get_mut(bin) {
                *slot += d.0.hypot(d.1);
            }
        }
    }
    let total: f64 = dirs.iter().sum();
    for d in &mut dirs {
        *d /= total.max(1e-9);
    }
    // Concentration of ink in the densest band one unit high (writing: the x-height band).
    let bins = ((h / unit * 4.0) as usize).clamp(4, 100_000);
    let (lo, hi) = ys.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), y| {
        (lo.min(*y), hi.max(*y))
    });
    let (lo, hi) = if hi > lo { (lo, hi) } else { (lo - 0.5, hi + 0.5) };
    let mut hist = vec![0.0f64; bins];
    for y in &ys {
        let k = (((y - lo) / (hi - lo)) * bins as f64) as usize;
        if let Some(slot) = hist.get_mut(k.min(bins.saturating_sub(1))) {
            *slot += 1.0;
        }
    }
    let band = hist.windows(4).map(|w| w.iter().sum::<f64>()).fold(0.0f64, f64::max) / (ys.len() as f64).max(1.0);
    let cx: Vec<f64> = strokes
        .iter()
        .map(|s| mean(&s.iter().map(|p| p.0).collect::<Vec<_>>()))
        .collect();
    let shu: Vec<f64> = sh.iter().map(|v| v / unit).collect();
    let sd = {
        let m = mean(&shu);
        (shu.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / shu.len().max(1) as f64).sqrt()
    };
    let n = strokes.len() as f64;
    let mut v = [0.0f64; N];
    let head = [
        n,
        (w / h).ln(),
        w / unit,
        h / unit,
        median(&mut lens.clone()),
        lens.iter().copied().fold(0.0, f64::max),
        mean(&straight),
        straight.iter().filter(|s| **s > 0.97).count() as f64 / n,
        mean(&closed),
        median(&mut turns.clone()),
        mean(&turns),
        band,
        sd,
        median(&mut shu.clone()),
        ptp(cx.iter().copied()) / unit / n.max(1.0),
    ];
    for (slot, x) in v.iter_mut().zip(head.iter().chain(dirs.iter())) {
        *slot = *x;
    }
    Some(v)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn features_are_scale_invariant_and_bounded() {
        let wave: Vec<(f64, f64)> = (0..100)
            .map(|i| (f64::from(i), (f64::from(i) * 0.3).sin() * 10.0))
            .collect();
        let big: Vec<(f64, f64)> = wave.iter().map(|p| (p.0 * 7.0, p.1 * 7.0)).collect();
        let (a, b) = (
            features(&[wave.clone(), wave.clone()]).unwrap(),
            features(&[big.clone(), big]).unwrap(),
        );
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 0.05 * (1.0 + x.abs()), "{a:?}\n{b:?}");
        }
        assert!(features(&[]).is_none());
        assert!(features(&[vec![(0.0, 0.0)]]).unwrap().iter().all(|v| v.is_finite()));
        assert!(features(&[vec![(0.0, 0.0), (f64::NAN, 1.0)]]).is_none());
    }
}
