//! Minimal ink model: pressure-width strokes → triangle mesh; linear ink prediction.

#[derive(Clone, Copy, Debug, Default)]
pub struct Pt {
    /// Physical pixels.
    pub x: f32,
    pub y: f32,
    /// 0..1
    pub p: f32,
    /// Event time, ns monotonic (receive time; used for prediction).
    pub ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Stroke {
    pub pts: Vec<Pt>,
    pub eraser: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

pub const INK: [f32; 4] = [0.08, 0.09, 0.12, 1.0];
pub const PAPER: [f32; 4] = [0.984, 0.980, 0.965, 1.0];
pub const ERASE: [f32; 4] = [0.95, 0.55, 0.55, 1.0];
pub const PREDICT: [f32; 4] = INK;

/// Stroke width in physical px for pressure p.
pub fn width(p: f32, scale: f32) -> f32 {
    (0.6 + 5.4 * p.clamp(0.0, 1.0)) * scale
}

fn disc(out: &mut Vec<Vertex>, x: f32, y: f32, r: f32, c: [f32; 4]) {
    const N: usize = 12;
    for i in 0..N {
        let a0 = i as f32 / N as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / N as f32 * std::f32::consts::TAU;
        out.push(Vertex { pos: [x, y], color: c });
        out.push(Vertex { pos: [x + r * a0.cos(), y + r * a0.sin()], color: c });
        out.push(Vertex { pos: [x + r * a1.cos(), y + r * a1.sin()], color: c });
    }
}

/// Appends triangles for segment a→b (tapered quad + round cap at b).
pub fn segment(out: &mut Vec<Vertex>, a: Pt, b: Pt, scale: f32, c: [f32; 4]) {
    let (wa, wb) = (width(a.p, scale) * 0.5, width(b.p, scale) * 0.5);
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len > 1e-3 {
        let (nx, ny) = (-dy / len, dx / len);
        let v = |x: f32, y: f32| Vertex { pos: [x, y], color: c };
        let (a0, a1) = (v(a.x + nx * wa, a.y + ny * wa), v(a.x - nx * wa, a.y - ny * wa));
        let (b0, b1) = (v(b.x + nx * wb, b.y + ny * wb), v(b.x - nx * wb, b.y - ny * wb));
        out.extend_from_slice(&[a0, a1, b0, a1, b1, b0]);
    }
    disc(out, b.x, b.y, wb, c);
}

/// Mesh for a whole stroke from point index `from` (for incremental appends).
pub fn stroke_mesh(out: &mut Vec<Vertex>, s: &Stroke, from: usize, scale: f32) {
    let c = if s.eraser { ERASE } else { INK };
    if from == 0 {
        if let Some(p) = s.pts.first() {
            disc(out, p.x, p.y, width(p.p, scale) * 0.5, c);
        }
    }
    for i in from.max(1)..s.pts.len() {
        segment(out, s.pts[i - 1], s.pts[i], scale, c);
    }
}

/// Linear velocity extrapolation over the last ~`window_ns` of input, `horizon_ns` ahead.
/// Returns predicted points (excluding the last real point). Empty if not enough data.
pub fn predict(s: &Stroke, horizon_ns: u64, window_ns: u64) -> Vec<Pt> {
    let n = s.pts.len();
    if horizon_ns == 0 || n < 3 {
        return vec![];
    }
    let last = s.pts[n - 1];
    let first = s.pts[..n - 1]
        .iter()
        .rev()
        .find(|p| last.ns.saturating_sub(p.ns) >= window_ns)
        .copied()
        .unwrap_or(s.pts[0]);
    let dt = last.ns.saturating_sub(first.ns) as f32;
    if dt <= 0.0 {
        return vec![];
    }
    let (vx, vy) = ((last.x - first.x) / dt, (last.y - first.y) / dt);
    let h = horizon_ns as f32;
    let steps = 3;
    (1..=steps)
        .map(|k| {
            let t = h * k as f32 / steps as f32;
            Pt { x: last.x + vx * t, y: last.y + vy * t, p: last.p, ns: last.ns + t as u64 }
        })
        .collect()
}
