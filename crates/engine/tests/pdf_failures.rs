#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! A PDF whose text cannot be extracted must not break search (M6–M9 review). The parent
//! imports a PDF with the real worker; a child process (this test binary again, with the worker
//! path pointing at a missing program) reopens the workspace and searches.

use serde_json::json;
use vibenote_engine::Engine;

const CHILD: &str = "VIBENOTE_TEST_PDF_FAIL_DB";

#[test]
fn search_works_when_pdf_text_extraction_fails() {
    if let Some(db) = std::env::var_os(CHILD) {
        child(std::path::Path::new(&db));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("w.sqlite");
    let pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pdf/three-pages.pdf");
    {
        let mut e = Engine::open(&db).unwrap();
        let n = e.execute("workspace.note.create", json!({"name": "Notiz"})).unwrap()["id"].clone();
        e.execute("note.open", json!({"id": n})).unwrap();
        e.execute("pdf.import", json!({"path": pdf, "at": [0.0, 0.0]})).unwrap();
        // Dropped before any search or pump: the PDF's text is not stored.
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "search_works_when_pdf_text_extraction_fails", "--nocapture"])
        .env(CHILD, &db)
        .env("VIBENOTE_PDFD", dir.path().join("no-such-worker"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
}

fn child(db: &std::path::Path) {
    let mut e = Engine::open(db).unwrap();
    e.load_all().unwrap();
    let r = e.execute("search.query", json!({"text": "Notiz"})).unwrap();
    assert_eq!(r["results"].as_array().unwrap().len(), 1, "{r}");
    // The failure is a warning and is remembered: the next search does not try again.
    assert!(
        e.session.warnings.iter().any(|w| w.contains("could not be extracted")),
        "{:?}",
        e.session.warnings
    );
    let starts = e.session.pdf.worker_starts();
    let t = std::time::Instant::now();
    e.execute("search.query", json!({"text": "Notiz"})).unwrap();
    assert_eq!(e.session.pdf.worker_starts(), starts, "no new attempt");
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    // Without waiting nothing blocks either.
    let r = e
        .execute("search.query", json!({"text": "Notiz", "wait": false}))
        .unwrap();
    assert_eq!(r["results"].as_array().unwrap().len(), 1, "{r}");
}
