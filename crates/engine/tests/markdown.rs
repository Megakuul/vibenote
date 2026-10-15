#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! Markdown in text boxes (R-TEXT-001): typed syntax formats as you go; markdown content
//! becomes formatted text.

use serde_json::{Value, json};
use vibenote_engine::Engine;
use vibenote_model::{TextMark, TextStyle};

fn typing() -> Engine {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "m"})).unwrap()["id"].clone();
    e.execute("note.open", json!({ "id": n })).unwrap();
    e.execute("tool.select.text", json!({})).unwrap();
    for kind in ["down", "up"] {
        e.execute(
            "input.pen",
            json!({"events": [{"t_us": 1000, "kind": kind, "tool": "pen", "x": 100.0, "y": 100.0, "p": 0.5}]}),
        )
        .unwrap();
    }
    e
}

/// Types `s` one character at a time (as the keyboard does); `\n` is Enter.
fn type_keys(e: &mut Engine, s: &str) {
    for c in s.chars() {
        let p = if c == '\n' {
            json!({"key": "enter"})
        } else {
            json!({"text": c.to_string()})
        };
        e.execute("text.input", p).unwrap();
    }
}

fn get(e: &mut Engine) -> (String, Vec<TextMark>) {
    let id = e.session.text_edit.as_ref().unwrap().id.unwrap();
    let v: Value = e.execute("text.get", json!({ "id": id })).unwrap();
    (
        v["content"].as_str().unwrap().to_string(),
        serde_json::from_value(v["marks"].clone()).unwrap(),
    )
}

fn m(start: u32, end: u32, style: TextStyle) -> TextMark {
    TextMark { start, end, style }
}

#[test]
fn headings_and_inline_formats_while_typing() {
    let mut e = typing();
    type_keys(&mut e, "# Title\nsome **bold** and *it* ~~x~~ 2*3*4");
    let (t, marks) = get(&mut e);
    assert_eq!(t, "Title\nsome bold and it x 2*3*4");
    assert_eq!(
        marks,
        vec![
            m(0, 6, TextStyle::Heading1),
            m(11, 15, TextStyle::Bold),
            m(20, 22, TextStyle::Italic),
            m(23, 24, TextStyle::Strike),
        ],
        "the heading does not go on after Enter; text after a span is plain"
    );
}

#[test]
fn lists_continue_on_enter_and_end_on_an_empty_item() {
    let mut e = typing();
    type_keys(&mut e, "- one\ntwo\n\nafter\n[ ] task\nnext");
    let (t, marks) = get(&mut e);
    assert_eq!(t, "one\ntwo\nafter\ntask\nnext");
    assert_eq!(
        marks,
        vec![
            m(0, 8, TextStyle::BulletList),
            m(14, 23, TextStyle::Checkbox { checked: false }),
        ]
    );
    // A prefix typed before existing text formats that line.
    let mut e = typing();
    type_keys(&mut e, "Plan");
    e.execute("text.input", json!({"key": "home"})).unwrap();
    type_keys(&mut e, "## ");
    let (t, marks) = get(&mut e);
    assert_eq!(t, "Plan");
    assert_eq!(marks, vec![m(0, 4, TextStyle::Heading2)]);
}

#[test]
fn markdown_content_is_formatted_on_create() {
    let mut e = typing();
    let id = e
        .execute(
            "text.create",
            json!({"at": [0.0, 0.0], "width": 300.0, "markdown": true, "content": "# Notes\n- **a**\n- b"}),
        )
        .unwrap()["id"]
        .clone();
    let v = e.execute("text.get", json!({ "id": id })).unwrap();
    assert_eq!(v["content"], "Notes\na\nb");
    let marks: Vec<TextMark> = serde_json::from_value(v["marks"].clone()).unwrap();
    assert!(marks.contains(&m(0, 5, TextStyle::Heading1)), "{marks:?}");
    assert!(marks.contains(&m(6, 7, TextStyle::Bold)), "{marks:?}");
    assert!(
        marks.iter().any(|x| x.style == TextStyle::BulletList && x.start == 6),
        "{marks:?}"
    );
}
