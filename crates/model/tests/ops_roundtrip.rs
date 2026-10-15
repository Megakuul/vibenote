#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Integration tests may unwrap/index/panic (AGENTS.md §3 applies to non-test code).
//! Property: any sequence of ops (valid or not) followed by applying the returned inverses in
//! reverse order restores the exact initial workspace; invalid ops leave the state unchanged.

use proptest::prelude::*;
use vibenote_model::*;

#[derive(Debug, Clone)]
enum Intent {
    NewFolder { parent: Option<usize>, name: String },
    NewNote { folder: Option<usize>, name: String },
    MoveFolder { f: usize, to: Option<usize> },
    RenameNote { n: usize, name: String },
    DeleteFolder { f: usize },
    DeleteNote { n: usize },
    AddStroke { n: usize, pts: Vec<(f64, f64, f32)> },
    RemoveObject { n: usize, o: usize },
    BadStroke { n: usize },
}

fn intent() -> impl Strategy<Value = Intent> {
    prop_oneof![
        (proptest::option::of(0usize..8), "[a-zäöü ]{0,12}")
            .prop_map(|(parent, name)| Intent::NewFolder { parent, name }),
        (proptest::option::of(0usize..8), "[a-z]{0,12}").prop_map(|(folder, name)| Intent::NewNote { folder, name }),
        (0usize..8, proptest::option::of(0usize..8)).prop_map(|(f, to)| Intent::MoveFolder { f, to }),
        (0usize..8, ".{0,20}").prop_map(|(n, name)| Intent::RenameNote { n, name }),
        (0usize..8).prop_map(|f| Intent::DeleteFolder { f }),
        (0usize..8).prop_map(|n| Intent::DeleteNote { n }),
        (
            0usize..8,
            prop::collection::vec((-1e7f64..1e7, -1e7f64..1e7, 0f32..=1.0), 1..20)
        )
            .prop_map(|(n, pts)| Intent::AddStroke { n, pts }),
        (0usize..8, 0usize..8).prop_map(|(n, o)| Intent::RemoveObject { n, o }),
        (0usize..8).prop_map(|n| Intent::BadStroke { n }),
    ]
}

fn nth<K: Copy, V>(m: &std::collections::BTreeMap<K, V>, i: usize) -> Option<K> {
    m.keys().copied().nth(i)
}

fn to_op(ws: &mut Workspace, it: &Intent) -> Option<Op> {
    Some(match it.clone() {
        Intent::NewFolder { parent, name } => Op::CreateFolder {
            folder: Folder {
                id: FolderId(ws.alloc_id().ok()?),
                parent: parent.and_then(|i| nth(&ws.folders, i)),
                name,
                color: None,
                sort: 0,
            },
        },
        Intent::NewNote { folder, name } => Op::CreateNote {
            note: Note {
                id: NoteId(ws.alloc_id().ok()?),
                folder: folder.and_then(|i| nth(&ws.folders, i)),
                name,
                color: None,
                sort: 0,
                background: None,
            },
            canvas: Canvas::default(),
        },
        Intent::MoveFolder { f, to } => {
            let mut after = ws.folders.get(&nth(&ws.folders, f)?)?.clone();
            after.parent = to.and_then(|i| nth(&ws.folders, i));
            Op::UpdateFolder { after }
        }
        Intent::RenameNote { n, name } => {
            let mut after = ws.notes.get(&nth(&ws.notes, n)?)?.clone();
            after.name = name;
            Op::UpdateNote { after }
        }
        Intent::DeleteFolder { f } => Op::DeleteFolder {
            folder: ws.folders.get(&nth(&ws.folders, f)?)?.clone(),
        },
        Intent::DeleteNote { n } => Op::DeleteNote {
            note: ws.notes.get(&nth(&ws.notes, n)?)?.clone(),
        },
        Intent::AddStroke { n, pts } => {
            let note = nth(&ws.notes, n)?;
            let samples = pts
                .iter()
                .enumerate()
                .map(|(i, &(x, y, p))| Sample {
                    x,
                    y,
                    pressure: p,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: i as u64 * 5000,
                })
                .collect();
            Op::InsertObject {
                note,
                object: CanvasObject {
                    id: ObjectId(ws.alloc_id().ok()?),
                    z: 0,
                    kind: ObjectKind::Stroke(Stroke {
                        tool: Tool::Pen,
                        color: Color::INK,
                        width: 2.0,
                        samples,
                    }),
                },
            }
        }
        Intent::RemoveObject { n, o } => {
            let note = nth(&ws.notes, n)?;
            let c = ws.canvases.get(&note)?;
            Op::RemoveObject {
                note,
                object: c.objects.values().nth(o)?.clone(),
            }
        }
        Intent::BadStroke { n } => {
            let note = nth(&ws.notes, n)?;
            let s = Sample {
                x: f64::NAN,
                y: 0.0,
                pressure: 2.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
                t_us: 0,
            };
            Op::InsertObject {
                note,
                object: CanvasObject {
                    id: ObjectId(ws.alloc_id().ok()?),
                    z: 0,
                    kind: ObjectKind::Stroke(Stroke {
                        tool: Tool::Pen,
                        color: Color::INK,
                        width: 2.0,
                        samples: vec![s],
                    }),
                },
            }
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn apply_then_undo_restores_exact_state(intents in prop::collection::vec(intent(), 1..60)) {
        let mut ws = Workspace::new();
        let initial = ws.clone();
        let mut undo: Vec<Op> = vec![];
        for it in &intents {
            let Some(op) = to_op(&mut ws, it) else { continue };
            let before = { let mut b = ws.clone(); b.next_id = 0; b };
            match ws.apply(op) {
                Ok(inv) => undo.push(inv),
                Err(_) => {
                    let mut after = ws.clone(); after.next_id = 0;
                    prop_assert_eq!(after, before, "failed op must not change state");
                }
            }
        }
        for inv in undo.into_iter().rev() {
            prop_assert!(ws.apply(inv).is_ok());
        }
        // next_id only grows (ids are never reused); everything else must be identical.
        ws.next_id = initial.next_id;
        prop_assert_eq!(ws, initial);
    }

    #[test]
    fn serde_roundtrip_is_identity(intents in prop::collection::vec(intent(), 1..40)) {
        let mut ws = Workspace::new();
        for it in &intents {
            if let Some(op) = to_op(&mut ws, it) { let _ = ws.apply(op); }
        }
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, ws);
    }
}

#[test]
fn folder_cycle_is_rejected() {
    let mut ws = Workspace::new();
    let a = Folder {
        id: FolderId(1),
        parent: None,
        name: "a".into(),
        color: None,
        sort: 0,
    };
    let b = Folder {
        id: FolderId(2),
        parent: Some(FolderId(1)),
        name: "b".into(),
        color: None,
        sort: 0,
    };
    ws.apply(Op::CreateFolder { folder: a.clone() }).unwrap();
    ws.apply(Op::CreateFolder { folder: b }).unwrap();
    let r = ws.apply(Op::UpdateFolder {
        after: Folder {
            parent: Some(FolderId(2)),
            ..a
        },
    });
    assert!(matches!(r, Err(ModelError::FolderCycle(..))), "{r:?}");
}

#[test]
fn deep_folder_chain_is_fine() {
    // R-ORG-001: depth 1,000 without recursion problems.
    let mut ws = Workspace::new();
    let mut parent = None;
    for i in 1..=1000u64 {
        ws.apply(Op::CreateFolder {
            folder: Folder {
                id: FolderId(i),
                parent,
                name: format!("f{i}"),
                color: None,
                sort: 0,
            },
        })
        .unwrap();
        parent = Some(FolderId(i));
    }
    assert!(ws.is_ancestor_or_self(FolderId(1), FolderId(1000)));
}
