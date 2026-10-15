#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! GPU-resident chunked ink (M3.1, ADR 0011): same pictures as the stateless per-segment
//! renderer, correct after changes, bounded work per frame, bounded memory.

use proptest::prelude::*;
use vibenote_model::{Canvas, CanvasObject, Color, ObjectId, ObjectKind, Sample, Stroke, Tool};
use vibenote_render::{FrameBudget, Headless, Image, Viewport, compare};

fn stroke(pts: &[(f64, f64, f32)], width: f32, color: u32, tool: Tool) -> Stroke {
    Stroke {
        tool,
        color: Color(color),
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
                    t_us: i as u64 * 1000,
                }
                .quantised()
            })
            .collect(),
    }
}

fn put(c: &mut Canvas, id: u64, z: i64, s: Stroke) {
    c.objects.insert(
        ObjectId(id),
        CanvasObject {
            id: ObjectId(id),
            z,
            kind: ObjectKind::Stroke(s),
        },
    );
}

fn vp(x: f64, y: f64, zoom: f64, w: u32, h: u32) -> Viewport {
    Viewport {
        origin_x: x,
        origin_y: y,
        zoom,
        width: w,
        height: h,
    }
}

/// The goldens' rule: channel tolerance 6, at most 0.1 % of pixels differ.
fn assert_close(a: &Image, b: &Image, what: &str) {
    let d = compare(a, b, 6).unwrap();
    assert!(
        d.fraction() <= 0.001,
        "{what}: {} pixels differ (max {})",
        d.differing,
        d.max_channel_diff
    );
}

/// A wavy stroke of `n` samples from (x, y), moving right, pressure rising.
fn wave(x: f64, y: f64, n: usize, len: f64, amp: f64) -> Vec<(f64, f64, f32)> {
    (0..n)
        .map(|j| {
            let u = j as f64 / (n.max(2) - 1) as f64;
            (x + len * u, y + amp * (u * 9.0).sin(), (0.3 + 0.6 * u) as f32)
        })
        .collect()
}

/// The same with constant pressure: every radius equals the stroke's largest, which the 8-bit
/// radius stores exactly, so the comparison checks geometry, seams and order exactly.
fn flat_wave(x: f64, y: f64, n: usize, len: f64, amp: f64, p: f32) -> Vec<(f64, f64, f32)> {
    wave(x, y, n, len, amp).into_iter().map(|(x, y, _)| (x, y, p)).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    /// Random pen canvases around cell seams, random z order and colours: the chunked renderer
    /// draws what the per-segment renderer draws.
    #[test]
    fn chunked_matches_the_reference(
        strokes in prop::collection::vec((1900.0f64..2200.0, 1900.0f64..2200.0, 2usize..60, 5.0f64..300.0, -20i64..20, 0u32..4, 0.5f32..12.0, 0.1f32..=1.0), 1..40),
        zoom in prop_oneof![Just(0.5f64), Just(1.0), Just(2.5), Just(4.0)],
    ) {
        let mut c = Canvas::default();
        let colors = [0x1e1714ff_u32, 0xd03030ff, 0x3050d0ff, 0x20a040ff];
        for (k, (x, y, n, len, z, col, w, p)) in strokes.into_iter().enumerate() {
            put(&mut c, k as u64 + 1, z, stroke(&flat_wave(x, y, n, len, 15.0, p), w, colors[col as usize], Tool::Pen));
        }
        let mut r = Headless::new().unwrap();
        let v = vp(2048.0 - 300.0 / zoom, 2048.0 - 200.0 / zoom, zoom, 600, 400);
        // Settled: below zoom 1 the frame is tiled and fills over several frames.
        let a = r.render_settled(&c, &[], &v, 200).unwrap();
        let b = r.render_reference(&c, &v).unwrap();
        let d = compare(&a, &b, 6).unwrap();
        prop_assert!(d.fraction() <= 0.001, "{} pixels differ (max {})", d.differing, d.max_channel_diff);
    }
}

/// Radii are stored in 8 bits relative to the stroke's largest radius (ADR 0011): the error is
/// at most r_max/510 per point, and the picture differs from the reference by no more than
/// that (doubled where segment ends overlap) on the ink's edge, at any zoom.
#[test]
fn radius_error_stays_within_the_8_bit_bound() {
    let mut r = Headless::new().unwrap();
    for (zoom, width) in [(1.0, 3.0f32), (4.0, 11.3), (10.0, 6.0)] {
        let mut c = Canvas::default();
        put(
            &mut c,
            1,
            0,
            stroke(&wave(1900.0, 2072.8, 25, 241.0, 15.0), width, 0x1e1714ff, Tool::Pen),
        );
        let v = vp(2048.0 - 300.0 / zoom, 2048.0 - 200.0 / zoom, zoom, 600, 400);
        let a = r.render(&c, &[], &v).unwrap();
        let b = r.render_reference(&c, &v).unwrap();
        let r_max_px = f64::from(vibenote_ink::pressure_width(width, 0.9)) * 0.5 * zoom;
        // paper (251) to ink (≈30): one pixel of coverage is ≈ 221 levels
        let bound = 2.0 + (2.0 * 221.0 * r_max_px / 510.0).ceil();
        let d = compare(&a, &b, 0).unwrap();
        assert!(
            f64::from(d.max_channel_diff) <= bound,
            "zoom {zoom}: max diff {} > {bound}",
            d.max_channel_diff
        );
    }
}

/// Two strokes cross a seam; the one with the higher z is on top on both sides of it, even
/// though its id is lower.
#[test]
fn z_order_holds_across_a_seam() {
    let mut c = Canvas::default();
    put(
        &mut c,
        1,
        5,
        stroke(
            &[(2000.0, 1000.0, 1.0), (2100.0, 1000.0, 1.0)],
            12.0,
            0xd03030ff,
            Tool::Pen,
        ),
    );
    put(
        &mut c,
        2,
        1,
        stroke(
            &[(2048.0, 950.0, 1.0), (2048.0, 1050.0, 1.0)],
            12.0,
            0x3050d0ff,
            Tool::Pen,
        ),
    );
    let mut r = Headless::new().unwrap();
    for zoom in [1.0, 4.0] {
        let v = vp(2048.0 - 50.0 / zoom, 1000.0 - 50.0 / zoom, zoom, 100, 100);
        let a = r.render(&c, &[], &v).unwrap();
        assert_close(&a, &r.render_reference(&c, &v).unwrap(), "seam");
        let mid = ((50 * 100 + 50) * 4) as usize;
        assert!(
            a.rgba[mid] > 150 && a.rgba[mid + 2] < 100,
            "red (z 5) on top: {:?}",
            &a.rgba[mid..mid + 4]
        );
    }
}

/// A canvas too large to hash every frame: 6,000 strokes on a grid.
fn large_canvas() -> Canvas {
    let mut c = Canvas::default();
    for k in 0..6000u64 {
        let (x, y) = ((k % 80) as f64 * 60.0, (k / 80) as f64 * 40.0);
        put(
            &mut c,
            k + 1,
            k as i64,
            stroke(&wave(x, y, 12, 45.0, 6.0), 2.0, 0x1e1714ff, Tool::Pen),
        );
    }
    c
}

/// Changes to a large canvas show up: appended, removed, and (with `invalidate`) edited in
/// place; the settled picture equals a fresh renderer's.
#[test]
fn large_canvas_changes_are_drawn() {
    let mut c = large_canvas();
    let v = vp(1000.0, 500.0, 1.0, 800, 600);
    let mut r = Headless::new().unwrap();
    r.render_settled(&c, &[], &v, 100).unwrap();
    // Append (the tail fast path) and remove one in the middle (length changes → verify pass).
    put(
        &mut c,
        9001,
        9001,
        stroke(&wave(1100.0, 600.0, 30, 300.0, 40.0), 6.0, 0xd03030ff, Tool::Pen),
    );
    c.objects.remove(&ObjectId(1300));
    let got = r.render_settled(&c, &[], &v, 100).unwrap();
    let want = Headless::new().unwrap().render_settled(&c, &[], &v, 100).unwrap();
    assert_eq!(got.rgba, want.rgba, "append + remove");
    // In-place edit of a middle object keeps the O(1) key: documented, needs `invalidate`.
    if let Some(o) = c.objects.get_mut(&ObjectId(1290)) {
        o.kind = ObjectKind::Stroke(stroke(
            &wave(1150.0, 640.0, 30, 200.0, 30.0),
            9.0,
            0x3050d0ff,
            Tool::Pen,
        ));
    }
    r.invalidate();
    let got = r.render_settled(&c, &[], &v, 100).unwrap();
    let want = Headless::new().unwrap().render_settled(&c, &[], &v, 100).unwrap();
    assert_eq!(got.rgba, want.rgba, "edit in place + invalidate");
}

/// Each frame stays within the budget; the canvas completes over frames and then equals the
/// unlimited render.
#[test]
fn work_per_frame_is_bounded() {
    let c = large_canvas();
    let v = vp(0.0, 0.0, 0.25, 1200, 1000);
    let mut r = Headless::new().unwrap();
    let mut frames = 0;
    let last = loop {
        let (img, s) = r.render_stats(&c, &[], &v).unwrap();
        frames += 1;
        assert!(
            s.ingested_samples <= u64::from(FrameBudget::HEADLESS.ingest_samples) + 12,
            "{s:?}"
        );
        assert!(
            s.upload_bytes <= FrameBudget::HEADLESS.upload_bytes + 32768 * 2,
            "{s:?}"
        );
        if s.complete {
            break img;
        }
        assert!(frames < 100, "never completed");
    };
    assert!(frames >= 2, "6,000 strokes × 12 samples need several frames ({frames})");
    // `complete` includes the LOD lists (zoom 0.25 draws level 3), so compare settled renders.
    let mut u = Headless::new().unwrap();
    assert_eq!(u.render_settled(&c, &[], &v, 100).unwrap().rgba, last.rgba);
}

/// Off-screen chunks are not drawn.
#[test]
fn culling_skips_invisible_chunks() {
    let c = large_canvas();
    let mut r = Headless::new().unwrap();
    r.set_budget(FrameBudget::UNLIMITED);
    let (img, s) = r.render_stats(&c, &[], &vp(1.0e6, 1.0e6, 1.0, 200, 200)).unwrap();
    assert!(s.chunks > 0 && s.chunks_visible == 0, "{s:?}");
    assert!(img.rgba.chunks(4).all(|p| p == vibenote_render::PAPER));
}

/// With a small memory cap, invisible chunks are evicted and come back when visible again.
#[test]
fn eviction_bounds_memory_and_restores_content() {
    let c = large_canvas();
    let mut r = Headless::new().unwrap();
    r.set_gpu_cap(256 << 10);
    let a = vp(0.0, 0.0, 1.0, 400, 300);
    let first = r.render_settled(&c, &[], &a, 100).unwrap();
    for x in [2500.0, 4000.0] {
        let (_, s) = r.render_stats(&c, &[], &vp(x, 2000.0, 1.0, 400, 300)).unwrap();
        // the cap holds, apart from the chunks in view
        assert!(s.gpu_bytes <= (256 << 10) + 4 * (2 << 20), "{s:?}");
    }
    let back = r.render_settled(&c, &[], &a, 100).unwrap();
    assert_eq!(back.rgba, first.rgba);
}

/// Overlapping highlighter strokes of one colour do not darken (owner decision "No
/// darkening", 2026-10-13), and pen ink is drawn above highlighter ink.
#[test]
fn highlighter_overlaps_do_not_darken_and_stay_below_pen() {
    let one = {
        let mut c = Canvas::default();
        put(
            &mut c,
            1,
            0,
            stroke(
                &[(20.0, 50.0, 1.0), (180.0, 50.0, 1.0)],
                20.0,
                0xf0d000ff,
                Tool::Highlighter,
            ),
        );
        c
    };
    let mut two = one.clone();
    put(
        &mut two,
        2,
        1,
        stroke(
            &[(100.0, 10.0, 1.0), (100.0, 90.0, 1.0)],
            20.0,
            0xf0d000ff,
            Tool::Highlighter,
        ),
    );
    let mut r = Headless::new().unwrap();
    let v = vp(0.0, 0.0, 1.0, 200, 100);
    let a = r.render(&one, &[], &v).unwrap();
    let b = r.render(&two, &[], &v).unwrap();
    let at = |img: &Image, x: usize, y: usize| img.rgba[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4].to_vec();
    assert_eq!(
        at(&b, 100, 50),
        at(&a, 60, 50),
        "crossing is as light as a single stroke"
    );
    // A pen stroke with a lower z than the highlighter is still on top of it.
    let mut c = one.clone();
    c.objects.get_mut(&ObjectId(1)).unwrap().z = 10;
    put(
        &mut c,
        3,
        0,
        stroke(&[(100.0, 30.0, 1.0), (100.0, 70.0, 1.0)], 8.0, 0x000000ff, Tool::Pen),
    );
    let img = r.render(&c, &[], &v).unwrap();
    assert!(
        at(&img, 100, 50)[..3].iter().all(|&ch| ch < 20),
        "{:?}",
        at(&img, 100, 50)
    );
}

#[test]
fn hostile_strokes_render_without_panic_and_with_bounded_memory() {
    let mut c = Canvas::default();
    let huge: Vec<_> = (0..1_000_000)
        .map(|i| ((i % 5000) as f64, (i / 5000) as f64 * 3.0, 0.5f32))
        .collect();
    put(&mut c, 1, 0, stroke(&huge, 2.0, 0x000000ff, Tool::Pen));
    put(
        &mut c,
        2,
        1,
        stroke(&[(0.0, 0.0, 1.0), (300.0, 300.0, 1.0)], 9999.0, 0x000000ff, Tool::Pen),
    );
    put(
        &mut c,
        3,
        2,
        stroke(
            &[(-1.0e9, -1.0e9, 1.0), (1.0e9, 1.0e9, 1.0)],
            2.0,
            0x000000ff,
            Tool::Pen,
        ),
    );
    put(
        &mut c,
        4,
        3,
        stroke(&[(1.0e9, -1.0e9, 1.0)], 2.0, 0x000000ff, Tool::Highlighter),
    );
    let mut r = Headless::new().unwrap();
    let v = vp(0.0, 0.0, 0.1, 400, 400);
    let mut done = false;
    for _ in 0..400 {
        let (_, s) = r.render_stats(&c, &[], &v).unwrap();
        assert!(s.gpu_bytes < 600 << 20, "{s:?}");
        if s.complete {
            done = true;
            break;
        }
    }
    assert!(done);
}

/// Same input, same picture: every frame of the direct path (budgets are counts, not clocks),
/// and every settled frame. (Transient tiled frames may differ: they depend on when the LOD
/// worker delivers, design §4.1.)
#[test]
fn rendering_is_deterministic() {
    let c = large_canvas();
    let frames = |c: &Canvas| {
        let mut r = Headless::new().unwrap();
        let mut v: Vec<Vec<u8>> = (0..6)
            .map(|k| {
                r.render(c, &[], &vp(f64::from(k) * 100.0, 0.0, 1.0, 300, 200))
                    .unwrap()
                    .rgba
            })
            .collect();
        v.push(
            r.render_settled(c, &[], &vp(0.0, 0.0, 0.3, 300, 200), 300)
                .unwrap()
                .rgba,
        );
        v
    };
    assert_eq!(frames(&c), frames(&c));
}

/// Tracked mode (the app, M3.1b): with the canvas's change journal, edits anywhere in a large
/// canvas show up, in-place edits included, without `invalidate`.
#[test]
fn tracked_canvas_edits_are_drawn() {
    use vibenote_model::ChangeLog;
    use vibenote_render::Scene;
    let mut c = large_canvas();
    let mut log = ChangeLog::new(1);
    let v = vp(1000.0, 500.0, 1.0, 800, 600);
    let mut r = Headless::new().unwrap();
    let settle = |r: &mut Headless, c: &Canvas, log: &ChangeLog| {
        for _ in 0..200 {
            let (img, s) = r
                .render_scene(&Scene {
                    canvas: c,
                    changes: Some(log),
                    live: &[],
                    viewport: v,
                })
                .unwrap();
            if s.complete {
                return img;
            }
        }
        panic!("tracked render did not complete");
    };
    settle(&mut r, &c, &log);
    // In-place edit in the middle (same length, same first/last object), a low-z insert (order
    // fix), a removal and a z change.
    c.objects.get_mut(&ObjectId(1290)).unwrap().kind = ObjectKind::Stroke(stroke(
        &wave(1150.0, 640.0, 30, 200.0, 30.0),
        9.0,
        0x3050d0ff,
        Tool::Pen,
    ));
    log.record(ObjectId(1290));
    put(
        &mut c,
        9100,
        -5,
        stroke(&wave(1100.0, 600.0, 30, 300.0, 40.0), 14.0, 0xd03030ff, Tool::Pen),
    );
    log.record(ObjectId(9100));
    c.objects.remove(&ObjectId(1301));
    log.record(ObjectId(1301));
    c.objects.get_mut(&ObjectId(1282)).unwrap().z = 100_000;
    log.record(ObjectId(1282));
    let got = settle(&mut r, &c, &log);
    let want = Headless::new().unwrap().render_settled(&c, &[], &v, 100).unwrap();
    assert_eq!(got.rgba, want.rgba);
    // A journal that overflowed is resynchronised by a verify pass.
    for k in 0..ChangeLog::CAPACITY as u64 + 5 {
        log.record(ObjectId(1_000_000 + k));
    }
    c.objects.remove(&ObjectId(1295));
    let got = settle(&mut r, &c, &log);
    let want = Headless::new().unwrap().render_settled(&c, &[], &v, 100).unwrap();
    assert_eq!(got.rgba, want.rgba, "after overflow");
}

/// Ink pixels of `a` that have no ink pixel within `r` px in `b` (luminance < 200 = ink).
fn unmatched_ink(a: &Image, b: &Image, r: i64) -> usize {
    let (w, h) = (i64::from(a.width), i64::from(a.height));
    let ink =
        |img: &Image, x: i64, y: i64| x >= 0 && y >= 0 && x < w && y < h && img.rgba[((y * w + x) * 4) as usize] < 200;
    let mut n = 0;
    for y in 0..h {
        for x in 0..w {
            if ink(a, x, y) && !(-r..=r).any(|dy| (-r..=r).any(|dx| ink(b, x + dx, y + dy))) {
                n += 1;
            }
        }
    }
    n
}

/// Zoomed out, strokes are drawn from their LOD lists (M3.2): the picture stays within about a
/// pixel of full detail, both ways, and it is the same every time once settled.
#[test]
fn lod_stays_within_a_pixel_of_full_detail() {
    let mut c = Canvas::default();
    for k in 0..800u64 {
        let (x, y) = ((k % 40) as f64 * 120.0, (k / 40) as f64 * 80.0);
        let pts: Vec<_> = (0..90)
            .map(|j| {
                let u = f64::from(j) / 89.0;
                let a = u * 5.0 * std::f64::consts::TAU;
                (
                    x + 90.0 * u - 6.0 * a.sin(),
                    y - 12.0 * (1.0 - a.cos()),
                    (0.4 + 0.5 * u) as f32,
                )
            })
            .collect();
        put(&mut c, k + 1, k as i64, stroke(&pts, 3.0, 0x1e1714ff, Tool::Pen));
    }
    let v = vp(-50.0, -50.0, 0.2, 1000, 400);
    let mut r = Headless::new().unwrap();
    let lod = r.render_settled(&c, &[], &v, 200).unwrap();
    let full = r.render_reference(&c, &v).unwrap();
    assert_ne!(lod.rgba, full.rgba, "level 3 is in use at zoom 0.2");
    assert_eq!(unmatched_ink(&lod, &full, 2), 0, "LOD ink far from full-detail ink");
    assert_eq!(unmatched_ink(&full, &lod, 2), 0, "full-detail ink missing in LOD");
    let again = Headless::new().unwrap().render_settled(&c, &[], &v, 200).unwrap();
    assert_eq!(again.rgba, lod.rgba, "settled LOD frames are deterministic");
}

/// Regression: canvases that differ only in tilt are different pictures for the highlighter
/// (R-PEN-003); change detection must see it.
#[test]
fn a_tilt_change_is_a_change() {
    let hl = |tilt: f32| {
        let mut c = Canvas::default();
        let mut s = stroke(
            &[(5.0, 20.0, 0.6), (55.0, 20.0, 0.6)],
            3.0,
            0x1e1714ff,
            Tool::Highlighter,
        );
        for p in &mut s.samples {
            p.tilt_x = tilt;
        }
        put(&mut c, 1, 0, s);
        c
    };
    let v = vp(0.0, 0.0, 10.0, 600, 400);
    let mut r = Headless::new().unwrap();
    let flat = r.render(&hl(0.0), &[], &v).unwrap();
    let tilted = r.render(&hl(60.0), &[], &v).unwrap();
    assert_eq!(
        tilted.rgba,
        Headless::new().unwrap().render(&hl(60.0), &[], &v).unwrap().rgba
    );
    assert_ne!(flat.rgba, tilted.rgba);
}

/// Regression (M3 review H1): below zoom ≈ 0.027 cells draw unscissored and skip seam copies;
/// LOD must not merge owned and seam segments, or long strokes lose most of their ink.
#[test]
fn lod_keeps_strokes_that_cross_cells_at_the_lowest_zooms() {
    let mut c = Canvas::default();
    // straight and wavy strokes over many cells
    put(
        &mut c,
        1,
        0,
        stroke(
            &(0..2000).map(|i| (f64::from(i) * 10.0, 500.0, 0.6)).collect::<Vec<_>>(),
            30.0,
            0x1e1714ff,
            Tool::Pen,
        ),
    );
    for k in 0..30u64 {
        let pts = wave(
            -3000.0 + k as f64 * 700.0,
            1500.0 + k as f64 * 300.0,
            400,
            9000.0,
            900.0,
        );
        put(&mut c, k + 2, k as i64 + 1, stroke(&pts, 40.0, 0x3050d0ff, Tool::Pen));
    }
    let mut r = Headless::new().unwrap();
    for zoom in [0.01, 0.015, 0.02, 0.025] {
        let v = vp(-4000.0, -1000.0, zoom, 600, 300);
        let lod = r.render_settled(&c, &[], &v, 200).unwrap();
        let full = r.render_reference(&c, &v).unwrap();
        assert_eq!(unmatched_ink(&full, &lod, 2), 0, "zoom {zoom}: ink missing with LOD");
        assert_eq!(unmatched_ink(&lod, &full, 2), 0, "zoom {zoom}: extra ink with LOD");
    }
}

/// Regression (M3 review M1): a chunk rebuilt while one of its members has left the canvas
/// must not keep that member's old slot (it would later hide another stroke).
#[test]
fn rebuild_forgets_slots_of_removed_members() {
    let mut c = Canvas::default();
    for k in 0..5000u64 {
        let (x, y) = ((k % 100) as f64 * 30.0, (k / 100) as f64 * 30.0);
        put(
            &mut c,
            k + 1,
            k as i64 + 10,
            stroke(&wave(x, y, 8, 20.0, 4.0), 2.0, 0x1e1714ff, Tool::Pen),
        );
    }
    // the view covers both cells around the seam at x = 2,048
    let v = vp(1700.0, 0.0, 1.0, 900, 500);
    let mut r = Headless::new().unwrap();
    r.render_settled(&c, &[], &v, 200).unwrap();
    // A low-z stroke across cells (0,0) and (1,0) breaks both chunks' order. One budgeted
    // frame rebuilds the first; the second stays queued while one of its members (id 81, at
    // x = 2,400) leaves the canvas, then everything settles.
    put(
        &mut c,
        9000,
        0,
        stroke(&wave(1900.0, 5.0, 30, 400.0, 60.0), 8.0, 0xd03030ff, Tool::Pen),
    );
    let (_, s) = r.render_stats(&c, &[], &v).unwrap();
    assert!(!s.complete, "the second rebuild must still be queued");
    c.objects.remove(&ObjectId(81));
    let got = r.render_settled(&c, &[], &v, 200).unwrap();
    let want = Headless::new().unwrap().render_settled(&c, &[], &v, 200).unwrap();
    assert_eq!(got.rgba, want.rgba);
}

/// A canvas with many strokes spread over many cells (zoomed-out views are tiled).
fn spread_canvas() -> Canvas {
    let mut c = Canvas::default();
    for k in 0..3000u64 {
        let (x, y) = ((k % 60) as f64 * 160.0, (k / 60) as f64 * 120.0);
        put(
            &mut c,
            k + 1,
            k as i64,
            stroke(&wave(x, y, 30, 120.0, 25.0), 6.0, 0x1e1714ff, Tool::Pen),
        );
    }
    c
}

/// Zoomed out, the frame is composited from tiles (M3.3): settled, it stays within a pixel of
/// full detail, and every frame fills at most the tile budget.
#[test]
fn tiles_match_full_detail_and_respect_the_fill_budget() {
    let c = spread_canvas();
    for zoom in [0.125, 0.1, 0.03] {
        let v = vp(-100.0, -100.0, zoom, 700, 500);
        let mut r = Headless::new().unwrap();
        let mut frames = 0;
        let tiled = loop {
            let (img, s) = r.render_stats(&c, &[], &v).unwrap();
            assert!(s.tiled, "zoom {zoom} is tiled");
            assert!(s.tiles_filled <= FrameBudget::HEADLESS.tile_fills, "{s:?}");
            frames += 1;
            if s.complete {
                break img;
            }
            assert!(frames < 500, "tiles never completed");
        };
        let full = r.render_reference(&c, &v).unwrap();
        assert_eq!(unmatched_ink(&full, &tiled, 2), 0, "zoom {zoom}: ink missing in tiles");
        assert_eq!(unmatched_ink(&tiled, &full, 2), 0, "zoom {zoom}: extra ink in tiles");
    }
}

/// A stroke committed while zoomed out shows in the next frame (its tile is refilled first);
/// a removed one disappears; the live stroke is drawn on top of tiles at once.
#[test]
fn tiled_frames_show_changes_at_once() {
    use vibenote_model::ChangeLog;
    use vibenote_render::Scene;
    let mut c = spread_canvas();
    let mut log = ChangeLog::new(1);
    let v = vp(0.0, 0.0, 0.1, 600, 400);
    let mut r = Headless::new().unwrap();
    let frame = |r: &mut Headless, c: &Canvas, log: &ChangeLog, live: &[&Stroke]| {
        r.render_scene(&Scene {
            canvas: c,
            changes: Some(log),
            live,
            viewport: v,
        })
        .unwrap()
    };
    for _ in 0..300 {
        if frame(&mut r, &c, &log, &[]).1.complete {
            break;
        }
    }
    let red = |img: &Image| img.rgba.chunks(4).filter(|p| p[0] > 150 && p[1] < 90).count();
    let before = frame(&mut r, &c, &log, &[]).0;
    assert_eq!(red(&before), 0);
    // in empty space below the content
    put(
        &mut c,
        9000,
        9000,
        stroke(&wave(500.0, 3300.0, 40, 3000.0, 200.0), 60.0, 0xd03030ff, Tool::Pen),
    );
    log.record(ObjectId(9000));
    let after = frame(&mut r, &c, &log, &[]).0;
    assert!(
        red(&after) > 50,
        "new stroke visible in the next frame ({} px)",
        red(&after)
    );
    c.objects.remove(&ObjectId(9000));
    log.record(ObjectId(9000));
    let gone = frame(&mut r, &c, &log, &[]).0;
    assert_eq!(red(&gone), 0, "removed stroke gone in the next frame");
    let live = stroke(&wave(500.0, 3300.0, 40, 3000.0, 200.0), 60.0, 0xd03030ff, Tool::Pen);
    let mut fresh = Headless::new().unwrap();
    let img = frame(&mut fresh, &c, &log, &[&live]).0;
    assert!(red(&img) > 50, "live ink is never tiled, so it shows in frame 1");
}

/// A jump to a new area at a new zoom shows a coarser cached tile (blurry) while the tiles are
/// filled, never blank paper where there is ink.
#[test]
fn missing_tiles_fall_back_to_coarser_ones() {
    let c = spread_canvas();
    let mut r = Headless::new().unwrap();
    // settle the overview first (it keeps the coarsest levels filled)
    r.render_settled(&c, &[], &vp(-500.0, -500.0, 0.03, 600, 400), 500)
        .unwrap();
    let v = vp(4000.0, 1500.0, 0.2, 600, 400);
    let (img, s) = r.render_stats(&c, &[], &v).unwrap();
    assert!(s.tiles_missing > 0, "{s:?}");
    let ink = img.rgba.chunks(4).filter(|p| p[0] < 200).count();
    assert!(ink > 1000, "fallback shows the content ({ink} ink px)");
    let settled = r.render_settled(&c, &[], &v, 500).unwrap();
    assert_eq!(unmatched_ink(&settled, &r.render_reference(&c, &v).unwrap(), 2), 0);
}

/// Frames where only live ink changes reuse the cached committed frame, and look exactly like a
/// full redraw.
#[test]
fn pen_frames_reuse_the_committed_frame() {
    let c = large_canvas();
    let v = vp(500.0, 300.0, 1.0, 600, 400);
    let mut r = Headless::new().unwrap();
    r.render_settled(&c, &[], &v, 200).unwrap();
    let mut live = stroke(&[(600.0, 400.0, 0.5)], 3.0, 0xd03030ff, Tool::Pen);
    for k in 1..20 {
        live.samples.push(Sample {
            x: 600.0 + f64::from(k) * 5.0,
            y: 400.0,
            pressure: 0.5,
            tilt_x: 0.0,
            tilt_y: 0.0,
            t_us: 0,
        });
        let (img, s) = r.render_stats(&c, &[&live], &v).unwrap();
        assert!(
            s.cached,
            "frame {k}: committed ink redrawn although only live ink changed"
        );
        let mut fresh = Headless::new().unwrap();
        fresh.render_settled(&c, &[], &v, 200).unwrap();
        assert_eq!(img.rgba, fresh.render(&c, &[&live], &v).unwrap().rgba);
    }
    // A change to the view or the canvas redraws it.
    let (_, s) = r.render_stats(&c, &[&live], &vp(510.0, 300.0, 1.0, 600, 400)).unwrap();
    assert!(!s.cached);
}
