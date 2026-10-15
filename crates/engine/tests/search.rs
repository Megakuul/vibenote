#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! Search (M8, M6.6): one index over names, text boxes, links and PDF text that covers notes
//! not loaded, follows every edit, and keeps extracted PDF text across restarts.

use serde_json::{Value, json};
use vibenote_engine::Engine;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hits(e: &mut Engine, q: &str) -> Vec<Value> {
    e.execute("search.query", json!({ "text": q })).unwrap()["results"]
        .as_array()
        .unwrap()
        .clone()
}

fn sources(e: &mut Engine, q: &str) -> Vec<String> {
    hits(e, q)
        .iter()
        .map(|h| h["source"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn unloaded_notes_and_pdf_text_are_found_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let a = e.execute("workspace.note.create", json!({"name": "a"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": a})).unwrap();
    e.execute(
        "text.create",
        json!({"at": [0.0, 0.0], "width": 100.0, "content": "Eigenwertproblem"}),
    )
    .unwrap();
    e.execute(
        "pdf.import",
        json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 500.0]}),
    )
    .unwrap();
    // The background extraction finishes; pump stores it.
    for _ in 0..200 {
        if e.session.pdf.take_texts_pending_for_test() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    e.pump(10);
    e.sync().unwrap();
    drop(e);
    // A new session: note `a` is not loaded; its text and the PDF text are found, the PDF
    // text from storage (the worker does not run).
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(sources(&mut e, "eigenwert"), vec!["text"]);
    let z = hits(&mut e, "ZEBRAFISCH");
    assert_eq!(z.len(), 1);
    assert_eq!(z[0]["page"], json!(2));
    assert!(z[0]["snippet"].as_str().unwrap().contains("Zebrafisch"));
    assert_eq!(e.session.pdf.worker_starts(), 0, "text came from storage");
}

#[test]
fn the_index_follows_edits_undo_and_the_trash() {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "Notiz"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let other = e.execute("workspace.note.create", json!({"name": "Ziel"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    let t = e
        .execute(
            "text.create",
            json!({"at": [10.0, 20.0], "width": 100.0, "content": "Matrix"}),
        )
        .unwrap()["id"]
        .clone();
    assert_eq!(sources(&mut e, "matrix"), vec!["text"]);
    e.execute("text.edit", json!({"id": t, "delete": [0, 6], "insert": "Vektor"}))
        .unwrap();
    assert!(hits(&mut e, "matrix").is_empty());
    assert_eq!(sources(&mut e, "vektor"), vec!["text"]);
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(sources(&mut e, "matrix"), vec!["text"]);
    // Link labels follow the target's name.
    e.execute("link.create", json!({"at": [0.0, 0.0], "target": {"note": other}}))
        .unwrap();
    assert!(sources(&mut e, "ziel").contains(&"link".to_string()));
    e.execute("workspace.note.rename", json!({"id": other, "name": "Destination"}))
        .unwrap();
    assert!(sources(&mut e, "destination").contains(&"link".to_string()));
    // In-note find: regions on the canvas.
    let r = e.execute("search.in_note", json!({"text": "matr"})).unwrap();
    assert_eq!(r["regions"][0]["region"][0], json!(10.0));
    // Trashed notes are not found; restoring brings them back.
    e.execute("workspace.note.delete", json!({"id": n})).unwrap();
    assert!(hits(&mut e, "matrix").is_empty());
    e.execute("trash.restore", json!({"id": n})).unwrap();
    assert_eq!(sources(&mut e, "matrix"), vec!["text"]);
    // Hostile queries.
    assert!(hits(&mut e, "").is_empty());
    let long = "x".repeat(100_000);
    assert!(hits(&mut e, &long).is_empty());
}
