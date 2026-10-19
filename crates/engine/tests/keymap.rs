#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! Keymap (M5.3, R-CMD-002): defaults, rebinding, persistence; the commands keys need.

use serde_json::json;
use vibenote_engine::Engine;

#[test]
fn every_default_binding_names_a_registered_command() {
    let mut e = Engine::new().unwrap();
    let km = e.execute("keymap.get", json!({})).unwrap();
    let ids: Vec<&str> = e.registry.ids().collect();
    let b = km["bindings"].as_object().unwrap();
    assert!(b.len() >= 20);
    for (k, c) in b {
        assert!(ids.contains(&c.as_str().unwrap()), "{k} → {c}");
    }
}

#[test]
fn rebinding_validates_persists_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    e.execute(
        "keymap.set",
        json!({"key": "shift+ctrl+h", "command": "tool.select.highlighter"}),
    )
    .unwrap();
    e.execute("keymap.set", json!({"key": "W", "command": null})).unwrap();
    assert!(
        e.execute("keymap.set", json!({"key": "Ctrl+Q", "command": "no.such.command"}))
            .is_err()
    );
    assert!(
        e.execute("keymap.set", json!({"key": "Hyper+Q", "command": "edit.undo"}))
            .is_err()
    );
    assert_eq!(
        e.key_binding("Ctrl+Shift+H").as_deref(),
        Some("tool.select.highlighter")
    );
    assert_eq!(e.key_binding("W"), None);
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(
        e.key_binding("Ctrl+Shift+H").as_deref(),
        Some("tool.select.highlighter")
    );
    assert_eq!(e.key_binding("W"), None);
    e.execute("keymap.reset", json!({"key": "w"})).unwrap();
    assert_eq!(e.key_binding("W").as_deref(), Some("tool.select.pen"));
    e.execute("keymap.reset", json!({})).unwrap();
    assert_eq!(e.key_binding("Ctrl+Shift+H"), None);
    // Setting a key back to its default leaves no override.
    e.execute("keymap.set", json!({"key": "E", "command": "tool.select.eraser"}))
        .unwrap();
    assert_eq!(e.inspect(Some("/keymap_overrides")).unwrap(), json!({}));
    // Hostile stored keymaps: invalid entries are dropped, valid ones kept.
    e.execute("keymap.set", json!({"key": "F5", "command": "edit.undo"}))
        .unwrap();
    e.sync().unwrap();
    drop(e);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'keymap'",
        [r#"{"F5":"edit.undo","Hyper+X":"edit.redo","F6":"evil.cmd"}"#],
    )
    .unwrap();
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.key_binding("F5").as_deref(), Some("edit.undo"));
    assert_eq!(e.key_binding("F6"), None);
    c.execute("UPDATE meta SET value = 'garbage' WHERE key = 'keymap'", [])
        .unwrap();
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.key_binding("W").as_deref(), Some("tool.select.pen"));
}

#[test]
fn eraser_key_toggles_stroke_and_area_and_zoom_keys_step() {
    let mut e = Engine::new().unwrap();
    let tool = |e: &Engine| e.inspect(Some("/tool")).unwrap();
    e.execute("tool.select.eraser", json!({})).unwrap();
    assert_eq!(tool(&e), json!("eraser_stroke"));
    e.execute("tool.select.eraser", json!({})).unwrap();
    assert_eq!(tool(&e), json!("eraser_area"));
    assert_eq!(e.inspect(Some("/settings/eraser_mode")).unwrap(), json!("area"));
    e.execute("tool.select.pen", json!({})).unwrap();
    e.execute("tool.select.eraser", json!({})).unwrap();
    assert_eq!(tool(&e), json!("eraser_area"), "the last eraser mode is kept");
    let zoom = |e: &Engine| e.inspect(Some("/view/zoom")).unwrap().as_f64().unwrap();
    e.execute("view.zoom_in", json!({"anchor": [100.0, 100.0]})).unwrap();
    assert!((zoom(&e) - 1.25).abs() < 1e-12);
    e.execute("view.zoom_out", json!({})).unwrap();
    e.execute("view.zoom_out", json!({})).unwrap();
    assert!((zoom(&e) - 0.8).abs() < 1e-12);
    e.execute("view.zoom_reset", json!({"anchor": [50.0, 50.0]})).unwrap();
    assert!((zoom(&e) - 1.0).abs() < 1e-12);
    // UI panels and quit are state the UI reads.
    e.execute("ui.quickopen.show", json!({})).unwrap();
    assert_eq!(e.inspect(Some("/ui/panel")).unwrap(), json!("quickopen"));
    e.execute("ui.close", json!({})).unwrap();
    assert_eq!(e.inspect(Some("/ui/panel")).unwrap(), json!(null));
    e.execute("app.quit", json!({})).unwrap();
    assert_eq!(e.inspect(Some("/ui/quit")).unwrap(), json!(true));
}
