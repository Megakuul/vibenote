#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! R-TEXT-001..003 (SPEC §5).

use serde_json::json;
use vibenote_acceptance::*;

/// R-TEXT-001: basic rich text formatting survives save/load.
#[test]
fn r_text_001_rich_text() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "t"})));
    run(&mut e, "note.open", json!({"id": n}));
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "Überschrift Liste"}),
    ));
    for (fmt, range) in [
        ("bold", [0, 11]),
        ("italic", [0, 4]),
        ("underline", [5, 11]),
        ("strike", [12, 17]),
        ("heading1", [0, 11]),
        ("bullet_list", [12, 17]),
        ("checkbox", [12, 17]),
    ] {
        run(&mut e, "text.format", json!({"id": t, "format": fmt, "range": range}));
    }
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = vibenote_engine::Engine::open(&db).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
}

/// R-TEXT-002: German characters typed via IME/xkb arrive unchanged.
#[test]
fn r_text_002_umlauts() {
    let (mut e, _) = engine_with_note();
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": ""}),
    ));
    run(&mut e, "text.edit", json!({"id": t, "insert": "ÄÖÜäöüß", "at": 0}));
    assert_eq!(run(&mut e, "text.get", json!({"id": t}))["content"], "ÄÖÜäöüß");
}

/// R-TEXT-003: typed text is searchable.
#[test]
fn r_text_003_searchable() {
    let (mut e, _) = engine_with_note();
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "Eigenwert Matrix"}),
    );
    let r = run(&mut e, "search.query", json!({"text": "eigenwert"}));
    assert!(!r["results"].as_array().unwrap().is_empty());
}

fn frame(e: &mut vibenote_engine::Engine, t: u64) -> [f64; 4] {
    serde_json::from_value(run(e, "text.get", json!({"id": t}))["frame"].clone()).unwrap()
}

/// R-TEXT-004: the frame fits the laid-out text and follows typing and font size.
#[test]
fn r_text_004_auto_size() {
    let (mut e, _) = engine_with_note();
    run(&mut e, "tool.select.text", json!({}));
    for kind in ["down", "up"] {
        run(
            &mut e,
            "input.pen",
            json!({"events": [{"t_us": 1000, "kind": kind, "tool": "pen", "x": 100.0, "y": 100.0, "p": 0.5}]}),
        );
    }
    run(&mut e, "text.input", json!({"text": "ab"}));
    let t = e.session.text_edit.as_ref().unwrap().id.unwrap();
    let f1 = frame(&mut e, t);
    run(&mut e, "text.input", json!({"text": "cdefgh"}));
    let f2 = frame(&mut e, t);
    assert!(
        f2[2] - f2[0] > (f1[2] - f1[0]) * 2.0,
        "the frame grows with the line: {f1:?} → {f2:?}"
    );
    assert!((f2[3] - f2[1] - (f1[3] - f1[1])).abs() < 1.0, "one line, same height");
    // Past the wrap width (600 px at zoom 1) it wraps: two lines, width at most the wrap width.
    run(
        &mut e,
        "text.input",
        json!({"text": " lorem ipsum dolor sit amet".repeat(6)}),
    );
    let f3 = frame(&mut e, t);
    assert!(f3[2] - f3[0] <= 600.5, "{f3:?}");
    assert!(f3[3] - f3[1] > (f2[3] - f2[1]) * 1.8, "wrapped onto more lines: {f3:?}");
    // A larger font grows the frame.
    run(&mut e, "text.input", json!({"key": "select_all"}));
    run(&mut e, "text.style", json!({"format": "size", "value": 40.0}));
    let f4 = frame(&mut e, t);
    assert!(f4[3] - f4[1] > (f3[3] - f3[1]) * 1.8, "{f3:?} → {f4:?}");
    // The selection box is the frame.
    run(&mut e, "text.end", json!({}));
    let o = e.session.ws.canvas(e.session.current_note.unwrap()).unwrap().objects[&vibenote_model::ObjectId(t)].clone();
    let b = e.session.object_box(&o).unwrap();
    assert!((b.max.y - f4[3]).abs() < 1e-6 && (b.max.x - f4[2]).abs() < 1e-6);
}

/// R-TEXT-005 (revised by the owner 2026-10-15: "literally the normal field behaviour"): a click
/// types at the click, a drag selects, double/triple clicks select the word/line, Shift+click
/// extends, the frame band moves the box; in vim mode a click is Normal mode, a drag Visual.
#[test]
fn r_text_005_clicks() {
    let (mut e, n) = engine_with_note();
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [100.0, 100.0], "width": 300.0, "content": "hello big world\nsecond line"}),
    ));
    run(&mut e, "tool.select.text", json!({}));
    let ev = |e: &mut vibenote_engine::Engine, t_us: u64, kind: &str, x: f64, y: f64| {
        run(
            e,
            "input.pen",
            json!({"events": [{"t_us": t_us, "kind": kind, "tool": "pen", "x": x, "y": y, "p": 0.5}]}),
        );
    };
    let tap = |e: &mut vibenote_engine::Engine, t_us: u64, x: f64| {
        ev(e, t_us, "down", x, 108.0);
        ev(e, t_us, "up", x, 108.0);
    };
    // One click types into the box with the caret where it was clicked.
    tap(&mut e, 1_000_000, 150.0);
    let ed = e.session.text_edit.clone().unwrap();
    assert_eq!(
        (ed.id, ed.place),
        (Some(t), Some((150.0, 108.0))),
        "the caret goes to the click"
    );
    assert!(e.session.selection.ids.is_empty(), "no object selection");
    run(&mut e, "text.caret", json!({"offset": 7}));
    assert_eq!(e.session.text_edit.as_ref().unwrap().range(), (7, 7));
    // Double click: the word; triple click: the line.
    tap(&mut e, 3_000_000, 150.0);
    tap(&mut e, 3_200_000, 150.0);
    assert!(
        e.session.text_edit.as_ref().unwrap().word,
        "a double click asks for the word"
    );
    run(&mut e, "text.caret", json!({"offset": 7, "word": true}));
    assert_eq!(e.session.text_edit.as_ref().unwrap().range(), (6, 9));
    tap(&mut e, 3_350_000, 150.0);
    assert!(
        e.session.text_edit.as_ref().unwrap().line,
        "a triple click asks for the line"
    );
    run(&mut e, "text.caret", json!({"offset": 7, "line": true}));
    assert_eq!(e.session.text_edit.as_ref().unwrap().range(), (0, 15));
    // A drag selects from the press to the pointer.
    ev(&mut e, 5_000_000, "down", 120.0, 108.0);
    ev(&mut e, 5_050_000, "motion", 180.0, 108.0);
    let ed = e.session.text_edit.clone().unwrap();
    assert_eq!((ed.place_from, ed.place), (Some((120.0, 108.0)), Some((180.0, 108.0))));
    ev(&mut e, 5_100_000, "up", 180.0, 108.0);
    run(&mut e, "text.caret", json!({"offset": 9, "anchor": 2}));
    assert_eq!(e.session.text_edit.as_ref().unwrap().range(), (2, 9));
    // Shift+click extends the selection.
    run(
        &mut e,
        "input.pointer",
        json!({"events": [{"t_us": 7_000_000, "kind": "down", "x": 200.0, "y": 108.0, "button": 272, "shift": true},
                          {"t_us": 7_000_000, "kind": "up", "x": 200.0, "y": 108.0, "button": 272}]}),
    );
    assert!(e.session.text_edit.as_ref().unwrap().extend);
    run(&mut e, "text.caret", json!({"offset": 12, "extend": true}));
    assert_eq!(e.session.text_edit.as_ref().unwrap().range(), (2, 12));
    // The frame band just outside the text moves the box.
    let b = {
        let o = e.session.ws.canvas(vibenote_model::NoteId(n)).unwrap().objects[&vibenote_model::ObjectId(t)].clone();
        e.session.object_box(&o).unwrap()
    };
    let (fx, fy) = (b.min.x - 4.0, (b.min.y + b.max.y) / 2.0);
    ev(&mut e, 9_000_000, "down", fx, fy);
    ev(&mut e, 9_050_000, "motion", fx + 40.0, fy + 10.0);
    ev(&mut e, 9_100_000, "up", fx + 40.0, fy + 10.0);
    let o = run(&mut e, "text.get", json!({"id": t}));
    assert_eq!(
        (o["origin"]["x"].as_f64(), o["origin"]["y"].as_f64()),
        (Some(140.0), Some(110.0)),
        "moved"
    );
    // Vim: a click is the Normal-mode cursor on the clicked character; a drag is Visual mode.
    run(&mut e, "settings.set", json!({"vim": true}));
    run(&mut e, "text.end", json!({}));
    tap(&mut e, 11_000_000, 200.0);
    run(&mut e, "text.caret", json!({"offset": 4}));
    let v = serde_json::to_value(e.session.text_edit.as_ref().unwrap()).unwrap();
    assert_eq!(
        (v["vim"]["mode"].as_str(), v["caret"].as_u64()),
        (Some("normal"), Some(4))
    );
    run(&mut e, "text.caret", json!({"offset": 9, "anchor": 6}));
    let v = serde_json::to_value(e.session.text_edit.as_ref().unwrap()).unwrap();
    assert_eq!(v["vim"]["mode"].as_str(), Some("visual"));
    assert_eq!(e.session.vim_selection(), Some((6, 9)), "the dragged characters");
}

/// R-TEXT-006: a colour from the palette colours the selection (stored, rendered by the
/// renderer's spans).
#[test]
fn r_text_006_text_colour() {
    let (mut e, _) = engine_with_note();
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "red text"}),
    ));
    run(&mut e, "text.open", json!({"id": t, "offset": 0}));
    run(&mut e, "text.caret", json!({"offset": 3, "anchor": 0}));
    run(
        &mut e,
        "text.style",
        json!({"format": "color", "value": 0xc628_28ffu32}),
    );
    let marks = run(&mut e, "text.get", json!({"id": t}))["marks"].clone();
    assert_eq!(
        marks,
        json!([{"start": 0, "end": 3, "format": "color", "color": 0xc628_28ffu32}]),
        "{marks}"
    );
}

/// R-TEXT-007: fenced code blocks are monospace with syntax highlighting (display only).
#[test]
fn r_text_007_code_blocks() {
    let text = "Notes\n```python\ndef f(x):\n    return 'hi'  # greet\n```\nafter";
    let toks = vibenote_render::code::line_tokens(text);
    assert!(toks[0].is_none() && toks[5].is_none(), "outside the block");
    assert_eq!(toks[1].as_ref().unwrap()[0].2, vibenote_render::code::Token::Fence);
    let def = &toks[2].as_ref().unwrap()[0];
    assert_eq!((def.0, def.1, def.2), (0, 3, vibenote_render::code::Token::Keyword));
    assert!(
        toks[3]
            .as_ref()
            .unwrap()
            .iter()
            .any(|t| t.2 == vibenote_render::code::Token::Comment)
    );
    // The block is laid out in the monospace font: equal widths for equal character counts
    // (the frame of a box fits its laid-out text, R-TEXT-004).
    let (mut e, _) = engine_with_note();
    let width = |e: &mut vibenote_engine::Engine, content: &str| {
        let t = id(&run(
            e,
            "text.create",
            json!({"at": [0.0, 0.0], "width": 2000.0, "content": content}),
        ));
        let f = run(e, "text.get", json!({"id": t}))["frame"].clone();
        f[2].as_f64().unwrap() - f[0].as_f64().unwrap()
    };
    let (w_i, w_m) = (
        width(&mut e, "```\niiiiiiiiiiii\n```"),
        width(&mut e, "```\nmmmmmmmmmmmm\n```"),
    );
    assert!((w_i - w_m).abs() < 0.5, "monospace: {w_i} vs {w_m}");
    let (p_i, p_m) = (width(&mut e, "iiiiiiiiiiii"), width(&mut e, "mmmmmmmmmmmm"));
    assert!(p_m > p_i * 1.5, "plain text is proportional: {p_i} vs {p_m}");
    // Tab indents by 4 spaces, Shift+Tab dedents (any text box).
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 300.0, "content": "x"}),
    ));
    run(&mut e, "text.open", json!({"id": t, "offset": 0}));
    run(&mut e, "text.input", json!({"key": "tab"}));
    assert_eq!(run(&mut e, "text.get", json!({"id": t}))["content"], "    x");
    run(&mut e, "text.input", json!({"key": "tab", "shift": true}));
    assert_eq!(run(&mut e, "text.get", json!({"id": t}))["content"], "x");
}
