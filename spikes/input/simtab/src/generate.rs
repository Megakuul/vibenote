//! Stroke generator: deterministic (seeded) synthetic pen input.

use crate::stroke::{Header, Sample, Strokes, Tool};
use anyhow::Result;
use std::f64::consts::PI;

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Pattern {
    /// Horizontal lines, constant pressure.
    Lines,
    /// Sine-wave curves.
    Curves,
    /// Smooth random scribbles.
    Scribble,
    /// Lines with pressure ramping 0→1→0.
    PressureRamp,
    /// Lines with tilt sweeping -60°→60° on X then Y.
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

#[derive(clap::Args, Debug)]
pub struct GenOpts {
    #[arg(long, value_enum, default_value_t = Pattern::Scribble)]
    pub pattern: Pattern,
    /// Number of strokes.
    #[arg(long, default_value_t = 5)]
    pub strokes: u32,
    /// Event rate in Hz (typical pens: 133–360).
    #[arg(long, default_value_t = 200.0)]
    pub rate: f64,
    /// Contact duration of each stroke in ms.
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

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub fn generate(o: &GenOpts) -> Result<Strokes> {
    let mut rng = Rng(o.seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let dt_us = (1e6 / o.rate.max(1.0)) as u64;
    let n = (o.stroke_ms * 1000 / dt_us).max(2);
    let hover_n = (40_000 / dt_us).max(2); // 40 ms hover approach/leave
    let mut out = Vec::new();
    let mut t = 0u64;
    let k = o.strokes.max(1);
    for si in 0..k {
        let row = (si as f64 + 0.5) / k as f64;
        let tool = match o.pattern {
            Pattern::EraserFlip if si % 2 == 1 => Tool::Eraser,
            _ => Tool::Pen,
        };
        // EraserFlip: eraser retraces the previous stroke's row.
        let row = if matches!(o.pattern, Pattern::EraserFlip) {
            ((si / 2) as f64 + 0.5) / k.div_ceil(2) as f64
        } else {
            row
        };
        let (ph1, ph2) = (rng.f() * 2.0 * PI, rng.f() * 2.0 * PI);
        let path = |u: f64| -> (f64, f64) {
            match o.pattern {
                Pattern::Curves => (0.1 + 0.8 * u, row + 0.3 / k as f64 * (u * 4.0 * PI + ph1).sin()),
                Pattern::Scribble => {
                    let x = 0.5 + 0.35 * ((u * 3.1 * PI + ph1).sin() * 0.7 + (u * 7.3 * PI + ph2).sin() * 0.3);
                    let y = 0.5 + 0.35 * ((u * 2.3 * PI + ph2).cos() * 0.7 + (u * 5.9 * PI + ph1).cos() * 0.3);
                    (x, y)
                }
                Pattern::Loops => {
                    // Prolate cycloid: forward motion with small loops, like cursive 'l'/'e'.
                    let w = 10.0 * 2.0 * PI * u;
                    let a = 0.35 / k as f64;
                    (0.1 + 0.8 * u - 0.012 * w.sin(), row - a * 0.5 * (1.0 - w.cos()))
                }
                _ => (0.1 + 0.8 * u, row),
            }
        };
        let tilt = |u: f64| -> (f64, f64) {
            match o.pattern {
                Pattern::TiltSweep if si % 2 == 0 => (-60.0 + 120.0 * u, 0.0),
                Pattern::TiltSweep => (0.0, -60.0 + 120.0 * u),
                _ => (15.0, -10.0),
            }
        };
        let pres = |u: f64| -> f64 {
            match o.pattern {
                Pattern::PressureRamp => (u * PI).sin().max(0.002),
                Pattern::Hover => 0.0,
                _ => o.pressure,
            }
        };
        let mk = |t_us: u64, u: f64, p: f64, d: f64, b1: bool, b2: bool| {
            let (x, y) = path(u);
            let (tx, ty) = tilt(u);
            Sample { t_us, tool: Some(tool), x, y, p, tx, ty, d, b1, b2 }
        };
        let btn = |i: u64| matches!(o.pattern, Pattern::Buttons) && (i / 20) % 2 == 1;
        // Hover in (pressure 0, distance falling) — libinput derives contact from pressure.
        for i in 0..hover_n {
            let d = 1.0 - i as f64 / hover_n as f64;
            out.push(mk(t, 0.0, 0.0, d.max(0.05), btn(i), false));
            t += dt_us;
        }
        for i in 0..n {
            let u = i as f64 / (n - 1) as f64;
            out.push(mk(t, u, pres(u), 0.0, btn(i), matches!(o.pattern, Pattern::Buttons) && i > n / 2));
            t += dt_us;
        }
        for i in 0..hover_n {
            let d = (i + 1) as f64 / hover_n as f64;
            out.push(mk(t, 1.0, 0.0, d, false, false));
            t += dt_us;
        }
        out.push(Sample { t_us: t, tool: None, x: 0.0, y: 0.0, p: 0.0, tx: 0.0, ty: 0.0, d: 1.0, b1: false, b2: false });
        t += o.gap_ms * 1000;
    }
    Ok(Strokes {
        header: Header { format: "simtab-strokes".into(), version: 1, note: format!("generated: {o:?}") },
        samples: out,
    })
}
