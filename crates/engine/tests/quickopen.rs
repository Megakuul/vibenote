#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
//! Quick-open (M5.2, R-ORG-003): fuzzy by name, recent notes, trash excluded, index follows
//! renames; multi-note move.

use proptest::prelude::*;
use serde_json::{Value, json};
use vibenote_engine::Engine;

fn note(e: &mut Engine, name: &str, folder: Option<u64>) -> u64 {
    e.execute("workspace.note.create", json!({ "name": name, "folder": folder }))
        .unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn q(e: &mut Engine, text: &str) -> Vec<(u64, String)> {
    let r = e
        .execute("quickopen.query", json!({ "text": text, "limit": 50 }))
        .unwrap();
    r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| (h["id"].as_u64().unwrap(), h["name"].as_str().unwrap().to_string()))
        .collect()
}

fn names(v: &[(u64, String)]) -> Vec<&str> {
    v.iter().map(|(_, n)| n.as_str()).collect()
}

#[test]
fn ranks_prefixes_first_and_follows_renames_and_the_trash() {
    let mut e = Engine::new().unwrap();
    let uni = e.execute("workspace.folder.create", json!({"name": "Uni"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let ana = e
        .execute("workspace.folder.create", json!({"name": "Analysis", "parent": uni}))
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let a = note(&mut e, "Banknoten", None);
    let b = note(&mut e, "Notizen Analysis", Some(ana));
    let c = note(&mut e, "Meine Notizen", None);
    note(&mut e, "Einkauf", None);
    assert_eq!(
        names(&q(&mut e, "not")),
        vec!["Notizen Analysis", "Meine Notizen", "Banknoten"]
    );
    let r = e
        .execute("quickopen.query", json!({"text": "not", "limit": 1}))
        .unwrap();
    assert_eq!(r["results"][0]["path"], json!("Uni/Analysis"));
    assert_eq!(r["results"].as_array().unwrap().len(), 1);
    // Typing on narrows (incremental), deleting characters widens again.
    assert_eq!(names(&q(&mut e, "notiz")), vec!["Notizen Analysis", "Meine Notizen"]);
    assert_eq!(names(&q(&mut e, "notizm")), Vec::<&str>::new());
    assert_eq!(q(&mut e, "not").len(), 3);
    // Renames and the trash show up at once.
    e.execute("workspace.note.rename", json!({"id": a, "name": "Kassenbuch"}))
        .unwrap();
    assert_eq!(names(&q(&mut e, "not")), vec!["Notizen Analysis", "Meine Notizen"]);
    e.execute("workspace.note.delete", json!({"id": c})).unwrap();
    assert_eq!(names(&q(&mut e, "not")), vec!["Notizen Analysis"]);
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert_eq!(q(&mut e, "not").len(), 2);
    let _ = b;
}

#[test]
fn empty_text_lists_recent_notes_and_recency_breaks_ties() {
    let mut e = Engine::new().unwrap();
    let ids: Vec<u64> = (0..4).map(|k| note(&mut e, &format!("Blatt {k}"), None)).collect();
    assert!(q(&mut e, "").is_empty());
    for id in [ids[2], ids[0], ids[2]] {
        e.execute("note.open", json!({ "id": id })).unwrap();
    }
    assert_eq!(
        q(&mut e, "").iter().map(|h| h.0).collect::<Vec<_>>(),
        vec![ids[2], ids[0]]
    );
    let r = e.execute("quickopen.query", json!({"text": "blatt"})).unwrap();
    let got: Vec<u64> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_u64().unwrap())
        .collect();
    assert_eq!(got[..2], [ids[2], ids[0]], "recent first among equal scores");
    assert_eq!(r["results"][0]["recent"], json!(true));
    // A trashed recent note is not offered.
    e.execute("workspace.note.delete", json!({"id": ids[2]})).unwrap();
    assert_eq!(q(&mut e, "").iter().map(|h| h.0).collect::<Vec<_>>(), vec![ids[0]]);
    // Hostile input: huge limits are capped, long text is fine.
    let r = e
        .execute(
            "quickopen.query",
            json!({"text": "x".repeat(100_000), "limit": 1_000_000_000u64}),
        )
        .unwrap();
    assert!(r["results"].as_array().unwrap().is_empty());
}

#[test]
fn several_notes_move_in_one_step_keeping_their_order() {
    let mut e = Engine::new().unwrap();
    let f = e.execute("workspace.folder.create", json!({"name": "Ziel"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let ids: Vec<u64> = (0..4).map(|k| note(&mut e, &format!("n{k}"), None)).collect();
    let before = e.inspect(Some("/ws/notes")).unwrap();
    e.execute(
        "workspace.notes.move",
        json!({"ids": [ids[3], ids[1], ids[1]], "folder": f}),
    )
    .unwrap();
    let notes = e.inspect(Some("/ws/notes")).unwrap();
    let get = |id: u64| notes[id.to_string()].clone();
    assert_eq!(get(ids[1])["folder"], json!(f));
    assert_eq!(get(ids[3])["folder"], json!(f));
    assert!(get(ids[1])["sort"].as_i64().unwrap() < get(ids[3])["sort"].as_i64().unwrap());
    assert_eq!(get(ids[0])["folder"], Value::Null);
    e.execute("edit.undo", json!({"scope": "workspace"})).unwrap();
    assert_eq!(e.inspect(Some("/ws/notes")).unwrap(), before);
    assert!(
        e.execute("workspace.notes.move", json!({"ids": [ids[0], 999_999], "folder": f}))
            .is_err()
    );
    assert!(
        e.execute("workspace.notes.move", json!({"ids": [ids[0]], "folder": 999_999}))
            .is_err()
    );
    assert_eq!(e.inspect(Some("/ws/notes")).unwrap(), before);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// Typing character by character (incremental narrowing) gives the same results as asking
    /// a fresh engine for each prefix.
    #[test]
    fn incremental_queries_match_fresh_ones(
        names in prop::collection::vec("[a-cA-C äÖ]{0,8}", 1..40),
        text in "[a-c äö]{1,5}",
    ) {
        let build = || {
            let mut e = Engine::new().unwrap();
            for n in &names {
                note(&mut e, n, None);
            }
            e
        };
        let mut e = build();
        let chars: Vec<char> = text.chars().collect();
        for k in 1..=chars.len() {
            let prefix: String = chars[..k].iter().collect();
            let inc = q(&mut e, &prefix);
            prop_assert_eq!(&inc, &q(&mut build(), &prefix), "prefix {:?}", prefix);
        }
        // And back down again (deleting characters).
        for k in (1..chars.len()).rev() {
            let prefix: String = chars[..k].iter().collect();
            let inc = q(&mut e, &prefix);
            prop_assert_eq!(&inc, &q(&mut build(), &prefix), "prefix {:?}", prefix);
        }
    }
}

#[test]
fn recent_notes_survive_a_restart_and_bad_stored_values_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut e = Engine::open(&path).unwrap();
    let ids: Vec<u64> = (0..3).map(|k| note(&mut e, &format!("r{k}"), None)).collect();
    for id in [ids[1], ids[2]] {
        e.execute("note.open", json!({ "id": id })).unwrap();
    }
    e.sync().unwrap();
    drop(e);
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(
        q(&mut e, "").iter().map(|h| h.0).collect::<Vec<_>>(),
        vec![ids[2], ids[1]]
    );
    drop(e);
    let c = rusqlite::Connection::open(&path).unwrap();
    for bad in ["not json", "[-1, \"x\"]", "[999999999]"] {
        c.execute("UPDATE meta SET value = ?1 WHERE key = 'recent'", [bad])
            .unwrap();
        let mut e = Engine::open(&path).unwrap();
        assert!(q(&mut e, "").is_empty(), "{bad}");
    }
}
