#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! M7 object types: text boxes, links, shapes, snap-from-ink, the ruler, images, and moving /
//! scaling any object (R-TEXT-001/002, R-LINK-001..003, R-SHAPE-001/002, R-RULER-001,
//! R-CANVAS-003, R-DATA-004).

use serde_json::{Value, json};
use vibenote_engine::Engine;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn setup() -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = e.execute("workspace.note.create", json!({"name": "o"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    e.execute("note.open", json!({"id": n})).unwrap();
    (e, n)
}

fn id(v: &Value) -> u64 {
    v["id"].as_u64().unwrap()
}

fn obj(e: &Engine, n: u64, id: u64) -> Value {
    e.inspect(Some(&format!("/ws/canvases/{n}/objects/{id}"))).unwrap()
}

#[test]
fn text_edits_keep_formatting_in_place_and_undo() {
    let (mut e, _) = setup();
    let t = id(&e
        .execute(
            "text.create",
            json!({"at": [0.0, 0.0], "width": 300.0, "content": "Hallo Welt"}),
        )
        .unwrap());
    e.execute("text.format", json!({"id": t, "format": "bold", "range": [6, 10]}))
        .unwrap();
    e.execute(
        "text.format",
        json!({"id": t, "format": "color", "range": [0, 5], "value": 0xd03030ff_u32}),
    )
    .unwrap();
    // Insert before the bold word: the mark moves with it.
    e.execute("text.edit", json!({"id": t, "insert": "schöne ", "at": 6}))
        .unwrap();
    let g = e.execute("text.get", json!({"id": t})).unwrap();
    assert_eq!(g["content"], json!("Hallo schöne Welt"));
    let bold = g["marks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["format"] == "bold")
        .unwrap();
    assert_eq!((bold["start"].clone(), bold["end"].clone()), (json!(13), json!(17)));
    // Delete across the boundary; clear part of a format.
    e.execute("text.edit", json!({"id": t, "delete": [5, 13]})).unwrap();
    assert_eq!(
        e.execute("text.get", json!({"id": t})).unwrap()["content"],
        json!("HalloWelt")
    );
    e.execute(
        "text.format",
        json!({"id": t, "format": "bold", "range": [5, 7], "clear": true}),
    )
    .unwrap();
    let g = e.execute("text.get", json!({"id": t})).unwrap();
    let bold: Vec<(u64, u64)> = g["marks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["format"] == "bold")
        .map(|m| (m["start"].as_u64().unwrap(), m["end"].as_u64().unwrap()))
        .collect();
    assert_eq!(bold, vec![(7, 9)]);
    // Hostile: out-of-range edits and formats change nothing.
    let before = e.execute("text.get", json!({"id": t})).unwrap();
    for (cmd, args) in [
        ("text.edit", json!({"id": t, "insert": "x", "at": 99})),
        ("text.edit", json!({"id": t, "delete": [3, 99]})),
        ("text.format", json!({"id": t, "format": "italic", "range": [4, 4]})),
        (
            "text.format",
            json!({"id": t, "format": "size", "range": [0, 2], "value": 1.0e9}),
        ),
        ("text.format", json!({"id": t, "format": "color", "range": [0, 2]})),
    ] {
        assert!(e.execute(cmd, args.clone()).is_err(), "{cmd} {args}");
    }
    assert_eq!(e.execute("text.get", json!({"id": t})).unwrap(), before);
    for _ in 0..5 {
        e.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(
        e.execute("text.get", json!({"id": t})).unwrap()["content"],
        json!("Hallo Welt")
    );
}

#[test]
fn links_open_notes_folders_and_urls() {
    let (mut e, n) = setup();
    let f = id(&e.execute("workspace.folder.create", json!({"name": "F"})).unwrap());
    let other = id(&e.execute("workspace.note.create", json!({"name": "t"})).unwrap());
    let lf = id(&e
        .execute("link.create", json!({"at": [0.0, 0.0], "target": {"folder": f}}))
        .unwrap());
    e.execute("link.activate", json!({"id": lf})).unwrap();
    assert_eq!(e.inspect(Some("/ui/reveal_folder")).unwrap(), json!(f));
    let lv = id(&e
        .execute(
            "link.create",
            json!({"at": [0.0, 0.0], "target": {"note": other, "view": {"x": 5.0, "y": 6.0, "zoom": 1.0e9}}}),
        )
        .unwrap());
    e.execute("link.activate", json!({"id": lv})).unwrap();
    assert_eq!(e.inspect(Some("/current_note")).unwrap(), json!(other));
    assert_eq!(
        e.inspect(Some("/view/zoom")).unwrap(),
        json!(64.0),
        "zoom clamped to the range"
    );
    e.execute("note.open", json!({"id": n})).unwrap();
    for bad in [
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "javascript:alert(1)"}),
        json!({"url": "https://a b"}),
        json!({"note": 999_999}),
        json!({"note": other, "folder": f}),
        json!({"folder": f, "view": {"x": 0.0, "y": 0.0, "zoom": 1.0}}),
    ] {
        assert!(
            e.execute("link.create", json!({"at": [0.0, 0.0], "target": bad}))
                .is_err(),
            "{bad}"
        );
    }
    // A trashed folder breaks its link.
    e.execute("workspace.folder.delete", json!({"id": f})).unwrap();
    assert_eq!(
        e.execute("link.status", json!({"id": lf})).unwrap()["status"],
        json!("broken")
    );
}

#[test]
fn shapes_snap_and_undo_back_to_ink() {
    let (mut e, n) = setup();
    for (kind, args) in [
        ("line", json!({"from": [0.0, 0.0], "to": [10.0, 0.0]})),
        ("ellipse", json!({"from": [0.0, 0.0], "to": [40.0, 20.0]})),
        ("triangle", json!({"points": [[0.0, 0.0], [10.0, 0.0], [5.0, 8.0]]})),
    ] {
        let mut a = args.clone();
        a["kind"] = json!(kind);
        e.execute("shape.create", a).unwrap();
    }
    assert!(
        e.execute(
            "shape.create",
            json!({"kind": "triangle", "points": [[0.0, 0.0], [1.0, 1.0]]})
        )
        .is_err()
    );
    assert!(
        e.execute("shape.create", json!({"kind": "rectangle", "from": [0.0, 0.0]}))
            .is_err()
    );
    // A rough rectangle, held: becomes a rectangle; undo returns the ink.
    let mut samples = vec![];
    for (k, (x, y)) in [(0.0, 0.0), (100.0, 2.0), (101.0, 60.0), (1.0, 61.0), (0.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        samples.push(json!({"x": x, "y": y, "p": 0.5, "t_us": k as u64 * 1000}));
    }
    let mut dense = vec![];
    for w in samples.windows(2) {
        for k in 0..10 {
            let t = f64::from(k) / 10.0;
            let (x0, y0, x1, y1) = (
                w[0]["x"].as_f64().unwrap(),
                w[0]["y"].as_f64().unwrap(),
                w[1]["x"].as_f64().unwrap(),
                w[1]["y"].as_f64().unwrap(),
            );
            dense.push(json!({"x": x0 + (x1 - x0) * t, "y": y0 + (y1 - y0) * t, "p": 0.5}));
        }
    }
    dense.push(samples[4].clone());
    let s = id(&e.execute("ink.stroke.add", json!({"samples": dense})).unwrap());
    assert_eq!(
        e.execute("shape.snap", json!({"stroke": s, "held_ms": 200})).unwrap()["kind"],
        Value::Null
    );
    let r = e.execute("shape.snap", json!({"stroke": s, "held_ms": 700})).unwrap();
    assert_eq!(r["kind"], json!("rectangle"));
    assert!(e.inspect(Some(&format!("/ws/canvases/{n}/objects/{s}"))).is_err());
    e.execute("edit.undo", json!({})).unwrap();
    assert_eq!(obj(&e, n, s)["kind"], json!("stroke"));
}

#[test]
fn the_ruler_snaps_nearby_strokes_only() {
    let (mut e, n) = setup();
    e.execute(
        "ruler.toggle",
        json!({"visible": true, "at": [0.0, 0.0], "angle_deg": 47.0, "snap": true}),
    )
    .unwrap();
    assert_eq!(e.inspect(Some("/ruler/angle_deg")).unwrap(), json!(45.0));
    let near = id(&e
        .execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 10.0, "y": 18.0, "p": 0.5}, {"x": 100.0, "y": 96.0, "p": 0.5, "t_us": 1}]}),
        )
        .unwrap());
    let far = id(&e
        .execute(
            "ink.stroke.add",
            json!({"samples": [{"x": 10.0, "y": 60.0, "p": 0.5}, {"x": 100.0, "y": 140.0, "p": 0.5, "t_us": 1}]}),
        )
        .unwrap());
    for p in obj(&e, n, near)["samples"].as_array().unwrap() {
        assert!(
            (p["x"].as_f64().unwrap() - p["y"].as_f64().unwrap()).abs() < 1e-3,
            "on the 45° edge"
        );
    }
    assert_eq!(obj(&e, n, far)["samples"][0]["y"], json!(60.0));
    e.execute("ruler.toggle", json!({})).unwrap();
    assert_eq!(e.inspect(Some("/ruler/visible")).unwrap(), json!(false));
}

#[test]
fn images_import_from_png_and_svg_and_reject_junk() {
    let (mut e, n) = setup();
    let png = root().join("tests/golden/loops-4.png");
    let r = e
        .execute("image.import", json!({"path": png, "at": [0.0, 0.0]}))
        .unwrap();
    assert_eq!(r["pixels"], json!([1200, 800]));
    assert_eq!(
        (r["width"].clone(), r["height"].clone()),
        (json!(800.0), json!(800.0 * 800.0 / 1200.0))
    );
    let dir = tempfile::tempdir().unwrap();
    let svg = dir.path().join("x.svg");
    std::fs::write(
        &svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60"><circle cx="30" cy="30" r="20"/></svg>"#,
    )
    .unwrap();
    let r2 = e
        .execute("image.import", json!({"path": svg, "at": [0.0, 600.0], "width": 240.0}))
        .unwrap();
    assert_eq!(r2["height"], json!(120.0));
    let before = e.inspect(Some(&format!("/ws/canvases/{n}"))).unwrap();
    for (name, bytes) in [
        ("a.png", b"\x89PNG\r\n\x1a\nnope".to_vec()),
        ("b.jpg", vec![0xff, 0xd8, 0xff, 0x00]),
        ("c.txt", b"hello".to_vec()),
        ("d.gif", b"GIF89a".to_vec()),
    ] {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        assert!(
            e.execute("image.import", json!({"path": p, "at": [0.0, 0.0]})).is_err(),
            "{name}"
        );
    }
    assert_eq!(e.inspect(Some(&format!("/ws/canvases/{n}"))).unwrap(), before);
    // Shown: a headless view decodes it.
    let imgs = e.session.images_in_view((0.0, 0.0), 1.0, (1000.0, 1000.0), true);
    assert_eq!(imgs.len(), 2);
}

#[test]
fn every_object_type_moves_and_scales() {
    let (mut e, n) = setup();
    let ids = [
        id(&e
            .execute(
                "text.create",
                json!({"at": [10.0, 10.0], "width": 200.0, "content": "Hallo"}),
            )
            .unwrap()),
        id(&e
            .execute(
                "pdf.import",
                json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 300.0]}),
            )
            .unwrap()),
        id(&e
            .execute(
                "image.import",
                json!({"path": root().join("tests/golden/loops-4.png"), "at": [500.0, 0.0]}),
            )
            .unwrap()),
        id(&e
            .execute(
                "link.create",
                json!({"at": [5.0, 5.0], "target": {"url": "https://example.org"}}),
            )
            .unwrap()),
        id(&e
            .execute(
                "shape.create",
                json!({"kind": "rectangle", "from": [0.0, 0.0], "to": [50.0, 30.0]}),
            )
            .unwrap()),
    ];
    e.execute("selection.set", json!({"ids": ids})).unwrap();
    let before: Vec<Value> = ids.iter().map(|i| obj(&e, n, *i)).collect();
    e.execute(
        "selection.transform",
        json!({"translate": [10.0, 20.0], "scale": 2.0, "pivot": [0.0, 0.0]}),
    )
    .unwrap();
    assert_eq!(obj(&e, n, ids[0])["origin"], json!({"x": 30.0, "y": 40.0}));
    assert_eq!(obj(&e, n, ids[0])["size"], json!(32.0));
    assert_eq!(obj(&e, n, ids[1])["scale"].as_f64().unwrap(), 2.0 * 96.0 / 72.0);
    assert_eq!(obj(&e, n, ids[2])["width"], json!(1600.0));
    assert_eq!(obj(&e, n, ids[4])["points"][1], json!({"x": 110.0, "y": 80.0}));
    assert!(
        e.execute("selection.transform", json!({"rotate_deg": 30.0})).is_err(),
        "text cannot rotate"
    );
    e.execute("edit.undo", json!({})).unwrap();
    let after: Vec<Value> = ids.iter().map(|i| obj(&e, n, *i)).collect();
    assert_eq!(after, before);
}
