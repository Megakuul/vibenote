#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
//! Numeric render checks that goldens alone cannot prove.

use vibenote_model::{Canvas, CanvasObject, Color, ObjectId, ObjectKind, Sample, Stroke, Tool};
use vibenote_render::{Headless, Viewport, compare};

fn stroke(pts: &[(f64, f64, f32)], width: f32) -> Stroke {
    let samples = pts
        .iter()
        .enumerate()
        .map(|(i, &(x, y, p))| Sample {
            x,
            y,
            pressure: p,
            tilt_x: 0.0,
            tilt_y: 0.0,
            t_us: i as u64 * 5000,
        })
        .collect();
    Stroke {
        tool: Tool::Pen,
        color: Color(0x000000ff),
        width,
        samples,
    }
}

fn canvas(s: Stroke) -> Canvas {
    let mut c = Canvas::default();
    c.objects.insert(
        ObjectId(1),
        CanvasObject {
            id: ObjectId(1),
            z: 0,
            kind: ObjectKind::Stroke(s),
        },
    );
    c
}

/// R-CANVAS-001: rendering far from the origin is identical to rendering near it.
#[test]
fn far_from_origin_renders_identically() {
    let mut r = Headless::new().unwrap();
    let pts: Vec<(f64, f64, f32)> = (0..200)
        .map(|i| {
            let t = f64::from(i) / 199.0;
            (50.0 + 300.0 * t, 100.0 + 60.0 * (t * 12.0).sin(), 0.3 + 0.6 * t as f32)
        })
        .collect();
    let near = r
        .render(
            &canvas(stroke(&pts, 4.0)),
            &[],
            &Viewport {
                origin_x: 0.0,
                origin_y: 0.0,
                zoom: 2.0,
                width: 800,
                height: 400,
            },
        )
        .unwrap();
    for off in [1.0e7, -1.0e7] {
        let far_pts: Vec<_> = pts.iter().map(|&(x, y, p)| (x + off, y - off, p)).collect();
        let far = r
            .render(
                &canvas(stroke(&far_pts, 4.0)),
                &[],
                &Viewport {
                    origin_x: off,
                    origin_y: -off,
                    zoom: 2.0,
                    width: 800,
                    height: 400,
                },
            )
            .unwrap();
        let d = compare(&near, &far, 8).unwrap();
        assert!(
            d.differing <= 20,
            "offset {off}: {} pixels differ (max diff {})",
            d.differing,
            d.max_channel_diff
        );
    }
}

/// R-PEN-002: rendered stroke thickness follows `pressure_width` (±0.25 canvas px at zoom 1 → ±2.5 px at zoom 10).
#[test]
fn stroke_width_follows_pressure_curve() {
    let mut r = Headless::new().unwrap();
    let zoom = 10.0;
    for p in [0.1f32, 0.4, 0.7, 1.0] {
        let s = stroke(&[(5.0, 20.0, p), (55.0, 20.0, p)], 3.0);
        let img = r
            .render(
                &canvas(s),
                &[],
                &Viewport {
                    origin_x: 0.0,
                    origin_y: 0.0,
                    zoom,
                    width: 600,
                    height: 400,
                },
            )
            .unwrap();
        // column x = 300 px (canvas x = 30): sum coverage = 1 - luminance/paper
        let col = 300usize;
        let mut cov = 0.0f64;
        for y in 0..img.height as usize {
            let i = (y * img.width as usize + col) * 4;
            let lum = f64::from(img.rgba[i]) / 251.0; // paper red channel = 251
            cov += (1.0 - lum).clamp(0.0, 1.0);
        }
        let expected = f64::from(vibenote_ink::pressure_width(3.0, p)) * zoom;
        assert!(
            (cov - expected).abs() <= 2.5,
            "p={p}: measured {cov:.2} px, expected {expected:.2} px"
        );
    }
}
