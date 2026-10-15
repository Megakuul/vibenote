#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
//! Schema v2 (M2.2, R-UNDO-001 / R-DATA-003): undo/redo log records, history writes in the op
//! transaction, migration from v1.

use proptest::prelude::*;
use vibenote_geom::Point;
use vibenote_model::*;
use vibenote_storage::history::{decode_ops, encode_ops};
use vibenote_storage::{HistoryOp, SCHEMA_VERSION, Stack, StepRecord, Store};

fn stroke(n: usize, x0: f64) -> Stroke {
    Stroke {
        tool: Tool::Pen,
        color: Color::INK,
        width: 2.0,
        samples: (0..n)
            .map(|k| {
                Sample {
                    x: x0 + k as f64 * 0.7,
                    y: -(k as f64) * 1.3,
                    pressure: 0.3 + (k % 7) as f32 * 0.1,
                    tilt_x: 12.5,
                    tilt_y: -3.0,
                    t_us: k as u64 * 5000,
                }
                .quantised()
            })
            .collect(),
    }
}

fn note(id: u64) -> Note {
    Note {
        id: NoteId(id),
        folder: Some(FolderId(1)),
        name: "Vorlesung ü".into(),
        color: Some(Color(0x5a8cd2ff)),
        sort: -7,
        background: None,
    }
}

fn all_kinds() -> Vec<Op> {
    let folder = Folder {
        id: FolderId(1),
        parent: None,
        name: "A".into(),
        color: None,
        sort: 3,
    };
    let s = CanvasObject {
        id: ObjectId(10),
        z: -2,
        kind: ObjectKind::Stroke(stroke(40, 5.0)),
    };
    let t = CanvasObject {
        id: ObjectId(11),
        z: 4,
        kind: ObjectKind::Text {
            origin: Point { x: 1.5, y: -2.25 },
            width: 300.0,
            content: "Hallo \"Welt\"".into(),
            marks: vec![],
            size: 16.0,
        },
    };
    let mut canvas = Canvas::default();
    canvas.objects.insert(s.id, s.clone());
    canvas.objects.insert(t.id, t.clone());
    vec![
        Op::CreateFolder { folder: folder.clone() },
        Op::UpdateFolder { after: folder.clone() },
        Op::DeleteFolder { folder },
        Op::CreateNote { note: note(2), canvas },
        Op::UpdateNote { after: note(2) },
        Op::DeleteNote { note: note(2) },
        Op::InsertObject {
            note: NoteId(2),
            object: s.clone(),
        },
        Op::RemoveObject {
            note: NoteId(2),
            object: t,
        },
        Op::ReplaceObject {
            note: NoteId(2),
            after: s,
        },
        Op::TrashNote { note: NoteId(2) },
        Op::RestoreNote { note: NoteId(u64::MAX) },
        Op::TrashFolder { folder: FolderId(1) },
        Op::RestoreFolder { folder: FolderId(0) },
        Op::SetNoteBackground {
            note: NoteId(2),
            background: Some(vibenote_model::settings::Background {
                kind: vibenote_model::settings::BackgroundKind::Dots,
                spacing: 17.25,
            }),
        },
        Op::SetNoteBackground {
            note: NoteId(2),
            background: None,
        },
    ]
}

#[test]
fn step_records_round_trip_every_op_kind() {
    let ops = all_kinds();
    assert_eq!(decode_ops(&encode_ops(&ops).unwrap()).unwrap(), ops);
    assert_eq!(decode_ops(&encode_ops(&[]).unwrap()).unwrap(), vec![]);
}

#[test]
fn step_records_are_compact() {
    // A 300-sample stroke step costs what the object table stores (stroke codec), not JSON.
    let ops = vec![Op::RemoveObject {
        note: NoteId(2),
        object: CanvasObject {
            id: ObjectId(9),
            z: 0,
            kind: ObjectKind::Stroke(stroke(300, 0.0)),
        },
    }];
    let n = encode_ops(&ops).unwrap().len();
    let json = serde_json::to_vec(&ops).unwrap().len();
    assert!(n * 5 < json, "{n} bytes vs {json} bytes as JSON");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn hostile_step_records_never_panic(b in prop::collection::vec(any::<u8>(), 0..512), cut in 0usize..2000) {
        let _ = decode_ops(&b);
        let mut with_magic = b"O1".to_vec();
        with_magic.extend_from_slice(&b);
        let _ = decode_ops(&with_magic);
        // Truncations and bit flips of a valid record are errors or valid ops, never panics.
        let good = encode_ops(&all_kinds()).unwrap();
        let _ = decode_ops(&good[..cut.min(good.len())]);
        let mut flipped = good.clone();
        let i = cut % flipped.len();
        flipped[i] ^= b.first().copied().unwrap_or(0x5a);
        let _ = decode_ops(&flipped);
    }
}

fn step(label: &str, x: f64) -> StepRecord {
    StepRecord {
        label: label.into(),
        ops: vec![Op::RemoveObject {
            note: NoteId(2),
            object: CanvasObject {
                id: ObjectId(x as u64),
                z: 0,
                kind: ObjectKind::Stroke(stroke(3, x)),
            },
        }],
    }
}

#[test]
fn history_ops_apply_in_order_per_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    let sc = |s: &str| s.to_string();
    let push = |scope: &str, stack, s| HistoryOp::Push {
        scope: sc(scope),
        stack,
        step: s,
    };
    st.apply(
        &[],
        &[
            push("note:2", Stack::Undo, step("a", 1.0)),
            push("note:2", Stack::Undo, step("b", 2.0)),
            push("note:2", Stack::Undo, step("c", 3.0)),
            push("workspace", Stack::Undo, step("w", 4.0)),
            HistoryOp::Pop {
                scope: sc("note:2"),
                stack: Stack::Undo,
            },
            push("note:2", Stack::Redo, step("c'", 3.0)),
            HistoryOp::DropOldest {
                scope: sc("note:2"),
                stack: Stack::Undo,
            },
        ],
        1,
    )
    .unwrap();
    let h = Store::open(&path).unwrap().load_history("note:2").unwrap();
    assert_eq!(h.undo, vec![step("b", 2.0)]);
    assert_eq!(h.redo, vec![step("c'", 3.0)]);
    st.apply(
        &[],
        &[
            HistoryOp::Clear {
                scope: sc("note:2"),
                stack: Stack::Redo,
            },
            push("note:2", Stack::Undo, step("d", 5.0)),
        ],
        1,
    )
    .unwrap();
    let r = Store::open_reader(&path).unwrap();
    let h = r.load_history("note:2").unwrap();
    assert_eq!(h.undo, vec![step("b", 2.0), step("d", 5.0)]);
    assert!(h.redo.is_empty());
    assert_eq!(r.load_history("workspace").unwrap().undo, vec![step("w", 4.0)]);
    assert_eq!(r.load_history("note:99").unwrap(), Default::default());
}

/// A failing transaction leaves neither the ops nor the history change behind.
#[test]
fn history_and_ops_commit_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TRIGGER no BEFORE INSERT ON note BEGIN SELECT RAISE(ABORT, 'no'); END;")
        .unwrap();
    let r = st.apply(
        &[Op::CreateNote {
            note: note(2),
            canvas: Canvas::default(),
        }],
        &[HistoryOp::Push {
            scope: "workspace".into(),
            stack: Stack::Undo,
            step: step("x", 1.0),
        }],
        3,
    );
    assert!(r.is_err());
    assert!(st.load_history("workspace").unwrap().undo.is_empty());
}

/// The exact v1 schema as shipped on 2026-10-12 (frozen copy; do not edit).
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

#[test]
fn migrates_a_v1_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite");
    let s = stroke(25, 3.0);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(V1).unwrap();
        c.execute_batch(
            "INSERT INTO folder VALUES(1, NULL, 'A', NULL, 0);
             INSERT INTO note VALUES(2, 1, 'n', 4294901760, 5);
             UPDATE meta SET value = '4' WHERE key = 'next_id';",
        )
        .unwrap();
        c.execute(
            "INSERT INTO object VALUES(3, 2, 0, 'stroke', ?1)",
            [vibenote_storage::encode_stroke(&s).unwrap()],
        )
        .unwrap();
    }
    assert!(matches!(
        Store::open_reader(&path),
        Err(vibenote_storage::StorageError::Version(1))
    ));
    let mut st = Store::open(&path).unwrap();
    assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(st.integrity_check().unwrap(), "ok");
    let ws = st.load().unwrap();
    assert_eq!(ws.next_id, 4);
    assert_eq!(ws.notes[&NoteId(2)].name, "n");
    assert_eq!(
        ws.canvases[&NoteId(2)].objects[&ObjectId(3)].kind,
        ObjectKind::Stroke(s)
    );
    assert!(st.load_history("note:2").unwrap().undo.is_empty());
    st.apply(
        &[],
        &[HistoryOp::Push {
            scope: "note:2".into(),
            stack: Stack::Undo,
            step: step("after migration", 1.0),
        }],
        4,
    )
    .unwrap();
    drop(st);
    // Reopening a migrated file is a no-op.
    let st = Store::open(&path).unwrap();
    assert_eq!(st.load_history("note:2").unwrap().undo.len(), 1);
    assert_eq!(st.load().unwrap(), ws);
}

/// The table/index definitions of a database (sorted), for schema comparisons.
fn schema_of(path: &std::path::Path) -> Vec<(String, String)> {
    let c = rusqlite::Connection::open(path).unwrap();
    let mut q = c
        .prepare("SELECT name, coalesce(sql, '') FROM sqlite_master ORDER BY name")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_new_workspace_has_the_migrated_schema() {
    // New databases are created in one transaction (one fsync); the result must equal a v1
    // database migrated step by step.
    let dir = tempfile::tempdir().unwrap();
    let (fresh, old) = (dir.path().join("new.sqlite"), dir.path().join("v1.sqlite"));
    Store::open(&fresh).unwrap();
    {
        let c = rusqlite::Connection::open(&old).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(V1).unwrap();
    }
    let st = Store::open(&old).unwrap();
    assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
    drop(st);
    let st = Store::open(&fresh).unwrap();
    assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(st.integrity_check().unwrap(), "ok");
    drop(st);
    assert_eq!(schema_of(&fresh), schema_of(&old));
}
