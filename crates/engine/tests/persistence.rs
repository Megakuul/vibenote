#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Engine + storage: every command result survives reopening (incremental save = identity).

use proptest::prelude::*;
use serde_json::json;
use vibenote_engine::Engine;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn reopen_equals_in_memory_state(ops in prop::collection::vec((0u8..7, 0usize..5, -5e5f64..5e5, 0f32..=1.0), 1..40)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        let mut e = Engine::open(&path).unwrap();
        for (k, i, x, p) in ops {
            let notes: Vec<u64> = e.session.ws.notes.keys().map(|n| n.0).collect();
            let folders: Vec<u64> = e.session.ws.folders.keys().map(|f| f.0).collect();
            let pick = |v: &Vec<u64>| v.get(i % v.len().max(1)).copied().unwrap_or(9_999);
            let _ = match k {
                0 => e.execute("workspace.folder.create", json!({"name": format!("f{i}")})),
                1 => e.execute("workspace.note.create", json!({"folder": folders.get(i % folders.len().max(1)), "name": "n"})),
                2 => e.execute("ink.stroke.add", json!({"note": pick(&notes), "samples": [{"x": x, "y": -x, "p": p}, {"x": x + 3.3, "y": 1.0, "p": p, "t_us": 9000}]})),
                3 => e.execute("edit.undo", json!({"scope": pick(&notes)})),
                4 => e.execute("edit.undo", json!({"scope": "workspace"})),
                5 => e.execute("workspace.note.delete", json!({"id": pick(&notes)})),
                _ => e.execute("edit.redo", json!({"scope": "workspace"})),
            };
        }
        let mem = e.session.ws.clone();
        drop(e);
        let mut back = Engine::open(&path).unwrap();
        back.load_all().unwrap();
        prop_assert_eq!(&back.session.ws, &mem);
    }
}
