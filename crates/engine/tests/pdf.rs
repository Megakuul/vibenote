#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]
//! PDFs in the engine (M6.3): import as one object, blob kept, page rendering and caching,
//! hit-testing, undo, persistence, hostile files.

use serde_json::{Value, json};
use vibenote_engine::Engine;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pdf(name: &str) -> String {
    root()
        .join("tests/fixtures/pdf")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn setup(e: &mut Engine) -> u64 {
    let n = e.execute("workspace.note.create", json!({"name": "p"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    n
}

#[test]
fn import_renders_and_times() {
    let mut e = Engine::new().unwrap();
    setup(&mut e);
    let t = std::time::Instant::now();
    let r = e
        .execute("pdf.import", json!({"path": pdf("three-pages.pdf"), "at": [0.0, 0.0]}))
        .unwrap();
    let import_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    let p = e
        .execute("pdf.render_page", json!({"id": r["id"], "page": 3, "width_px": 1240}))
        .unwrap();
    let render_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    e.execute("pdf.render_page", json!({"id": r["id"], "page": 3, "width_px": 1240}))
        .unwrap();
    let cached_ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("import {import_ms:.1} ms, render {render_ms:.1} ms, cached {cached_ms:.2} ms, bitmap {p}");
    assert!(cached_ms < 5.0);
}

#[test]
fn import_is_one_undoable_object_that_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let n = setup(&mut e);
    let r = e
        .execute(
            "pdf.import",
            json!({"path": pdf("three-pages.pdf"), "at": [100.0, 50.0]}),
        )
        .unwrap();
    let pages = r["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(
        (pages[0]["x"].clone(), pages[0]["y"].clone()),
        (json!(100.0), json!(50.0))
    );
    let blob = r["blob"].as_str().unwrap().to_string();
    let bytes = e.execute("blob.read", json!({"hash": blob})).unwrap();
    let stored: Vec<u8> = serde_json::from_value(bytes["bytes"].clone()).unwrap();
    assert_eq!(stored, std::fs::read(pdf("three-pages.pdf")).unwrap());
    // Ink drawn later is on top of the page; hit-testing finds it first, then the PDF.
    let s = e
        .execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 150.0, "y": 100.0, "p": 0.5}, {"x": 250.0, "y": 100.0, "p": 0.5, "t_us": 1}]}),
        )
        .unwrap()["id"]
        .clone();
    let hit = e.execute("canvas.hit_test", json!({"at": [200.0, 100.0]})).unwrap();
    assert_eq!(hit["ids"], json!([s, r["id"]]));
    let gap_hit = e
        .execute("canvas.hit_test", json!({"at": [200.0, 50.0 + 793.7 + 12.0]}))
        .unwrap();
    assert_eq!(gap_hit["ids"], json!([]), "between pages is not the PDF");
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    e.load_all().unwrap();
    // Reopened: the page renders from the stored blob.
    let p = e
        .execute("pdf.render_page", json!({"id": r["id"], "page": 2, "width_px": 300}))
        .unwrap();
    assert_eq!(p["width"], json!(362));
    e.execute("edit.undo", json!({})).unwrap(); // the stroke
    e.execute("edit.undo", json!({})).unwrap(); // the import
    assert!(
        e.session
            .ws
            .canvas(vibenote_model::NoteId(n))
            .unwrap()
            .objects
            .is_empty()
    );
    // Undo keeps the blob (redo needs it); nothing referenced → collected after the history goes.
    e.execute("blob.gc", json!({})).unwrap();
    assert!(e.execute("blob.read", json!({"hash": blob})).is_ok());
}

#[test]
fn hostile_imports_change_nothing() {
    let mut e = Engine::new().unwrap();
    setup(&mut e);
    let before: Value = e.inspect(None).unwrap();
    for (path, at) in [
        (pdf("corrupt.pdf"), json!([0.0, 0.0])),
        (pdf("truncated.pdf"), json!([0.0, 0.0])),
        ("/nonexistent/x.pdf".to_string(), json!([0.0, 0.0])),
        (root().to_string_lossy().into_owned(), json!([0.0, 0.0])),
        (pdf("three-pages.pdf"), json!([1.0e12, 0.0])),
    ] {
        assert!(
            e.execute("pdf.import", json!({"path": path, "at": at})).is_err(),
            "{path}"
        );
    }
    assert_eq!(e.inspect(None).unwrap(), before);
    assert!(
        e.execute("pdf.render_page", json!({"id": 999, "page": 1, "width_px": 100}))
            .is_err()
    );
}

#[test]
fn detach_and_rejoin_pages_undoably() {
    let mut e = Engine::new().unwrap();
    let n = setup(&mut e);
    let r = e
        .execute("pdf.import", json!({"path": pdf("three-pages.pdf"), "at": [0.0, 0.0]}))
        .unwrap();
    let id = r["id"].clone();
    let objs = |e: &Engine| e.inspect(Some(&format!("/ws/canvases/{n}/objects"))).unwrap();
    let before = objs(&e);
    let d = e.execute("pdf.page_detach", json!({"id": id, "page": 2})).unwrap();
    let after = objs(&e);
    let obj = |v: &Value, id: &Value| v[id.to_string()].clone();
    // The original keeps pages 1 and 3 (closed up); the detached page stays where it was.
    let pages: Vec<u64> = obj(&after, &id)["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["index"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, vec![0, 2]);
    let det = obj(&after, &d["id"]);
    assert_eq!(det["pages"][0]["index"], json!(1));
    assert_eq!(
        det["origin"]["y"].as_f64().unwrap(),
        r["pages"][1]["y"].as_f64().unwrap()
    );
    assert!(
        e.execute("pdf.page_detach", json!({"id": d["id"], "page": 1})).is_err(),
        "last page"
    );
    assert!(e.execute("pdf.page_detach", json!({"id": id, "page": 5})).is_err());
    // Rejoin restores the page order; undo twice gets back the import exactly.
    e.execute("pdf.page_rejoin", json!({"id": d["id"], "into": id}))
        .unwrap();
    let pages: Vec<u64> = obj(&objs(&e), &id)["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["index"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, vec![0, 1, 2]);
    assert_eq!(objs(&e).as_object().unwrap().len(), 1);
    e.execute("edit.undo", json!({})).unwrap();
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(objs(&e), before);
}

/// A valid PDF of `n` A4 pages, page k showing "Seite k" (Helvetica, not embedded).
fn many_pages(n: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &str| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", offsets.len()).as_bytes());
    };
    let kids: Vec<String> = (0..n).map(|k| format!("{} 0 R", 4 + 2 * k)).collect();
    obj(&mut out, &mut offsets, "<< /Type /Catalog /Pages 2 0 R >>");
    obj(
        &mut out,
        &mut offsets,
        &format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
    );
    obj(
        &mut out,
        &mut offsets,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    for k in 0..n {
        obj(
            &mut out,
            &mut offsets,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * k
            ),
        );
        let text = format!("BT /F1 24 Tf 72 760 Td (Seite {}) Tj ET", k + 1);
        obj(
            &mut out,
            &mut offsets,
            &format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()),
        );
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// R-PDF-003 / R-PERF-006: a 2,000-page PDF imports and shows its first page within 2 s; pages
/// render lazily, one at a time, anywhere in the document.
#[test]
fn a_2000_page_pdf_imports_fast_and_renders_lazily() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.pdf");
    std::fs::write(&path, many_pages(2000)).unwrap();
    let mut e = Engine::new().unwrap();
    setup(&mut e);
    let t = std::time::Instant::now();
    let r = e
        .execute("pdf.import", json!({"path": path, "at": [0.0, 0.0]}))
        .unwrap();
    e.execute("pdf.render_page", json!({"id": r["id"], "page": 1, "width_px": 1240}))
        .unwrap();
    let first_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(r["pages"].as_array().unwrap().len(), 2000);
    assert!(first_ms <= 2000.0, "import + first page {first_ms:.0} ms");
    let t = std::time::Instant::now();
    e.execute(
        "pdf.render_page",
        json!({"id": r["id"], "page": 1999, "width_px": 1240}),
    )
    .unwrap();
    let deep_ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(deep_ms <= 100.0, "page 1999 {deep_ms:.0} ms");
    eprintln!("2,000 pages: import + first page {first_ms:.0} ms, page 1999 {deep_ms:.0} ms");
    // The page column is as long as 2,000 A4 pages plus gaps.
    let last = &r["pages"][1999];
    let want = 1999.0 * (842.0 * 96.0 / 72.0 + 24.0);
    assert!((last["y"].as_f64().unwrap() - want).abs() < 1e-6);
}

/// Regression (M6–M9 review): the worker keeps 8 documents open. Earlier documents were evicted
/// behind the client's back and then failed to render and came back without text.
#[test]
fn more_documents_than_the_worker_keeps_open_still_render_and_have_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::new().unwrap();
    setup(&mut e);
    let mut ids = Vec::new();
    for k in 0..10 {
        let path = dir.path().join(format!("{k}.pdf"));
        std::fs::write(&path, many_pages(k + 1)).unwrap();
        let r = e
            .execute("pdf.import", json!({"path": path, "at": [0.0, 2000.0 * k as f64]}))
            .unwrap();
        ids.push(r["id"].clone());
    }
    for (k, id) in ids.iter().enumerate() {
        e.execute("pdf.render_page", json!({"id": id, "page": 1, "width_px": 300 + k}))
            .unwrap_or_else(|err| panic!("document {k}: {err}"));
    }
    let r = e.execute("search.query", json!({"text": "Seite 10"})).unwrap();
    let pdf_hits: Vec<&Value> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["source"] == "pdf")
        .collect();
    assert_eq!(pdf_hits.len(), 1, "{r}");
    // Every document's text was extracted (9 documents have a page "Seite 2").
    let r = e.execute("search.query", json!({"text": "Seite 2"})).unwrap();
    let n = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["source"] == "pdf")
        .count();
    assert_eq!(n, 9, "{r}");
}
