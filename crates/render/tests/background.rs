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
//! Note backgrounds (M4.8, R-CANVAS-006): numeric checks of the pattern under the ink.

use vibenote_model::settings::{Background, BackgroundKind};
use vibenote_model::{Canvas, CanvasObject, Color, ObjectId, ObjectKind, Sample, Stroke, Tool};
use vibenote_render::{Headless, Image, PAPER, Viewport, compare};

fn vp(ox: f64, oy: f64, zoom: f64) -> Viewport {
    Viewport {
        origin_x: ox,
        origin_y: oy,
        zoom,
        width: 256,
        height: 192,
    }
}

fn bg(kind: BackgroundKind, spacing: f64) -> Background {
    Background { kind, spacing }
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
}

fn is_paper(p: [u8; 4]) -> bool {
    (0..3).all(|c| p[c].abs_diff(PAPER[c]) <= 1)
}

fn render(r: &mut Headless, b: Background, v: &Viewport) -> Image {
    r.set_background(b);
    r.render_settled(&Canvas::default(), &[], v, 100).unwrap()
}

/// Rows (or columns) whose pixels differ from paper, sampled at one column (row).
fn marked_rows(img: &Image, x: u32) -> Vec<u32> {
    (0..img.height).filter(|&y| !is_paper(px(img, x, y))).collect()
}

#[test]
fn blank_is_plain_paper() {
    let mut r = Headless::new().unwrap();
    let img = render(&mut r, bg(BackgroundKind::Blank, 32.0), &vp(0.0, 0.0, 1.0));
    assert!(img.rgba.chunks(4).all(|p| is_paper([p[0], p[1], p[2], p[3]])));
}

#[test]
fn lines_sit_on_multiples_of_the_spacing_as_crisp_hairlines() {
    let mut r = Headless::new().unwrap();
    // Origin 10 → canvas lines at y = 32k appear at screen rows 22, 54, 86, …
    let img = render(&mut r, bg(BackgroundKind::Lined, 32.0), &vp(0.0, 10.0, 1.0));
    let rows = marked_rows(&img, 100);
    let want: Vec<u32> = (0..6).map(|k| 22 + 32 * k).filter(|&y| y < 192).collect();
    assert_eq!(rows, want);
    // Lined has no vertical lines.
    assert!((0..img.width).all(|x| is_paper(px(&img, x, 30))));
    // Zoom 2 doubles the on-screen spacing.
    let img = render(&mut r, bg(BackgroundKind::Lined, 32.0), &vp(0.0, 0.0, 2.0));
    assert_eq!(marked_rows(&img, 7), vec![0, 64, 128]);
}

#[test]
fn grid_and_dots_mark_both_axes() {
    let mut r = Headless::new().unwrap();
    let g = render(&mut r, bg(BackgroundKind::Grid, 40.0), &vp(0.0, 0.0, 1.0));
    assert!(!is_paper(px(&g, 40, 13)), "vertical line");
    assert!(!is_paper(px(&g, 13, 40)), "horizontal line");
    assert!(is_paper(px(&g, 13, 13)));
    let d = render(&mut r, bg(BackgroundKind::Dots, 40.0), &vp(0.0, 0.0, 1.0));
    assert!(!is_paper(px(&d, 40, 40)), "dot at the intersection");
    assert!(
        is_paper(px(&d, 40, 13)) && is_paper(px(&d, 13, 40)),
        "no lines between dots"
    );
}

/// R-CANVAS-001 for backgrounds: far from the origin the pattern is identical.
#[test]
fn far_origins_render_the_same_pattern() {
    let mut r = Headless::new().unwrap();
    for kind in [BackgroundKind::Lined, BackgroundKind::Grid, BackgroundKind::Dots] {
        let b = bg(kind, 25.0);
        let near = render(&mut r, b, &vp(5.0, 7.0, 1.5));
        let far = render(&mut r, b, &vp(5.0 + 25.0 * 4.0e9, 7.0 - 25.0 * 3.0e9, 1.5));
        let d = compare(&near, &far, 0).unwrap();
        assert_eq!(d.differing, 0, "{kind:?}");
    }
}

#[test]
fn dense_patterns_fade_out_when_zoomed_out() {
    let mut r = Headless::new().unwrap();
    // 32 units at zoom 0.1 = 3.2 px: gone.
    let img = render(&mut r, bg(BackgroundKind::Grid, 32.0), &vp(0.0, 0.0, 0.1));
    assert!(img.rgba.chunks(4).all(|p| is_paper([p[0], p[1], p[2], p[3]])));
    // 8 px: present but lighter than at full size.
    let faint = render(&mut r, bg(BackgroundKind::Lined, 32.0), &vp(0.0, 0.0, 0.25));
    let full = render(&mut r, bg(BackgroundKind::Lined, 32.0), &vp(0.0, 0.0, 1.0));
    let dark = |p: [u8; 4]| u32::from(PAPER[0].abs_diff(p[0])) + u32::from(PAPER[2].abs_diff(p[2]));
    assert!(dark(px(&faint, 50, 0)) > 0 && dark(px(&faint, 50, 0)) < dark(px(&full, 50, 0)));
}

#[test]
fn ink_draws_over_the_background_and_changes_redraw_the_cached_frame() {
    let mut r = Headless::new().unwrap();
    let mut c = Canvas::default();
    let samples = (0..20)
        .map(|i| Sample {
            x: 10.0 + f64::from(i) * 10.0,
            y: 64.0,
            pressure: 0.6,
            tilt_x: 0.0,
            tilt_y: 0.0,
            t_us: u64::from(i as u32) * 1000,
        })
        .collect();
    c.objects.insert(
        ObjectId(1),
        CanvasObject {
            id: ObjectId(1),
            z: 0,
            kind: ObjectKind::Stroke(Stroke {
                tool: Tool::Pen,
                color: Color(0x000000ff),
                width: 3.0,
                samples,
            }),
        },
    );
    let v = vp(0.0, 0.0, 1.0);
    r.set_background(bg(BackgroundKind::Lined, 32.0));
    let lined = r.render_settled(&c, &[], &v, 100).unwrap();
    let ink = px(&lined, 100, 64);
    assert!(
        ink[0] < 40 && ink[1] < 40 && ink[2] < 40,
        "ink on top of the line: {ink:?}"
    );
    assert!(!is_paper(px(&lined, 100, 96)));
    // Same canvas and viewport, new background: the next frame shows it.
    r.set_background(bg(BackgroundKind::Blank, 32.0));
    let blank = r.render(&c, &[], &v).unwrap();
    assert!(is_paper(px(&blank, 100, 96)));
}
