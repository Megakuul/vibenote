#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
//! Schema v3 (M2.3, R-PERF-004 / R-DATA-003): metadata-only load, viewport query through the
//! per-note R-tree, keyset paging, topmost z, migration from v2.

use proptest::prelude::*;
use vibenote_geom::{Point, Rect};
use vibenote_model::*;
use vibenote_storage::{SCHEMA_VERSION, Store};

fn stroke_at(x: f64, y: f64, len: f64, width: f32) -> Stroke {
    Stroke {
        tool: Tool::Pen,
        color: Color::INK,
        width,
        samples: (0..6)
            .map(|k| {
                let u = f64::from(k) / 5.0;
                Sample {
                    x: x + len * u,
                    y: y + len * 0.5 * (u * 6.0).sin(),
                    pressure: 0.5,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: u64::from(k as u8) * 1000,
                }
                .quantised()
            })
            .collect(),
    }
}

fn text(x: f64) -> ObjectKind {
    ObjectKind::Text {
        origin: Point::new(x, 7.0),
        width: 100.0,
        content: "Hallo".into(),
        marks: vec![],
        size: 16.0,
    }
}

/// Two notes in one folder; `objs` go round-robin into them (every 5th object is a text box).
fn workspace(objs: &[(f64, f64, f64, f32)]) -> (Workspace, [NoteId; 2]) {
    let mut ws = Workspace::new();
    let f = FolderId(ws.alloc_id().unwrap());
    ws.apply(Op::CreateFolder {
        folder: Folder {
            id: f,
            parent: None,
            name: "F".into(),
            color: None,
            sort: 0,
        },
    })
    .unwrap();
    let mut notes = [NoteId(0); 2];
    for (k, n) in notes.iter_mut().enumerate() {
        *n = NoteId(ws.alloc_id().unwrap());
        ws.apply(Op::CreateNote {
            note: Note {
                id: *n,
                folder: Some(f),
                name: format!("n{k}"),
                color: None,
                sort: k as i64,
                background: None,
            },
            canvas: Canvas::default(),
        })
        .unwrap();
    }
    for (k, &(x, y, len, w)) in objs.iter().enumerate() {
        let id = ObjectId(ws.alloc_id().unwrap());
        let kind = if k % 5 == 4 {
            text(x)
        } else {
            ObjectKind::Stroke(stroke_at(x, y, len, w))
        };
        ws.apply(Op::InsertObject {
            note: notes[k % 2],
            object: CanvasObject {
                id,
                z: (k as i64 * 37) % 101 - 50,
                kind,
            },
        })
        .unwrap();
    }
    (ws, notes)
}

fn saved(ws: &Workspace) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    Store::open(&path).unwrap().save_all(ws).unwrap();
    let st = Store::open(&path).unwrap();
    (dir, st)
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.y <= b.max.y && b.min.y <= a.max.y
}

fn ids(v: &[CanvasObject]) -> Vec<u64> {
    let mut v: Vec<u64> = v.iter().map(|o| o.id.0).collect();
    v.sort_unstable();
    v
}

fn arb_objs() -> impl Strategy<Value = Vec<(f64, f64, f64, f32)>> {
    prop::collection::vec((-5e3f64..5e3, -5e3f64..5e3, 0.5f64..400.0, 0.5f32..40.0), 0..80)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every object whose bounds meet the area comes back, objects without bounds always do,
    /// nothing of another note does, and everything else returned is within f32 rounding of
    /// the area (the R-tree stores f32 boxes, rounded outwards).
    #[test]
    fn area_query_matches_brute_force(
        objs in arb_objs(),
        (ax, ay, aw, ah) in (-6e3f64..6e3, -6e3f64..6e3, 1.0f64..4e3, 1.0f64..4e3),
    ) {
        let (ws, notes) = workspace(&objs);
        let (_d, st) = saved(&ws);
        let area = Rect { min: Point::new(ax, ay), max: Point::new(ax + aw, ay + ah) };
        let slack = Rect { min: Point::new(ax - 1e-2, ay - 1e-2), max: Point::new(ax + aw + 1e-2, ay + ah + 1e-2) };
        for n in notes {
            let got = st.load_area(n, &area).unwrap();
            let c = &ws.canvases[&n];
            for o in &got {
                prop_assert_eq!(c.objects.get(&o.id), Some(o), "object of this note, unchanged");
            }
            let want: Vec<CanvasObject> = c.objects.values().filter(|o| match &o.kind {
                ObjectKind::Stroke(s) => intersects(&s.bounds(), &area),
                _ => true,
            }).cloned().collect();
            let got_ids = ids(&got);
            for id in ids(&want) {
                prop_assert!(got_ids.binary_search(&id).is_ok(), "missing {}", id);
            }
            for o in &got {
                if let ObjectKind::Stroke(s) = &o.kind {
                    prop_assert!(intersects(&s.bounds(), &slack), "{} far outside the area", o.id);
                }
            }
        }
    }

    /// Paging returns each object of the note exactly once, in id order, for any page size.
    #[test]
    fn pages_cover_the_note_once(objs in arb_objs(), limit in 1u32..20) {
        let (ws, notes) = workspace(&objs);
        let (_d, st) = saved(&ws);
        for n in notes {
            let mut all = Vec::new();
            let mut after = None;
            loop {
                let page = st.load_page(n, after, limit).unwrap();
                prop_assert!(page.len() <= limit as usize);
                after = page.last().map(|o| o.id).or(after);
                let last = page.len() < limit as usize;
                all.extend(page);
                if last { break; }
            }
            let want: Vec<CanvasObject> = ws.canvases[&n].objects.values().cloned().collect();
            prop_assert_eq!(all, want);
            let max = ws.canvases[&n].objects.values().map(|o| o.z).max();
            prop_assert_eq!(st.max_z(n).unwrap(), max);
        }
    }
}

#[test]
fn meta_load_has_the_tree_and_no_objects() {
    let objs: Vec<_> = (0..20).map(|k| (f64::from(k) * 50.0, 0.0, 30.0, 2.0)).collect();
    let (ws, notes) = workspace(&objs);
    let (_d, st) = saved(&ws);
    let meta = st.load_meta().unwrap();
    assert_eq!(meta.folders, ws.folders);
    assert_eq!(meta.notes, ws.notes);
    assert_eq!(meta.next_id, ws.next_id);
    for n in notes {
        assert!(meta.canvases[&n].objects.is_empty());
    }
    assert_eq!(st.load().unwrap(), ws);
}

/// Edits keep the R-tree in step: removed objects leave it, replaced ones move.
#[test]
fn rtree_follows_edits() {
    let objs: Vec<_> = (0..10).map(|k| (f64::from(k) * 100.0, 0.0, 20.0, 1.0)).collect();
    let (mut ws, notes) = workspace(&objs);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    st.save_all(&ws).unwrap();
    let n = notes[0];
    let mut it = ws.canvases[&n]
        .objects
        .values()
        .filter(|o| matches!(o.kind, ObjectKind::Stroke(_)));
    let (a, b) = (it.next().unwrap().clone(), it.next().unwrap().clone());
    let moved = CanvasObject {
        kind: ObjectKind::Stroke(stroke_at(9000.0, 9000.0, 10.0, 1.0)),
        ..b.clone()
    };
    let ops = vec![
        Op::RemoveObject {
            note: n,
            object: a.clone(),
        },
        Op::ReplaceObject {
            note: n,
            after: moved.clone(),
        },
    ];
    ws.apply_all(ops.clone()).unwrap();
    st.apply(&ops, &[], ws.next_id).unwrap();
    let near = Rect {
        min: Point::new(-100.0, -100.0),
        max: Point::new(2000.0, 100.0),
    };
    let got = ids(&st.load_area(n, &near).unwrap());
    assert!(got.binary_search(&a.id.0).is_err(), "removed object left the index");
    assert!(got.binary_search(&b.id.0).is_err(), "replaced object moved away");
    let far = Rect {
        min: Point::new(8990.0, 8990.0),
        max: Point::new(9100.0, 9100.0),
    };
    let got = st.load_area(n, &far).unwrap();
    assert!(got.contains(&moved));
    assert_eq!(st.load().unwrap(), ws);
}

/// The exact v1 schema and v1 → v2 migration as shipped (frozen copies; do not edit).
const V1: &str = "BEGIN;
 CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS folder(id INTEGER PRIMARY KEY, parent INTEGER, name TEXT NOT NULL, color INTEGER, sort INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS note(id INTEGER PRIMARY KEY, folder INTEGER, name TEXT NOT NULL, color INTEGER, sort INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS object(id INTEGER PRIMARY KEY, note INTEGER NOT NULL, z INTEGER NOT NULL, kind TEXT NOT NULL, data BLOB NOT NULL);
 CREATE INDEX IF NOT EXISTS object_note ON object(note);
 CREATE VIRTUAL TABLE IF NOT EXISTS object_rt USING rtree(id, minx, maxx, miny, maxy);
 INSERT OR IGNORE INTO meta VALUES('next_id', '1');
 PRAGMA user_version = 1;
 COMMIT;";
const V2: &str = "BEGIN;
 CREATE TABLE IF NOT EXISTS history(scope TEXT NOT NULL, kind INTEGER NOT NULL, seq INTEGER NOT NULL,
     label TEXT NOT NULL, ops BLOB NOT NULL, PRIMARY KEY(scope, kind, seq)) WITHOUT ROWID;
 PRAGMA user_version = 2;
 COMMIT;";

#[test]
fn migrates_a_v2_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.sqlite");
    let s = stroke_at(100.0, 100.0, 50.0, 2.0);
    let b = s.bounds();
    let t = serde_json::to_vec(&text(5.0)).unwrap();
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(V1).unwrap();
        c.execute_batch(V2).unwrap();
        c.execute_batch(
            "INSERT INTO note VALUES(2, NULL, 'a', NULL, 0);
             INSERT INTO note VALUES(3, NULL, 'b', NULL, 1);
             UPDATE meta SET value = '7' WHERE key = 'next_id';",
        )
        .unwrap();
        // v2 wrote strokes with a 2-D box and other kinds without one.
        c.execute(
            "INSERT INTO object VALUES(4, 2, 9, 'stroke', ?1)",
            [vibenote_storage::encode_stroke(&s).unwrap()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO object_rt VALUES(4, ?1, ?2, ?3, ?4)",
            [b.min.x, b.max.x, b.min.y, b.max.y],
        )
        .unwrap();
        c.execute("INSERT INTO object VALUES(5, 2, 3, 'json', ?1)", [&t])
            .unwrap();
        c.execute(
            "INSERT INTO object VALUES(6, 3, 1, 'stroke', ?1)",
            [vibenote_storage::encode_stroke(&s).unwrap()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO object_rt VALUES(6, ?1, ?2, ?3, ?4)",
            [b.min.x, b.max.x, b.min.y, b.max.y],
        )
        .unwrap();
    }
    let before = Store::open_reader(&path).err();
    assert!(matches!(before, Some(vibenote_storage::StorageError::Version(2))));
    let st = Store::open(&path).unwrap();
    assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(st.integrity_check().unwrap(), "ok");
    let area = Rect {
        min: Point::new(90.0, 90.0),
        max: Point::new(110.0, 110.0),
    };
    assert_eq!(
        ids(&st.load_area(NoteId(2), &area).unwrap()),
        vec![4, 5],
        "stroke by box, text always"
    );
    assert_eq!(
        ids(&st.load_area(NoteId(3), &area).unwrap()),
        vec![6],
        "only the asked note"
    );
    let away = Rect {
        min: Point::new(-900.0, -900.0),
        max: Point::new(-800.0, -800.0),
    };
    assert_eq!(ids(&st.load_area(NoteId(2), &away).unwrap()), vec![5]);
    assert_eq!(st.max_z(NoteId(2)).unwrap(), Some(9));
    let ws = st.load().unwrap();
    assert_eq!(ws.next_id, 7);
    assert_eq!(
        ws.canvases[&NoteId(2)].objects[&ObjectId(4)].kind,
        ObjectKind::Stroke(s)
    );
    drop(st);
    let st = Store::open(&path).unwrap();
    assert_eq!(st.load().unwrap(), ws, "reopening a migrated file is a no-op");
}
