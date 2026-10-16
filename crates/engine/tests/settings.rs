#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Workspace settings (M2.4): defaults, validation, persistence, not part of undo.

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::Engine;

fn settings(e: &Engine) -> Value {
    e.inspect(Some("/settings")).unwrap()
}

const DEFAULTS: &str = r#"{"tool": "pen", "eraser_mode": "stroke", "prettify_mode": "suggest",
    "block_idle_ms": 1500, "prediction_ms": 8.0, "background": {"kind": "blank", "spacing": 32.0},
    "barrel1": "lasso", "barrel2": "eraser", "shape": "rectangle", "snap_on_lift": false, "vim": false,
    "pen_color": 504829183, "highlighter_color": 4123525375}"#;

#[test]
fn defaults_follow_the_spec() {
    let e = Engine::new().unwrap();
    assert_eq!(settings(&e), serde_json::from_str::<Value>(DEFAULTS).unwrap());
}

#[test]
fn set_changes_only_the_given_fields() {
    let mut e = Engine::new().unwrap();
    let r = e
        .execute(
            "settings.set",
            json!({"eraser_mode": "area", "background": {"kind": "dots"}}),
        )
        .unwrap();
    let mut want: Value = serde_json::from_str(DEFAULTS).unwrap();
    want["eraser_mode"] = json!("area");
    want["background"]["kind"] = json!("dots");
    assert_eq!(r, want, "returns all settings");
    assert_eq!(settings(&e), want);
    e.execute("hwr.set_mode", json!({"mode": "off"})).unwrap();
    e.execute("settings.set_block_idle", json!({"ms": 10_000})).unwrap();
    assert_eq!(e.inspect(Some("/settings/prettify_mode")).unwrap(), json!("off"));
    assert_eq!(e.inspect(Some("/settings/block_idle_ms")).unwrap(), json!(10_000));
    assert_eq!(e.inspect(Some("/settings/eraser_mode")).unwrap(), json!("area"));
}

#[test]
fn invalid_values_change_nothing() {
    let mut e = Engine::new().unwrap();
    let before = settings(&e);
    for (cmd, p) in [
        ("settings.set", json!({"block_idle_ms": 499})),
        ("settings.set", json!({"block_idle_ms": 10_001, "tool": "lasso"})),
        ("settings.set", json!({"prediction_ms": -0.5})),
        ("settings.set", json!({"prediction_ms": 20.5})),
        ("settings.set", json!({"background": {"spacing": 3.9}})),
        ("settings.set", json!({"background": {"spacing": 1e300}})),
        ("settings.set", json!({"tool": "ruler"})),
        ("settings.set", json!({"colour": "red"})),
        ("settings.set", json!({"prediction_ms": "8"})),
        ("hwr.set_mode", json!({"mode": "sometimes"})),
        ("settings.set_block_idle", json!({"ms": -1})),
        ("settings.set_block_idle", json!({"ms": 100})),
    ] {
        assert!(e.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
        assert_eq!(settings(&e), before, "{cmd} {p}");
    }
}

#[test]
fn settings_persist_and_are_not_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "s"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("settings.set", json!({"tool": "highlighter", "prediction_ms": 0.0}))
        .unwrap();
    e.execute("hwr.set_mode", json!({"mode": "automatic"})).unwrap();
    let want = settings(&e);
    // Undo reverts the note creation, not the settings.
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert_eq!(settings(&e), want);
    assert!(e.execute("edit.undo", json!({"scope": "workspace"})).is_err());
    assert!(!e.session.ws.notes.contains_key(&vibenote_model::NoteId(n)));
    drop(e);
    let e = Engine::open(&path).unwrap();
    assert_eq!(settings(&e), want);
    assert_eq!(e.inspect(Some("/warnings")).ok(), None, "no warnings");
}

#[test]
fn changing_settings_never_touches_ink() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "s"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": 0.0, "y": 0.0, "p": 0.5}, {"x": 9.0, "y": 1.0, "p": 0.5}]}),
    )
    .unwrap();
    let (ws, hist) = (e.session.ws.clone(), e.session.history.clone());
    for m in ["off", "automatic", "suggest"] {
        e.execute("hwr.set_mode", json!({"mode": m})).unwrap();
    }
    e.execute(
        "settings.set",
        json!({"tool": "eraser_area", "background": {"kind": "grid", "spacing": 20.0}}),
    )
    .unwrap();
    assert_eq!(e.session.ws, ws);
    assert_eq!(e.session.history, hist);
}

/// Stored settings are hostile input: unreadable or out-of-range values fall back to defaults
/// with a warning, unknown (newer) fields are ignored, missing fields default.
#[test]
fn bad_stored_settings_fall_back_to_defaults() {
    let defaults: Value = serde_json::from_str(DEFAULTS).unwrap();
    for (stored, want_warning, check) in [
        ("not json", true, defaults.clone()),
        (r#"{"tool": "time_machine"}"#, true, defaults.clone()),
        (
            r#"{"block_idle_ms": 5, "prediction_ms": 2.0, "background": {"kind": "lined", "spacing": -4}}"#,
            true,
            {
                let mut v = defaults.clone();
                v["prediction_ms"] = json!(2.0);
                v["background"]["kind"] = json!("lined");
                v
            },
        ),
        // A value from a newer build resets only its own field.
        (
            r#"{"tool": "laser", "eraser_mode": "area", "background": {"kind": "hex", "spacing": 20.0}}"#,
            true,
            {
                let mut v = defaults.clone();
                v["eraser_mode"] = json!("area");
                v["background"]["spacing"] = json!(20.0);
                v
            },
        ),
        (r#"[1, 2]"#, true, defaults.clone()),
        (r#"{"tool": "lasso", "from_the_future": 1}"#, false, {
            let mut v = defaults.clone();
            v["tool"] = json!("lasso");
            v
        }),
        ("{}", false, defaults.clone()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        drop(Engine::open(&path).unwrap());
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("INSERT INTO meta(key, value) VALUES('settings', ?1)", [stored])
            .unwrap();
        drop(c);
        let e = Engine::open(&path).unwrap();
        assert_eq!(settings(&e), check, "{stored}");
        let warned = e.inspect(Some("/warnings")).is_ok();
        assert_eq!(warned, want_warning, "{stored}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// Any sequence of setting changes (valid or not) reads back identically after a reopen.
    #[test]
    fn reopen_keeps_the_last_valid_settings(
        changes in prop::collection::vec(
            (0u8..4, 0u32..12_000, -5.0f64..25.0, prop::sample::select(vec!["pen", "lasso", "text", "eraser_area"])),
            0..20,
        )
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut e = Engine::open(&path).unwrap();
        let mut mem = Engine::new().unwrap();
        for (k, ms, f, t) in changes {
            let p = match k {
                0 => json!({"block_idle_ms": ms}),
                1 => json!({"prediction_ms": f}),
                2 => json!({"tool": t, "background": {"spacing": f * 10.0}}),
                _ => json!({"prettify_mode": if ms % 2 == 0 { "off" } else { "automatic" }}),
            };
            let a = e.execute("settings.set", p.clone()).map_err(|x| x.to_string());
            let b = mem.execute("settings.set", p).map_err(|x| x.to_string());
            prop_assert_eq!(a, b);
        }
        drop(e);
        let back = Engine::open(&path).unwrap();
        prop_assert_eq!(settings(&back), settings(&mem));
    }
}
