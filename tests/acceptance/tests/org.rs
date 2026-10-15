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
//! R-ORG-001..004 (SPEC §9).

use serde_json::json;
use vibenote_acceptance::*;

/// R-ORG-001: depth-1,000 folder hierarchy works (iterative code, no recursion limits).
#[test]
fn r_org_001_deep_hierarchy() {
    let mut e = vibenote_engine::Engine::new().unwrap();
    let mut parent = serde_json::Value::Null;
    let mut ids = vec![];
    for i in 0..1000 {
        let f = id(&run(
            &mut e,
            "workspace.folder.create",
            json!({"parent": parent, "name": format!("f{i}")}),
        ));
        ids.push(f);
        parent = json!(f);
    }
    let note = id(&run(
        &mut e,
        "workspace.note.create",
        json!({"folder": ids[999], "name": "deep"}),
    ));
    run(
        &mut e,
        "workspace.folder.move",
        json!({"id": ids[500], "parent": serde_json::Value::Null}),
    );
    assert!(
        try_run(
            &mut e,
            "workspace.folder.move",
            json!({"id": ids[500], "parent": ids[999]})
        )
        .is_err(),
        "cycle must be refused"
    );
    assert!(e.inspect(Some(&format!("/ws/notes/{note}"))).is_ok());
}

/// R-ORG-002: name, colour, reorder and move persist.
#[test]
fn r_org_002_organise_persists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    let a = id(&run(&mut e, "workspace.folder.create", json!({"name": "A"})));
    let b = id(&run(&mut e, "workspace.folder.create", json!({"name": "B"})));
    let n = id(&run(&mut e, "workspace.note.create", json!({"folder": a, "name": "n"})));
    run(&mut e, "workspace.folder.rename", json!({"id": b, "name": "Bee"}));
    run(
        &mut e,
        "workspace.folder.set_color",
        json!({"id": a, "color": 0xd6604dff_u32}),
    );
    run(
        &mut e,
        "workspace.folder.move",
        json!({"id": b, "parent": null, "sort": -5}),
    );
    run(&mut e, "workspace.note.move", json!({"id": n, "folder": b}));
    run(
        &mut e,
        "workspace.note.set_color",
        json!({"id": n, "color": 0x5a8cd2ff_u32}),
    );
    let mem = e.session.ws.clone();
    drop(e);
    let mut back = vibenote_engine::Engine::open(&db).unwrap();
    back.load_all().unwrap();
    assert_eq!(back.session.ws, mem);
    assert_eq!(back.session.ws.child_folders(None)[0].name, "Bee", "manual order kept");
}

/// R-ORG-003 / R-PERF-008: quick-open over 100k notes within budget per keystroke.
#[test]
fn r_org_003_quick_open() {
    let (db, _) = reference_db();
    let mut e = vibenote_engine::Engine::open(&db).unwrap();
    for q in ["N", "No", "Not", "Notiz 99"] {
        let t = std::time::Instant::now();
        let r = run(&mut e, "quickopen.query", json!({"text": q, "limit": 20}));
        assert!(t.elapsed().as_secs_f64() * 1e3 <= budget("quickopen", "per_keystroke_ms"));
        assert!(!r["results"].as_array().unwrap().is_empty());
    }
}

/// R-ORG-004: delete goes to trash; restore brings it back.
#[test]
fn r_org_004_trash() {
    let (mut e, _) = engine_with_note();
    let n = id(&run(&mut e, "workspace.note.create", json!({"name": "x"})));
    run(&mut e, "workspace.note.delete", json!({"id": n}));
    run(&mut e, "trash.restore", json!({"id": n}));
    assert!(e.inspect(Some(&format!("/ws/notes/{n}"))).is_ok());
}
