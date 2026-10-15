//! Ink → line images for the recogniser (ADR 0006 decision 2): a block is split into lines,
//! and each line is drawn black on white into a 48 px-high greyscale image, the input size of
//! PP-OCR's recogniser. Width follows the line's aspect ratio (bounded).

/// Height of a line image (pixels).
pub const LINE_H: usize = 48;
/// Widest line image (pixels): longer lines are scaled down.
pub const MAX_W: usize = 3200;

/// A greyscale image, 0 = black ink, 1 = white paper, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub px: Vec<f32>,
}

fn bbox(strokes: &[&Vec<(f64, f64)>]) -> Option<(f64, f64, f64, f64)> {
    let mut b: Option<(f64, f64, f64, f64)> = None;
    for p in strokes
        .iter()
        .flat_map(|s| s.iter())
        .filter(|p| p.0.is_finite() && p.1.is_finite())
    {
        b = Some(match b {
            Some(o) => (o.0.min(p.0), o.1.min(p.1), o.2.max(p.0), o.3.max(p.1)),
            None => (p.0, p.1, p.0, p.1),
        });
    }
    b
}

/// Splits a block into text lines (top to bottom): stroke centres are grouped by their height
/// after removing the block's overall slope (a tilted line stays one line).
pub fn lines(strokes: &[Vec<(f64, f64)>]) -> Vec<Vec<usize>> {
    let items: Vec<(usize, f64, f64, f64)> = strokes
        .iter()
        .enumerate()
        .filter_map(|(k, s)| {
            let (x0, y0, x1, y1) = bbox(&[s])?;
            Some((k, (x0 + x1) / 2.0, (y0 + y1) / 2.0, y1 - y0))
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    let mut hs: Vec<f64> = items.iter().map(|i| i.3).collect();
    hs.sort_by(f64::total_cmp);
    let typical = hs.get(hs.len() / 2).copied().unwrap_or(1.0).max(1e-6);
    // Robust slope of the stroke centres (Theil–Sen over a bounded number of pairs); pairs far
    // apart vertically (other lines) are left out, steep slopes are not writing lines.
    let mut slopes = Vec::new();
    let step = (items.len() / 150).max(1);
    for (i, a) in items.iter().enumerate().step_by(step) {
        for b in items.iter().skip(i.saturating_add(1)).step_by(step) {
            let dx = b.1 - a.1;
            if dx.abs() > typical * 0.5 && (b.2 - a.2).abs() < 2.0 * typical {
                slopes.push((b.2 - a.2) / dx);
            }
        }
    }
    slopes.sort_by(f64::total_cmp);
    let slope = slopes.get(slopes.len() / 2).copied().unwrap_or(0.0).clamp(-0.6, 0.6);
    let mut keyed: Vec<(usize, f64)> = items.iter().map(|i| (i.0, i.2 - slope * i.1)).collect();
    keyed.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out: Vec<(f64, Vec<usize>)> = Vec::new();
    for (k, r) in keyed {
        // A stroke belongs to a line whose level is within a typical stroke height.
        match out.iter_mut().find(|(c, _)| (r - *c).abs() <= 0.75 * typical) {
            Some((c, ks)) => {
                #[allow(clippy::cast_precision_loss)] // stroke counts
                let n = ks.len() as f64;
                *c = (*c * n + r) / (n + 1.0);
                ks.push(k);
            }
            None => out.push((r, vec![k])),
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter()
        .map(|(_, mut ks)| {
            ks.sort_unstable();
            ks
        })
        .collect()
}

/// Draws strokes into a `LINE_H`-high image: the ink's height maps to 70 % of the image with
/// a margin, pen width about 2.5 % of the height (like scanned writing at this size).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)] // pixel math, bounded
pub fn render(strokes: &[&Vec<(f64, f64)>]) -> Option<Gray> {
    let (x0, y0, x1, y1) = bbox(strokes)?;
    let ink_h = (y1 - y0).max(1e-6);
    let target = LINE_H as f64 * 0.7;
    let mut k = target / ink_h;
    let margin = LINE_H as f64 * 0.15;
    let mut width = ((x1 - x0) * k + 2.0 * margin).ceil().max(LINE_H as f64);
    if width > MAX_W as f64 {
        k *= (MAX_W as f64 - 2.0 * margin) / ((x1 - x0) * k).max(1e-6);
        width = MAX_W as f64;
    }
    let (w, h) = (width as usize, LINE_H);
    let mut px = vec![1.0f32; w.saturating_mul(h)];
    let radius = 0.6 + LINE_H as f64 * 0.0125;
    let oy = (h as f64 - (y1 - y0) * k) / 2.0;
    let map = |p: &(f64, f64)| ((p.0 - x0) * k + margin, (p.1 - y0) * k + oy);
    for s in strokes {
        let pts: Vec<(f64, f64)> = s
            .iter()
            .filter(|p| p.0.is_finite() && p.1.is_finite())
            .map(map)
            .collect();
        let segs: Vec<((f64, f64), (f64, f64))> = if pts.len() == 1 {
            pts.first().map(|p| vec![(*p, *p)]).unwrap_or_default()
        } else {
            pts.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))).collect()
        };
        for (a, b) in segs {
            let (bx0, bx1) = (
                (a.0.min(b.0) - radius - 1.0).max(0.0),
                (a.0.max(b.0) + radius + 1.0).min(w as f64 - 1.0),
            );
            let (by0, by1) = (
                (a.1.min(b.1) - radius - 1.0).max(0.0),
                (a.1.max(b.1) + radius + 1.0).min(h as f64 - 1.0),
            );
            if bx1 < bx0 || by1 < by0 {
                continue;
            }
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            for y in (by0 as usize)..=(by1 as usize) {
                for x in (bx0 as usize)..=(bx1 as usize) {
                    let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                    let t = if l2 > 0.0 {
                        (((cx - a.0) * dx + (cy - a.1) * dy) / l2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = (cx - a.0 - dx * t).hypot(cy - a.1 - dy * t);
                    let cov = (radius + 0.5 - d).clamp(0.0, 1.0) as f32;
                    if let Some(v) = px.get_mut(y.saturating_mul(w).saturating_add(x)) {
                        *v = v.min(1.0 - cov);
                    }
                }
            }
        }
    }
    Some(Gray {
        width: w,
        height: h,
        px,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_by_height_and_render_fits() {
        let a = vec![(0.0, 0.0), (10.0, 20.0)];
        let b = vec![(20.0, 2.0), (30.0, 18.0)];
        let c = vec![(0.0, 100.0), (10.0, 120.0)];
        assert_eq!(lines(&[a.clone(), c.clone(), b.clone()]), vec![vec![0, 2], vec![1]]);
        let g = render(&[&a, &b]).unwrap();
        assert_eq!(g.height, LINE_H);
        assert!(g.px.iter().any(|v| *v < 0.2) && g.px.iter().any(|v| *v > 0.99));
        let long: Vec<(f64, f64)> = (0..10_000).map(|i| (f64::from(i) * 10.0, f64::from(i % 2))).collect();
        assert!(render(&[&long]).unwrap().width <= MAX_W);
    }
}
