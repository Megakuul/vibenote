#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! The app chrome (M11, ADR 0002) without a window: layout, hit areas, keyboard flows, and a
//! headless screenshot (written to the target dir for review).

use serde_json::json;
use vibenote_engine::Engine;
use vibenote_ui::chrome::{Action, Chrome, SIDE_W, TOP_H, ViewMap, screenshot};

fn workspace() -> Engine {
    let mut e = Engine::new().unwrap();
    let f = e
        .execute(
            "workspace.folder.create",
            json!({"name": "Uni", "color": 0x5a8c_d2ff_u32}),
        )
        .unwrap()["id"]
        .clone();
    e.execute("workspace.note.create", json!({"name": "Lineare Algebra", "folder": f}))
        .unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "Ideas"})).unwrap()["id"].clone();
    e.execute("note.open", json!({ "id": n })).unwrap();
    e.execute(
        "text.create",
        json!({"at": [400.0, 200.0], "width": 300.0, "content": "Eigenwerte und Matrizen"}),
    )
    .unwrap();
    e
}

fn vm(e: &Engine) -> ViewMap {
    let v = e.session.view;
    ViewMap {
        origin_x: v.origin_x,
        origin_y: v.origin_y,
        scale: v.zoom,
    }
}

#[test]
fn panels_cover_only_their_areas_and_show_over_the_canvas() {
    let e = workspace();
    let mut c = Chrome::new();
    let (img, actions) = screenshot(&e, &mut c, (1000, 640)).unwrap();
    assert!(actions.is_empty());
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("chrome.png");
    std::fs::write(&out, img.encode_png().unwrap()).unwrap();
    eprintln!("screenshot: {}", out.display());
    assert!(c.covers(10.0, 10.0), "toolbar");
    assert!(c.covers(f64::from(SIDE_W) - 5.0, 300.0), "sidebar");
    assert!(!c.covers(f64::from(SIDE_W) + 50.0, f64::from(TOP_H) + 50.0), "canvas");
    let px = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
    };
    let paper = px(990, 630);
    assert_ne!(px(5, 5), paper, "the toolbar is drawn over the canvas");
    assert_ne!(px(20, 300), paper, "the sidebar is drawn over the canvas");
}

#[test]
fn search_and_find_follow_the_keyboard() {
    let mut e = workspace();
    let mut c = Chrome::new();
    let size = (1000.0, 640.0);
    c.run(&e, size, 1.0, vm(&e));
    c.open_search();
    assert!(c.run(&e, size, 1.0, vm(&e)).is_empty());
    assert!(c.wants_keyboard(), "the search field has focus");
    c.event(egui::Event::Text("eigen".into()));
    let a = c.run(&e, size, 1.0, vm(&e));
    assert_eq!(a, vec![Action::Search("eigen".into())]);
    let r = e
        .execute("search.query", json!({"text": "eigen", "wait": false}))
        .unwrap();
    c.set_results(&r);
    assert_eq!(c.result_count(), 1);
    assert!(c.run(&e, size, 1.0, vm(&e)).is_empty(), "no repeated search");
    // Escape closes the panels; the find bar then asks for in-note matches.
    assert!(c.close_panels());
    c.open_find();
    c.run(&e, size, 1.0, vm(&e));
    c.event(egui::Event::Text("matr".into()));
    let a = c.run(&e, size, 1.0, vm(&e));
    assert_eq!(a, vec![Action::Find("matr".into())]);
    let r = e.execute("search.in_note", json!({"text": "matr"})).unwrap();
    c.set_regions(&r);
    // Highlights follow the view: panning asks for a new layout.
    assert!(c.needs_run(vm(&e)));
    // egui settles within a few runs (window fade-in), then waits for input.
    for _ in 0..100 {
        if !c.needs_run(vm(&e)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        c.run(&e, size, 1.0, vm(&e));
    }
    assert!(!c.needs_run(vm(&e)));
    e.execute("view.pan", json!({"dx": 40.0, "dy": 0.0})).unwrap();
    assert!(c.needs_run(vm(&e)));
}

/// Clicks (press, frame, release, frame) at `p` with `jitter` px of movement in between, as a
/// pen tip or a slightly moving mouse does.
fn click(c: &mut Chrome, e: &Engine, p: egui::Pos2, jitter: f32) -> Vec<Action> {
    let size = (1000.0, 640.0);
    let mut out = Vec::new();
    c.event(egui::Event::PointerMoved(p));
    out.extend(c.run(e, size, 1.0, vm(e)));
    c.event(egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    out.extend(c.run(e, size, 1.0, vm(e)));
    let q = p + egui::vec2(jitter, jitter / 2.0);
    c.event(egui::Event::PointerMoved(q));
    out.extend(c.run(e, size, 1.0, vm(e)));
    c.event(egui::Event::PointerButton {
        pos: q,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    out.extend(c.run(e, size, 1.0, vm(e)));
    out.extend(c.run(e, size, 1.0, vm(e)));
    out
}

fn row(c: &Chrome, name: &str) -> Option<egui::Rect> {
    c.tree_rows().into_iter().find(|(n, _)| n == name).map(|(_, r)| r)
}

#[test]
fn clicking_a_folder_expands_it_even_with_a_little_movement() {
    let e = workspace();
    let mut c = Chrome::new();
    c.run(&e, (1000.0, 640.0), 1.0, vm(&e));
    let uni = row(&c, "Uni").expect("folder row");
    assert!(row(&c, "Lineare Algebra").is_none(), "collapsed at start");
    click(&mut c, &e, uni.center(), 0.0);
    assert!(row(&c, "Lineare Algebra").is_some(), "a click expands the folder");
    // Past the double-click time, so this is a second single click.
    std::thread::sleep(std::time::Duration::from_millis(400));
    click(&mut c, &e, uni.center(), 3.0);
    assert!(
        row(&c, "Lineare Algebra").is_none(),
        "a click with 3 px of jitter collapses it"
    );
}

#[test]
fn double_click_renames_and_dragging_a_note_onto_a_folder_moves_it() {
    let e = workspace();
    let mut c = Chrome::new();
    let size = (1000.0, 640.0);
    c.run(&e, size, 1.0, vm(&e));
    let ideas = row(&c, "Ideas").unwrap();
    let mut a = click(&mut c, &e, ideas.center(), 0.0);
    a.extend(click(&mut c, &e, ideas.center(), 0.0));
    assert!(matches!(a.first(), Some(Action::Show { .. })), "{a:?}");
    c.run(&e, size, 1.0, vm(&e));
    assert!(c.wants_keyboard(), "the rename field has focus");
    // The name is selected when the field opens: typing replaces it.
    c.event(egui::Event::Text("Plans".into()));
    c.event(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    let mut a = c.run(&e, size, 1.0, vm(&e));
    a.extend(c.run(&e, size, 1.0, vm(&e)));
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::Cmd("workspace.note.rename", v) if v["name"] == "Plans")),
        "{a:?}"
    );

    // Drag "Ideas" onto "Uni".
    let uni = row(&c, "Uni").unwrap();
    let ideas = row(&c, "Ideas").unwrap();
    let press = |c: &mut Chrome, p: egui::Pos2, down: bool| {
        c.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: egui::Modifiers::NONE,
        });
    };
    c.event(egui::Event::PointerMoved(ideas.center()));
    c.run(&e, size, 1.0, vm(&e));
    press(&mut c, ideas.center(), true);
    c.run(&e, size, 1.0, vm(&e));
    let mut a = Vec::new();
    for i in 1..=10u8 {
        let t = f32::from(i) / 10.0;
        c.event(egui::Event::PointerMoved(ideas.center().lerp(uni.center(), t)));
        a.extend(c.run(&e, size, 1.0, vm(&e)));
    }
    press(&mut c, uni.center(), false);
    a.extend(c.run(&e, size, 1.0, vm(&e)));
    assert!(
        a.iter().any(|x| matches!(x, Action::Cmd("workspace.note.move", _))),
        "{a:?}"
    );
    assert!(
        !a.iter().any(|x| matches!(x, Action::Show { .. })),
        "a drag is no click: {a:?}"
    );
}
