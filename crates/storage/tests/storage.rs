#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
//! Storage properties: lossless codec, save→load identity, hostile blobs never panic.

use proptest::prelude::*;
use vibenote_model::*;
use vibenote_storage::{Store, decode_stroke, encode_stroke};

fn arb_stroke() -> impl Strategy<Value = Stroke> {
    (
        prop::collection::vec(
            (
                -1e7f64..1e7,
                -1e7f64..1e7,
                0f32..=1.0,
                -90f32..=90.0,
                -90f32..=90.0,
                0u64..100_000,
            ),
            1..200,
        ),
        prop_oneof![Just(Tool::Pen), Just(Tool::Highlighter)],
        any::<u32>(),
        0.1f32..50.0,
    )
        .prop_map(|(pts, tool, color, width)| {
            let mut t = 0u64;
            let samples = pts
                .into_iter()
                .map(|(x, y, p, tx, ty, dt)| {
                    t += dt;
                    Sample {
                        x,
                        y,
                        pressure: p,
                        tilt_x: tx,
                        tilt_y: ty,
                        t_us: t,
                    }
                    .quantised()
                })
                .collect();
            Stroke {
                tool,
                color: Color(color),
                width,
                samples,
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn codec_is_lossless_on_quantised_strokes(s in arb_stroke()) {
        let b = encode_stroke(&s).unwrap();
        prop_assert_eq!(decode_stroke(&b).unwrap(), s);
    }

    #[test]
    fn quantisation_is_idempotent(x in -1e9f64..1e9, p in 0f32..=1.0, t in -90f32..=90.0) {
        let s = Sample { x, y: -x, pressure: p, tilt_x: t, tilt_y: -t, t_us: 7 }.quantised();
        prop_assert_eq!(s.quantised(), s);
    }

    #[test]
    fn hostile_blobs_never_panic(b in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode_stroke(&b);
        let mut with_magic = b"S1".to_vec();
        with_magic.extend_from_slice(&b);
        let _ = decode_stroke(&with_magic);
    }

    #[test]
    fn save_all_then_load_is_identity(strokes in prop::collection::vec(arb_stroke(), 0..6), names in prop::collection::vec(".{0,12}", 1..4)) {
        let mut ws = Workspace::new();
        let mut parent = None;
        for n in &names {
            let id = FolderId(ws.alloc_id().unwrap());
            ws.apply(Op::CreateFolder { folder: Folder { id, parent, name: n.clone(), color: Some(Color(0xff0000ff)), sort: 5 } }).unwrap();
            parent = Some(id);
        }
        let note = NoteId(ws.alloc_id().unwrap());
        ws.apply(Op::CreateNote { note: Note { id: note, folder: parent, name: "n".into(), color: None, sort: -3, background: None }, canvas: Canvas::default() }).unwrap();
        for (k, s) in strokes.into_iter().enumerate() {
            let id = ObjectId(ws.alloc_id().unwrap());
            ws.apply(Op::InsertObject { note, object: CanvasObject { id, z: k as i64, kind: ObjectKind::Stroke(s) } }).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.sqlite");
        Store::open(&path).unwrap().save_all(&ws).unwrap();
        let back = Store::open(&path).unwrap().load().unwrap();
        prop_assert_eq!(back, ws);
    }
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.sqlite");
    drop(Store::open(&path).unwrap());
    let c = rusqlite_conn(&path);
    c.execute_batch("PRAGMA user_version = 99;").unwrap();
    drop(c);
    assert!(Store::open(&path).is_err());
}

fn rusqlite_conn(p: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(p).unwrap()
}

/// Durability configuration (ADR 0005): WAL + synchronous=FULL. kill -9 tests cannot detect a
/// missing fsync (only power loss can), so the configuration itself is asserted (found by
/// harness self-validation).
#[test]
fn durability_is_wal_and_full_sync() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ws.sqlite")).unwrap();
    assert_eq!(s.durability().unwrap(), ("wal".to_string(), 2));
}

/// Regression: un-snapped samples decode to exactly the model's snapped value. Pressure
/// rounded in f64 by the codec used to differ by one 1/65535 step from the model's f32
/// rounding for some inputs, so a reopened workspace differed from memory.
#[test]
fn encode_snaps_like_the_model() {
    let mut mismatches_before_fix = 0;
    let samples: Vec<Sample> = (0..200_000u32)
        .map(|k| {
            let p = k as f32 / 200_000.0 + 1.3e-6;
            if ((f64::from(p) * 65535.0).round() as f32 / 65535.0) != (p * 65535.0).round() / 65535.0 {
                mismatches_before_fix += 1;
            }
            Sample {
                x: f64::from(k) * 0.000_37 - 3.1,
                y: -f64::from(k) * 0.011,
                pressure: p.min(1.0),
                tilt_x: k as f32 * 0.003_3 - 60.0,
                tilt_y: 0.0,
                t_us: u64::from(k) * 1000,
            }
        })
        .collect();
    assert!(
        mismatches_before_fix > 0,
        "test inputs must cover the f32/f64 rounding edge"
    );
    let s = Stroke {
        tool: Tool::Pen,
        color: Color::INK,
        width: 2.0,
        samples,
    };
    let back = decode_stroke(&encode_stroke(&s).unwrap()).unwrap();
    let want: Vec<Sample> = s.samples.iter().map(|x| x.quantised()).collect();
    assert_eq!(back.samples, want);
}
