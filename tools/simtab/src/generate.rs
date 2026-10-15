//! Deterministic (seeded) synthetic pen input.

use crate::stroke::{Sample, Strokes, Tool, header};

use anyhow::{Result, bail};
use std::f64::consts::{PI, TAU};

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Horizontal lines, constant pressure.
    Lines,
    /// Sine-wave curves.
    Curves,
    /// Smooth random scribbles.
    Scribble,
    /// Lines with pressure ramping 0→1→0.
    PressureRamp,
    /// Lines with tilt sweeping −60°→60° on X, then Y.
    TiltSweep,
    /// Pen stroke, then eraser stroke over it, alternating.
    EraserFlip,
    /// Hover only (proximity + distance), no contact.
    Hover,
    /// Cursive-like loops (handwriting-shaped motion).
    Loops,
    /// Barrel buttons pressed during hover and contact.
    Buttons,
}

#[derive(clap::Args, Debug, Clone)]
pub struct GenOpts {
    #[arg(long, value_enum, default_value_t = Pattern::Scribble)]
    pub pattern: Pattern,
    /// Number of strokes (1..=10000).
    #[arg(long, default_value_t = 5)]
    pub strokes: u32,
    /// Event rate in Hz (1..=2000; typical pens 133–360).
    #[arg(long, default_value_t = 200.0)]
    pub rate: f64,
    /// Contact duration of each stroke in ms (1..=600000).
    #[arg(long, default_value_t = 800)]
    pub stroke_ms: u64,
    /// Gap between strokes (out of proximity) in ms.
    #[arg(long, default_value_t = 250)]
    pub gap_ms: u64,
    /// Constant pressure for patterns that do not vary it.
    #[arg(long, default_value_t = 0.5)]
    pub pressure: f64,
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
}

impl Default for GenOpts {
    fn default() -> Self {
        GenOpts {
            pattern: Pattern::Scribble,
            strokes: 5,
            rate: 200.0,
            stroke_ms: 800,
            gap_ms: 250,
            pressure: 0.5,
            seed: 1,
        }
    }
}

/// xorshift64* — deterministic and dependency-free.
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        // Top 32 bits → [0, 1).
        let hi = u32::try_from(self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32).unwrap_or(0);
        f64::from(hi) / 4_294_967_296.0
    }
}

fn idx_f(i: u64) -> f64 {
    // Exact for i < 2^53; generator indices are far below that.
    f64::from(u32::try_from(i).unwrap_or(u32::MAX))
}

pub fn generate(o: &GenOpts) -> Result<Strokes> {
    if !(1..=10_000).contains(&o.strokes) || !(1.0..=2000.0).contains(&o.rate) || !(1..=600_000).contains(&o.stroke_ms)
    {
        bail!("generator options out of range: {o:?}");
    }
    let mut rng = Rng(o.seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let dt = crate::f64_to_u64((1e6 / o.rate).round()).max(1);
    // dt ≥ 1, so checked_div never fails; fall back defensively anyway.
    let n = o.stroke_ms.saturating_mul(1000).checked_div(dt).unwrap_or(2).max(2);
    let hover_n = 40_000u64.checked_div(dt).unwrap_or(2).max(2);
    let mut out = Vec::new();
    let mut t = 0u64;
    let k = o.strokes;
    let kf = f64::from(k);
    for si in 0..k {
        let sif = f64::from(si);
        let tool = if o.pattern == Pattern::EraserFlip && si % 2 == 1 {
            Tool::Eraser
        } else {
            Tool::Pen
        };
        let row = if o.pattern == Pattern::EraserFlip {
            (f64::from(si / 2) + 0.5) / f64::from(k.div_ceil(2))
        } else {
            (sif + 0.5) / kf
        };
        let (ph1, ph2) = (rng.f() * TAU, rng.f() * TAU);
        let pattern = o.pattern;
        let path = move |u: f64| -> (f64, f64) {
            match pattern {
                Pattern::Curves => (0.1 + 0.8 * u, row + 0.3 / kf * (u * 4.0 * PI + ph1).sin()),
                Pattern::Scribble => (
                    0.5 + 0.35 * ((u * 3.1 * PI + ph1).sin() * 0.7 + (u * 7.3 * PI + ph2).sin() * 0.3),
                    0.5 + 0.35 * ((u * 2.3 * PI + ph2).cos() * 0.7 + (u * 5.9 * PI + ph1).cos() * 0.3),
                ),
                Pattern::Loops => {
                    let w = 10.0 * TAU * u;
                    let a = 0.35 / kf;
                    (0.1 + 0.8 * u - 0.012 * w.sin(), row - a * 0.5 * (1.0 - w.cos()))
                }
                _ => (0.1 + 0.8 * u, row),
            }
        };
        let tilt = move |u: f64| -> (f64, f64) {
            match pattern {
                Pattern::TiltSweep if si % 2 == 0 => (-60.0 + 120.0 * u, 0.0),
                Pattern::TiltSweep => (0.0, -60.0 + 120.0 * u),
                _ => (15.0, -10.0),
            }
        };
        let pres = |u: f64| -> f64 {
            match pattern {
                Pattern::PressureRamp => (u * PI).sin().max(0.002),
                Pattern::Hover => 0.0,
                _ => o.pressure,
            }
        };
        let btn = |i: u64| pattern == Pattern::Buttons && (i / 20) % 2 == 1;
        let mk = |t_us: u64, u: f64, p: f64, d: f64, b1: bool, b2: bool| {
            let (x, y) = path(u);
            let (tx, ty) = tilt(u);
            Sample {
                t_us,
                tool: Some(tool),
                x,
                y,
                p,
                tx,
                ty,
                d,
                b1,
                b2,
            }
        };
        // Hover in with p = 0 so libinput's pressure-offset heuristic sees a zero baseline.
        for i in 0..hover_n {
            let d = 1.0 - idx_f(i) / idx_f(hover_n);
            out.push(mk(t, 0.0, 0.0, d.max(0.05), btn(i), false));
            t = t.saturating_add(dt);
        }
        for i in 0..n {
            let u = idx_f(i) / idx_f(n.saturating_sub(1).max(1));
            out.push(mk(t, u, pres(u), 0.0, btn(i), pattern == Pattern::Buttons && i > n / 2));
            t = t.saturating_add(dt);
        }
        for i in 0..hover_n {
            let d = idx_f(i.saturating_add(1)) / idx_f(hover_n);
            out.push(mk(t, 1.0, 0.0, d, false, false));
            t = t.saturating_add(dt);
        }
        out.push(Sample {
            t_us: t,
            tool: None,
            x: 0.0,
            y: 0.0,
            p: 0.0,
            tx: 0.0,
            ty: 0.0,
            d: 1.0,
            b1: false,
            b2: false,
        });
        t = t.saturating_add(o.gap_ms.saturating_mul(1000));
    }
    Ok(Strokes {
        header: header(format!("generated: {o:?}")),
        samples: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        for p in [
            Pattern::Lines,
            Pattern::Scribble,
            Pattern::PressureRamp,
            Pattern::TiltSweep,
            Pattern::EraserFlip,
            Pattern::Loops,
        ] {
            let o = GenOpts {
                pattern: p,
                strokes: 3,
                ..Default::default()
            };
            let a = generate(&o).unwrap();
            assert_eq!(a, generate(&o).unwrap());
            for s in &a.samples {
                assert!((0.0..=1.0).contains(&s.x) && (-0.2..=1.2).contains(&s.y), "{p:?} {s:?}");
                assert!((0.0..=1.0).contains(&s.p) && (-90.0..=90.0).contains(&s.tx));
            }
        }
    }

    #[test]
    fn rng_is_uniformish() {
        let mut r = Rng(42);
        let v: Vec<f64> = (0..10_000).map(|_| r.f()).collect();
        assert!(v.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = v.iter().sum::<f64>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
    }

    #[test]
    fn rejects_bad_options() {
        assert!(
            generate(&GenOpts {
                strokes: 0,
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            generate(&GenOpts {
                rate: f64::NAN,
                ..Default::default()
            })
            .is_err()
        );
    }
}
