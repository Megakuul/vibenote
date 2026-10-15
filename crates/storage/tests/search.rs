#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! The FTS5 search index (ADR 0005, M8) is written with the objects it describes.

use vibenote_geom::Point;
use vibenote_model::search::forms;
use vibenote_model::{Canvas, CanvasObject, Note, NoteId, ObjectId, ObjectKind, Op, PdfPage};
use vibenote_storage::{Derived, Store};

fn q(s: &str) -> (String, String) {
    forms(s)
}

fn text(id: u64, content: &str) -> CanvasObject {
    CanvasObject {
        id: ObjectId(id),
        z: 0,
        kind: ObjectKind::Text {
            origin: Point::new(1.0, 2.0),
            width: 100.0,
            content: content.into(),
            marks: vec![],
            size: 16.0,
        },
    }
}

#[test]
fn the_index_follows_object_writes_and_pdf_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    let n = NoteId(1);
    let note = Note {
        id: n,
        folder: None,
        name: "n".into(),
        color: None,
        sort: 0,
        background: None,
    };
    st.apply(
        &[Op::CreateNote {
            note,
            canvas: Canvas::default(),
        }],
        &[],
        10,
    )
    .unwrap();
    st.apply(
        &[Op::InsertObject {
            note: n,
            object: text(2, "Größe der Matrix"),
        }],
        &[],
        10,
    )
    .unwrap();
    let hit = st.search(&q("groesse"), 10, None).unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!((hit[0].object, hit[0].source.as_str()), (ObjectId(2), "text"));
    assert_eq!(
        st.search(&q("matr"), 10, Some(NoteId(99))).unwrap().len(),
        0,
        "other note"
    );
    st.apply(
        &[Op::ReplaceObject {
            note: n,
            after: text(2, "Vektor"),
        }],
        &[],
        10,
    )
    .unwrap();
    assert!(st.search(&q("matrix"), 10, None).unwrap().is_empty());
    assert_eq!(st.search(&q("vektor"), 10, None).unwrap().len(), 1);
    // A PDF object: no text yet → not found; text arrives → its page is found.
    let blob = "ab".repeat(32);
    let pdf = CanvasObject {
        id: ObjectId(3),
        z: 1,
        kind: ObjectKind::Pdf {
            origin: Point::new(0.0, 0.0),
            scale: 1.0,
            gap: 10.0,
            blob: blob.clone(),
            pages: vec![
                PdfPage {
                    index: 0,
                    width: 100.0,
                    height: 100.0,
                },
                PdfPage {
                    index: 1,
                    width: 100.0,
                    height: 100.0,
                },
            ],
        },
    };
    st.apply(
        &[Op::InsertObject {
            note: n,
            object: pdf.clone(),
        }],
        &[],
        10,
    )
    .unwrap();
    assert_eq!(st.pdf_blobs_without_text().unwrap(), vec![blob.clone()]);
    st.apply_batch_meta(
        &[],
        &[],
        10,
        None,
        &[],
        &[Derived::PdfText {
            blob: blob.clone(),
            pages: vec!["eins".into(), "Zebrafisch".into()],
        }],
    )
    .unwrap();
    let z = st.search(&q("zebra"), 10, None).unwrap();
    assert_eq!((z.len(), z[0].page), (1, Some(2)));
    assert!(
        (z[0].region.unwrap().min.y - 110.0).abs() < 1e-9,
        "page 2 is below page 1 and the gap"
    );
    assert!(st.pdf_blobs_without_text().unwrap().is_empty());
    st.apply(&[Op::RemoveObject { note: n, object: pdf }], &[], 10).unwrap();
    assert!(st.search(&q("zebra"), 10, None).unwrap().is_empty());
    drop(st);
    let st = Store::open(&path).unwrap();
    assert_eq!(st.search(&q("VEKTOR"), 10, None).unwrap().len(), 1);
    // LIKE wildcards in the query are literal.
    assert!(st.search(&q("v%r"), 10, None).unwrap().is_empty());
}

/// Regression (M6–M9 review): with `LIKE … ESCAPE` FTS5 scanned every row. Queries now use the
/// trigram index, and wildcard characters in a query still match only themselves.
#[test]
fn queries_use_the_trigram_index_and_match_wildcards_literally() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    let n = NoteId(1);
    st.apply(
        &[Op::CreateNote {
            note: Note {
                id: n,
                folder: None,
                name: "n".into(),
                color: None,
                sort: 0,
                background: None,
            },
            canvas: Canvas::default(),
        }],
        &[],
        100,
    )
    .unwrap();
    let texts = [
        "Rabatt 50% heute",
        "a*b ist kein Muster",
        "was?wo",
        "Liste [1] Punkt",
        "a_b und 50x",
    ];
    for (k, t) in texts.iter().enumerate() {
        st.apply(
            &[Op::InsertObject {
                note: n,
                object: text(10 + k as u64, t),
            }],
            &[],
            100,
        )
        .unwrap();
    }
    let hits = |s: &str| {
        let mut v: Vec<u64> = st.search(&q(s), 50, None).unwrap().iter().map(|h| h.object.0).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(hits("50%"), vec![10]);
    assert_eq!(hits("a*b"), vec![11]);
    assert_eq!(hits("s?w"), vec![12]);
    assert_eq!(hits("[1]"), vec![13]);
    assert_eq!(hits("a_b"), vec![14]);
    assert_eq!(hits("RABATT"), vec![10]);
    drop(st);
    // The plan reads the FTS5 index (a full scan shows up as `INDEX 0:`).
    let c = rusqlite::Connection::open(&path).unwrap();
    let plan: Vec<String> = c
        .prepare(
            "EXPLAIN QUERY PLAN SELECT r.object FROM search_fts f JOIN search_row r ON r.rowid = f.rowid
             WHERE (f.a GLOB '*rabatt*' OR f.b GLOB '*rabatt*')",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        !plan.iter().any(|l| l.trim_end().ends_with("INDEX 0:")),
        "full scan: {plan:?}"
    );
}
