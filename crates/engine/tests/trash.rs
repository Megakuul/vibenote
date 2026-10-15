#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Trash (M5.1, R-ORG-004): delete → trash (undoable, canvas kept), restore, empty (permanent).

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::{Engine, LoadState};
use vibenote_model::{FolderId, NoteId};

fn id(v: &Value) -> u64 {
    v["id"].as_u64().unwrap()
}

fn stroke(e: &mut Engine, n: u64, x: f64) {
    e.execute(
        "ink.stroke.add",
        json!({"note": n, "samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 3.0, "y": 1.0, "p": 0.5}]}),
    )
    .unwrap();
}

#[test]
fn delete_goes_to_the_trash_and_restore_brings_it_back() {
    let mut e = Engine::new().unwrap();
    let n = id(&e.execute("workspace.note.create", json!({"name": "x"})).unwrap());
    stroke(&mut e, n, 0.0);
    e.execute("note.open", json!({"id": n})).unwrap();
    let before = e.session.ws.clone();
    e.execute("workspace.note.delete", json!({"id": n})).unwrap();
    assert_eq!(e.session.current_note, None);
    assert!(e.session.ws.trashed_notes.contains(&NoteId(n)));
    assert_eq!(
        e.session.ws.canvases[&NoteId(n)],
        before.canvases[&NoteId(n)],
        "canvas kept"
    );
    let list = e.execute("trash.list", json!({})).unwrap();
    assert_eq!(list["notes"][0]["name"], json!("x"));
    assert!(
        e.execute("workspace.note.delete", json!({"id": n})).is_err(),
        "already in the trash"
    );
    e.execute("trash.restore", json!({"id": n})).unwrap();
    assert_eq!(e.session.ws, before);
    assert!(
        e.execute("trash.restore", json!({"id": n})).is_err(),
        "not in the trash"
    );
    // Both steps are undoable.
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert!(e.session.ws.trashed_notes.contains(&NoteId(n)));
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert_eq!(e.session.ws, before);
}

#[test]
fn folders_go_to_the_trash_only_with_their_content() {
    let mut e = Engine::new().unwrap();
    let a = id(&e.execute("workspace.folder.create", json!({"name": "a"})).unwrap());
    let b = id(&e
        .execute("workspace.folder.create", json!({"parent": a, "name": "b"}))
        .unwrap());
    let n = id(&e
        .execute("workspace.note.create", json!({"folder": b, "name": "n"}))
        .unwrap());
    assert!(
        e.execute("workspace.folder.delete", json!({"id": b})).is_err(),
        "live note inside"
    );
    e.execute("workspace.note.delete", json!({"id": n})).unwrap();
    e.execute("workspace.folder.delete", json!({"id": b})).unwrap();
    e.execute("workspace.folder.delete", json!({"id": a})).unwrap();
    // Nothing new goes into a trashed folder.
    for (cmd, p) in [
        ("workspace.note.create", json!({"folder": b, "name": "z"})),
        ("workspace.folder.create", json!({"parent": a, "name": "z"})),
    ] {
        assert!(e.execute(cmd, p).is_err(), "{cmd}");
    }
    let other = id(&e.execute("workspace.note.create", json!({"name": "o"})).unwrap());
    assert!(
        e.execute("workspace.note.move", json!({"id": other, "folder": a}))
            .is_err()
    );
    // Restoring the note restores its folder chain, outermost first.
    e.execute("trash.restore", json!({"id": n})).unwrap();
    assert!(e.session.ws.trashed_notes.is_empty());
    assert!(e.session.ws.trashed_folders.is_empty());
    assert_eq!(e.session.ws.folder(FolderId(b)).unwrap().parent, Some(FolderId(a)));
}

#[test]
fn empty_deletes_permanently_and_clears_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let f = id(&e.execute("workspace.folder.create", json!({"name": "f"})).unwrap());
    let n = id(&e
        .execute("workspace.note.create", json!({"folder": f, "name": "n"}))
        .unwrap());
    let keep = id(&e.execute("workspace.note.create", json!({"name": "keep"})).unwrap());
    for k in 0..3000 {
        stroke(&mut e, n, f64::from(k));
    }
    stroke(&mut e, keep, 1.0);
    e.execute("workspace.note.delete", json!({"id": n})).unwrap();
    e.execute("workspace.folder.delete", json!({"id": f})).unwrap();
    drop(e);
    // A lazily opened workspace empties the trash without reading the trashed canvas.
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(e.session.load_state(NoteId(n)), LoadState::Unloaded);
    let r = e.execute("trash.empty", json!({})).unwrap();
    assert_eq!(r, json!({"notes": 1, "folders": 1}));
    assert!(e.session.ws.note(NoteId(n)).is_err());
    assert!(e.session.ws.folder(FolderId(f)).is_err());
    assert!(
        e.execute("edit.undo", json!({"scope": "workspace"})).is_err(),
        "history cleared"
    );
    assert!(e.session.history_of(&format!("note:{n}")).undo.is_empty());
    assert_eq!(
        e.execute("trash.list", json!({})).unwrap(),
        json!({"notes": [], "folders": []})
    );
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    e.load_all().unwrap();
    assert_eq!(e.session.ws.notes.len(), 1);
    assert_eq!(e.session.ws.canvases[&NoteId(keep)].objects.len(), 1);
    assert!(e.session.ws.trashed_notes.is_empty() && e.session.ws.trashed_folders.is_empty());
    assert!(e.session.history_of("workspace").undo.is_empty());
    assert!(e.session.history_of(&format!("note:{n}")).undo.is_empty());
    assert_eq!(
        e.session.history_of(&format!("note:{keep}")).undo.len(),
        1,
        "other notes keep theirs"
    );
    let st = vibenote_storage::Store::open(&path).unwrap();
    assert_eq!(st.load().unwrap().canvases.len(), 1, "objects gone from disk");
}

type Cmd = (u8, usize, f64);

fn apply(e: &mut Engine, (k, i, x): Cmd) -> Result<Value, String> {
    let notes: Vec<u64> = e.session.ws.notes.keys().map(|n| n.0).collect();
    let folders: Vec<u64> = e.session.ws.folders.keys().map(|f| f.0).collect();
    let pick = |v: &[u64]| v.get(i % v.len().max(1)).copied().unwrap_or(9_999);
    let any = |v: &[u64], w: &[u64]| if i % 2 == 0 { pick(v) } else { pick(w) };
    let r = match k {
        0 => e.execute("workspace.folder.create", json!({"name": "f", "parent": (i % 3 == 0).then(|| pick(&folders))})),
        1 => e.execute("workspace.note.create", json!({"name": "n", "folder": (i % 3 == 0).then(|| pick(&folders))})),
        2 => e.execute(
            "ink.stroke.add",
            json!({"note": pick(&notes), "samples": [{"x": x, "y": 0.0, "p": 0.5}, {"x": x + 2.0, "y": 1.0, "p": 0.5}]}),
        ),
        3 => e.execute("workspace.note.delete", json!({"id": pick(&notes)})),
        4 => e.execute("workspace.folder.delete", json!({"id": pick(&folders)})),
        5 => e.execute("trash.restore", json!({"id": any(&notes, &folders)})),
        6 => e.execute("edit.undo", json!({"scope": "workspace"})),
        7 => e.execute("edit.redo", json!({"scope": "workspace"})),
        8 => e.execute("workspace.note.move", json!({"id": pick(&notes), "folder": pick(&folders)})),
        9 => e.execute("edit.undo", json!({"scope": pick(&notes)})),
        _ => e.execute("trash.empty", json!({})),
    };
    r.map_err(|e| e.to_string())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    /// Trash commands on a lazily reopened workspace behave exactly like one in-memory session,
    /// and the trash invariant holds: nothing live inside a trashed folder.
    #[test]
    fn trash_on_disk_matches_memory(a in prop::collection::vec((0u8..11, 0usize..6, -1e3f64..1e3), 0..40),
                                    b in prop::collection::vec((0u8..11, 0usize..6, -1e3f64..1e3), 0..40)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut mem = Engine::new().unwrap();
        let mut disk = Engine::open(&path).unwrap();
        for c in &a {
            prop_assert_eq!(apply(&mut disk, *c), apply(&mut mem, *c));
        }
        drop(disk);
        let mut disk = Engine::open(&path).unwrap();
        for c in &b {
            prop_assert_eq!(apply(&mut disk, *c), apply(&mut mem, *c), "{:?}", c);
        }
        disk.load_all().unwrap();
        prop_assert_eq!(&disk.session.ws, &mem.session.ws);
        let keys: Vec<String> = ["workspace".to_string()].into_iter().chain(mem.session.ws.notes.keys().map(|n| format!("note:{n}"))).collect();
        for k in keys {
            let want = mem.session.history_of(&k);
            prop_assert_eq!(disk.session.history_of(&k), want, "{}", k);
        }
        let ws = &mem.session.ws;
        for n in ws.notes.values() {
            if let Some(f) = n.folder {
                prop_assert!(!ws.folder_in_trash(f) || ws.trashed_notes.contains(&n.id), "live note {} in trashed folder", n.id);
            }
        }
        for f in ws.folders.values() {
            if let Some(p) = f.parent {
                prop_assert!(!ws.folder_in_trash(p) || ws.trashed_folders.contains(&f.id), "live folder {} in trashed folder", f.id);
            }
        }
    }
}
