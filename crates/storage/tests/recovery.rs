#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
//! Recovery file format: exact round trip; torn, truncated or damaged files are errors.

use proptest::prelude::*;
use vibenote_model::*;
use vibenote_storage::recovery::{decode_recovery, encode_recovery};
use vibenote_storage::{HistoryOp, Recovery, Stack, StepRecord, read_recovery, write_recovery};

fn sample() -> Recovery {
    let stroke = Stroke {
        tool: Tool::Pen,
        color: Color::INK,
        width: 2.0,
        samples: (0..5)
            .map(|k| {
                Sample {
                    x: f64::from(k) * 1.5,
                    y: -f64::from(k),
                    pressure: 0.5,
                    tilt_x: 3.0,
                    tilt_y: -2.0,
                    t_us: u64::from(k as u8) * 1000,
                }
                .quantised()
            })
            .collect(),
    };
    let obj = CanvasObject {
        id: ObjectId(9),
        z: 4,
        kind: ObjectKind::Stroke(stroke),
    };
    let ins = Op::InsertObject {
        note: NoteId(2),
        object: obj.clone(),
    };
    let rem = Op::RemoveObject {
        note: NoteId(2),
        object: obj,
    };
    Recovery {
        ops: vec![ins.clone(), rem.clone()],
        hist: vec![
            HistoryOp::Push {
                scope: "note:2".into(),
                stack: Stack::Undo,
                step: StepRecord {
                    label: "stroke ü".into(),
                    ops: vec![rem],
                },
            },
            HistoryOp::Pop {
                scope: "note:2".into(),
                stack: Stack::Undo,
            },
            HistoryOp::Clear {
                scope: "workspace".into(),
                stack: Stack::Redo,
            },
            HistoryOp::DropOldest {
                scope: "note:2".into(),
                stack: Stack::Undo,
            },
        ],
        next_id: 10,
        settings: Some(Settings {
            block_idle_ms: 700,
            ..Settings::default()
        }),
    }
}

#[test]
fn round_trip_through_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ws.sqlite.recovery");
    assert_eq!(read_recovery(&p).unwrap(), None);
    let r = sample();
    write_recovery(&p, &r).unwrap();
    assert_eq!(read_recovery(&p).unwrap(), Some(r));
    let empty = Recovery::default();
    write_recovery(&p, &empty).unwrap();
    assert_eq!(read_recovery(&p).unwrap(), Some(empty));
}

#[test]
fn every_truncation_is_rejected() {
    let b = encode_recovery(&sample()).unwrap();
    for n in 0..b.len() {
        assert!(decode_recovery(&b[..n]).is_err(), "prefix {n}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn damage_is_detected(pos in 0usize..10_000, bit in 0u8..8) {
        let mut b = encode_recovery(&sample()).unwrap();
        let i = pos % b.len();
        b[i] ^= 1 << bit;
        prop_assert!(decode_recovery(&b).is_err());
    }

    #[test]
    fn hostile_bytes_never_panic(b in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode_recovery(&b);
    }
}
