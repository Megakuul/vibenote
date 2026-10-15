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
//! R-CMD-001..002 (SPEC §14).

use serde_json::json;
use vibenote_acceptance::*;

/// R-CMD-001: every action is a registered command with id, params schema and requirements; the
/// control channel dispatches by id.
#[test]
fn r_cmd_001_registry_and_control_channel() {
    let mut e = vibenote_engine::Engine::new().unwrap();
    let d = e.registry.describe();
    for c in d.as_array().unwrap() {
        assert!(c["id"].is_string() && c["reqs"].as_array().is_some_and(|r| !r.is_empty()));
        if c["implemented"] == true {
            assert!(c["params"].is_object(), "{} needs a params schema", c["id"]);
        }
    }
    let line = r#"{"id":7,"method":"engine.execute","params":{"cmd":"workspace.note.create","args":{"name":"x"}}}"#;
    let resp: serde_json::Value = serde_json::from_str(&vibenote_control::handle_line(
        &mut e,
        &mut vibenote_control::NoExt,
        line,
    ))
    .unwrap();
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["id"], 7);
}

/// R-CMD-002: the default keymap maps every shortcut to an existing command id.
#[test]
fn r_cmd_002_keymap() {
    let mut e = vibenote_engine::Engine::new().unwrap();
    let km = run(&mut e, "keymap.get", json!({}));
    let ids: Vec<String> = e.registry.ids().map(String::from).collect();
    let bindings = km["bindings"].as_object().unwrap();
    for key in [
        "P",
        "H",
        "E",
        "L",
        "S",
        "R",
        "T",
        "Ctrl+Z",
        "Ctrl+Shift+Z",
        "Ctrl+C",
        "Ctrl+X",
        "Ctrl+V",
        "Delete",
        "Ctrl+P",
        "Ctrl+F",
        "Ctrl+Shift+F",
        "Ctrl+0",
        "Ctrl+1",
    ] {
        let cmd = bindings[key].as_str().unwrap_or_else(|| panic!("no binding for {key}"));
        assert!(ids.iter().any(|i| i == cmd), "{key} → unknown command {cmd}");
    }
}
