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
//! R-LINK-001..003 (SPEC §6).

use serde_json::json;
use vibenote_acceptance::*;

/// R-LINK-001: note, canvas-position and URL links.
#[test]
fn r_link_001_link_types() {
    let (mut e, n) = engine_with_note();
    let other = id(&run(&mut e, "workspace.note.create", json!({"name": "target"})));
    let l1 = id(&run(
        &mut e,
        "link.create",
        json!({"at": [0.0, 0.0], "target": {"note": other}}),
    ));
    run(&mut e, "link.activate", json!({"id": l1}));
    assert_eq!(e.inspect(Some("/current_note")).unwrap(), json!(other));
    run(&mut e, "note.open", json!({"id": n}));
    let l2 = id(&run(
        &mut e,
        "link.create",
        json!({"at": [0.0, 0.0], "target": {"note": other, "view": {"x": 120.0, "y": -40.0, "zoom": 2.0}}}),
    ));
    run(&mut e, "link.activate", json!({"id": l2}));
    assert_eq!(e.inspect(Some("/view/zoom")).unwrap(), json!(2.0));
    run(&mut e, "note.open", json!({"id": n}));
    let l3 = id(&run(
        &mut e,
        "link.create",
        json!({"at": [0.0, 0.0], "target": {"url": "https://example.org"}}),
    ));
    let r = run(&mut e, "link.activate", json!({"id": l3, "dry_run": true}));
    assert_eq!(r["opener"], json!(["xdg-open", "https://example.org"]));
}

/// R-LINK-002: deleted targets → broken, activation is an error (no panic).
#[test]
fn r_link_002_broken_links() {
    let (mut e, _) = engine_with_note();
    let other = id(&run(&mut e, "workspace.note.create", json!({"name": "gone"})));
    let l = id(&run(
        &mut e,
        "link.create",
        json!({"at": [0.0, 0.0], "target": {"note": other}}),
    ));
    run(&mut e, "workspace.note.delete", json!({"id": other}));
    assert_eq!(run(&mut e, "link.status", json!({"id": l}))["status"], "broken");
    assert!(try_run(&mut e, "link.activate", json!({"id": l})).is_err());
}

/// R-LINK-003: moving/renaming the target keeps the link working.
#[test]
fn r_link_003_stable_ids() {
    let (mut e, _) = engine_with_note();
    let f = id(&run(&mut e, "workspace.folder.create", json!({"name": "F"})));
    let other = id(&run(&mut e, "workspace.note.create", json!({"name": "t"})));
    let l = id(&run(
        &mut e,
        "link.create",
        json!({"at": [0.0, 0.0], "target": {"note": other}}),
    ));
    run(&mut e, "workspace.note.rename", json!({"id": other, "name": "renamed"}));
    run(&mut e, "workspace.note.move", json!({"id": other, "folder": f}));
    assert_eq!(run(&mut e, "link.status", json!({"id": l}))["status"], "ok");
}
