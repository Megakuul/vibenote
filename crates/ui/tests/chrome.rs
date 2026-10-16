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
    click_in(c, e, p, jitter, (1000.0, 640.0))
}

fn click_in(c: &mut Chrome, e: &Engine, p: egui::Pos2, jitter: f32, size: (f32, f32)) -> Vec<Action> {
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

#[test]
fn a_context_menu_stays_open_while_the_pointer_moves_onto_it() {
    let e = workspace();
    let mut c = Chrome::new();
    let size = (1000.0, 640.0);
    c.run(&e, size, 1.0, vm(&e));
    let uni = row(&c, "Uni").unwrap();
    let p = uni.center();
    c.event(egui::Event::PointerMoved(p));
    c.run(&e, size, 1.0, vm(&e));
    for pressed in [true, false] {
        c.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        c.run(&e, size, 1.0, vm(&e));
    }
    c.run(&e, size, 1.0, vm(&e));
    let open = |c: &Chrome| c.covers(f64::from(p.x) + 30.0, f64::from(p.y) + 40.0);
    assert!(open(&c), "the menu is open below the click");
    for k in 1..=20u8 {
        c.event(egui::Event::PointerMoved(
            p + egui::vec2(f32::from(k) * 1.5, f32::from(k) * 2.0),
        ));
        c.run(&e, size, 1.0, vm(&e));
        assert!(open(&c), "still open after moving {k} steps toward its items");
    }
    // Passing "Colour" opened its submenu, which reaches past the sidebar's right edge: that
    // part is chrome too (it went to the canvas before, and the menu closed).
    assert!(!c.covers(f64::from(SIDE_W) + 20.0, 600.0), "the canvas is still canvas");
    assert!(
        c.covers(f64::from(SIDE_W) + 20.0, f64::from(p.y) + 60.0),
        "the submenu past the sidebar"
    );
}

/// R-VIM-005: the finder takes the keyboard; typing asks for results, Ctrl+N / Ctrl+P move,
/// Enter opens the selected one, Esc closes. No pointer involved.
#[test]
fn the_finder_works_from_the_keyboard_alone() {
    let mut e = workspace();
    let mut c = Chrome::new();
    let size = (1000.0, 640.0);
    let key = |c: &mut Chrome, key: egui::Key, modifiers: egui::Modifiers| {
        c.event(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
    };
    c.run(&e, size, 1.0, vm(&e));
    c.open_finder("notes");
    c.run(&e, size, 1.0, vm(&e));
    c.run(&e, size, 1.0, vm(&e));
    assert!(c.wants_keyboard(), "the prompt has the keyboard");
    c.event(egui::Event::Text("a".into()));
    let a = c.run(&e, size, 1.0, vm(&e));
    assert_eq!(
        a,
        vec![Action::FinderQuery {
            kind: "notes",
            query: "a".into()
        }]
    );
    let r = e
        .execute("finder.query", json!({"kind": "notes", "query": "a"}))
        .unwrap();
    assert!(r["results"].as_array().unwrap().len() >= 2, "{r}");
    c.set_finder_results("a", &r);
    c.run(&e, size, 1.0, vm(&e));
    key(&mut c, egui::Key::N, egui::Modifiers::CTRL);
    c.run(&e, size, 1.0, vm(&e));
    key(&mut c, egui::Key::Enter, egui::Modifiers::NONE);
    let a = c.run(&e, size, 1.0, vm(&e));
    assert_eq!(
        a,
        vec![Action::FinderOpen(r["results"][1].clone())],
        "Ctrl+N then Enter opens the second"
    );
    assert!(!c.finder_open());
    // Esc closes without opening anything.
    c.open_finder("text");
    c.run(&e, size, 1.0, vm(&e));
    key(&mut c, egui::Key::Escape, egui::Modifiers::NONE);
    let a = c.run(&e, size, 1.0, vm(&e));
    assert!(!a.iter().any(|x| matches!(x, Action::FinderOpen(_))));
    assert!(!c.finder_open());
}

/// Runs the chrome until egui asks for no immediate frame (as the app does), at most 8 times.
fn settle(c: &mut Chrome, e: &Engine, size: (f32, f32)) {
    for _ in 0..8 {
        if !c.wants_frame() {
            break;
        }
        c.run(e, size, 1.0, vm(e));
    }
}

/// R-PEN-013 / R-TEXT-006: the pen's palette sets its colour; while typing, the text palette
/// colours the selection.
#[test]
fn colour_palettes_set_pen_and_text_colours() {
    let mut e = workspace();
    let mut c = Chrome::new();
    let size = (1400.0, 640.0);
    c.run(&e, size, 1.0, vm(&e));
    let red = 0xc628_28ffu32;
    let r = c
        .swatch_rects()
        .iter()
        .find(|(k, _)| *k == red)
        .map(|(_, r)| *r)
        .expect("pen swatches shown");
    let a = click_in(&mut c, &e, r.center(), 0.0, size);
    assert!(
        a.contains(&Action::Cmd("settings.set", json!({ "pen_color": red }))),
        "{a:?}"
    );
    e.execute("settings.set", json!({ "pen_color": red })).unwrap();
    // Editing text: the text palette.
    let t = e
        .execute(
            "text.create",
            json!({"at": [400.0, 400.0], "width": 300.0, "content": "abc"}),
        )
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("text.open", json!({"id": t, "offset": 0})).unwrap();
    c.invalidate();
    c.run(&e, size, 1.0, vm(&e));
    // The palette button opens the text colours.
    let b = c
        .swatch_rects()
        .iter()
        .find(|(k, _)| *k == 0)
        .map(|(_, r)| *r)
        .expect("palette button");
    click_in(&mut c, &e, b.center(), 0.0, size);
    settle(&mut c, &e, size);
    let blue = 0x1f5f_bfffu32;
    let r = c
        .swatch_rects()
        .iter()
        .find(|(k, _)| *k == blue)
        .map(|(_, r)| *r)
        .expect("text swatches shown");
    let a = click_in(&mut c, &e, r.center(), 0.0, size);
    assert!(
        a.contains(&Action::Cmd("text.style", json!({"format": "color", "value": blue}))),
        "{a:?}"
    );
}

/// R-AUDIO-004: a failed action is shown to the user, at the bottom, for a few seconds.
#[test]
fn errors_are_shown_as_a_notice() {
    let e = workspace();
    let mut c = Chrome::new();
    let size = (1000.0, 640.0);
    c.run(&e, size, 1.0, vm(&e));
    c.notify("Could not start recording: no input device".into());
    // (A new area is sized in its first frame and drawn in the next; the app runs again.)
    c.run(&e, size, 1.0, vm(&e));
    c.run(&e, size, 1.0, vm(&e));
    assert_eq!(c.notice(), Some("Could not start recording: no input device"));
    assert!(c.covers(500.0, 610.0), "the notice is drawn at the bottom");
    settle(&mut c, &e, size);
    assert!(c.wake_in().is_some(), "it goes away by itself");
}
