#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! Settings storage (M2.4): stored in the batch transaction, read back exactly.

use vibenote_model::settings::{EraserMode, ToolKind};
use vibenote_model::*;
use vibenote_storage::Store;

#[test]
fn settings_round_trip_and_commit_with_their_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    let mut st = Store::open(&path).unwrap();
    assert_eq!(st.load_settings().unwrap(), None);
    let a = Settings {
        tool: ToolKind::Text,
        eraser_mode: EraserMode::Area,
        prediction_ms: 4.25,
        shape: ShapeKind::Ellipse,
        ..Settings::default()
    };
    st.apply_batch(&[], &[], 1, Some(&a)).unwrap();
    assert_eq!(st.load_settings().unwrap(), Some(a));
    // A batch whose op fails stores none of it, settings included.
    let b = Settings {
        block_idle_ms: 900,
        ..a
    };
    let missing = Op::DeleteFolder {
        folder: Folder {
            id: FolderId(7),
            parent: None,
            name: "x".into(),
            color: None,
            sort: 0,
        },
    };
    let bad = Op::InsertObject {
        note: NoteId(1),
        object: CanvasObject {
            id: ObjectId(1),
            z: 0,
            kind: ObjectKind::Text {
                origin: vibenote_geom::Point::new(0.0, 0.0),
                width: 1.0,
                content: String::new(),
                marks: vec![],
                size: 16.0,
            },
        },
    };
    // Same object id twice violates the primary key.
    assert!(st.apply_batch(&[missing, bad.clone(), bad], &[], 2, Some(&b)).is_err());
    assert_eq!(st.load_settings().unwrap(), Some(a));
    drop(st);
    assert_eq!(Store::open(&path).unwrap().load_settings().unwrap(), Some(a));
}
