//! Shared helpers for acceptance tests: engine shortcuts, budgets, reference workload.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
// Test-support crate: panicking helpers are intended (AGENTS.md §3 covers product code).

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use vibenote_engine::{Engine, EngineError};
use vibenote_model::*;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Engine with one open note; returns (engine, note id).
pub fn engine_with_note() -> (Engine, u64) {
    let mut e = Engine::new().unwrap();
    let n = id(&e
        .execute("workspace.note.create", json!({"name": "acceptance"}))
        .unwrap());
    e.execute("note.open", json!({"id": n})).unwrap();
    (e, n)
}

pub fn id(v: &Value) -> u64 {
    v["id"].as_u64().expect("result has an id")
}

/// Executes and panics with a readable message (incl. NotImplemented) on error.
pub fn run(e: &mut Engine, cmd: &str, args: Value) -> Value {
    match e.execute(cmd, args) {
        Ok(v) => v,
        Err(err) => panic!("{cmd}: {err}"),
    }
}

pub fn try_run(e: &mut Engine, cmd: &str, args: Value) -> Result<Value, EngineError> {
    e.execute(cmd, args)
}

/// Adds a straight stroke from (x0,y0) to (x1,y1) with n samples at pressure p.
pub fn line(e: &mut Engine, note: u64, (x0, y0): (f64, f64), (x1, y1): (f64, f64), p: f32) -> u64 {
    let n = 32u32;
    let samples: Vec<Value> = (0..n)
        .map(|i| {
            let t = f64::from(i) / f64::from(n - 1);
            json!({"x": x0 + (x1 - x0) * t, "y": y0 + (y1 - y0) * t, "p": p, "t_us": u64::from(i) * 5000})
        })
        .collect();
    id(&run(e, "ink.stroke.add", json!({"note": note, "samples": samples})))
}

pub fn objects(e: &Engine, note: u64) -> Vec<Value> {
    let v = e.inspect(Some(&format!("/ws/canvases/{note}/objects"))).unwrap();
    let mut o: Vec<Value> = v.as_object().unwrap().values().cloned().collect();
    o.sort_by_key(|x| x["id"].as_u64().unwrap());
    o
}

pub fn budgets() -> toml::Table {
    toml::from_str(&std::fs::read_to_string(root().join("tests/budgets.toml")).unwrap()).unwrap()
}

pub fn budget(section: &str, key: &str) -> f64 {
    let b = budgets();
    let v = &b[section][key];
    v.as_float()
        .or_else(|| v.as_integer().map(|i| i as f64))
        .unwrap_or_else(|| panic!("budget {section}.{key}"))
}

/// Reference workload (R-PERF-000): 10k notes in 500 folders (depth ≤ 8) plus one note with
/// 100k strokes (40–120 samples, avg ~80). Built via model ops, cached as a database under the
/// target dir. Returns (db path, reference note id).
pub fn reference_db() -> (PathBuf, NoteId) {
    let dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target"))
        .join("acceptance-ref");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reference-v2.sqlite");
    let note = NoteId(1_000_000);
    if !path.exists() {
        let tmp = dir.join("reference-v2.tmp.sqlite");
        let _ = std::fs::remove_file(&tmp);
        build_reference(&tmp, note);
        std::fs::rename(&tmp, &path).unwrap();
    }
    (path, note)
}

fn build_reference(path: &Path, note: NoteId) {
    let b = budgets();
    let folders = b["reference"]["folders"].as_integer().unwrap() as u64;
    let notes = b["reference"]["notes"].as_integer().unwrap() as u64;
    let strokes = b["reference"]["note_strokes"].as_integer().unwrap() as u64;
    let mut ws = Workspace::new();
    for f in 0..folders {
        let parent = if f < 20 {
            None
        } else {
            Some(FolderId(1 + (f % 20) + 20 * ((f / 20) % 7).min(1)))
        };
        let parent = parent.filter(|p| ws.folders.contains_key(p));
        ws.apply(Op::CreateFolder {
            folder: Folder {
                id: FolderId(1 + f),
                parent,
                name: format!("Ordner {f}"),
                color: None,
                sort: f as i64,
            },
        })
        .unwrap();
    }
    for n in 0..notes {
        ws.apply(Op::CreateNote {
            note: Note {
                id: NoteId(10_000 + n),
                folder: Some(FolderId(1 + n % folders)),
                name: format!("Notiz {n} Vorlesung"),
                color: None,
                sort: n as i64,
                background: None,
            },
            canvas: Canvas::default(),
        })
        .unwrap();
    }
    ws.apply(Op::CreateNote {
        note: Note {
            id: note,
            folder: None,
            name: "Reference".into(),
            color: None,
            sort: 0,
            background: None,
        },
        canvas: Canvas::default(),
    })
    .unwrap();
    let mut r = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = || {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        (r >> 11) as f64 / (1u64 << 53) as f64
    };
    let per_page = 44 * 12;
    let mut objs = std::collections::BTreeMap::new();
    for i in 0..strokes {
        let (pg, k) = (i / per_page, i % per_page);
        let (line, word) = (k / 12, k % 12);
        let ox = (pg % 14) as f64 * 1600.0 + 60.0 + word as f64 * 110.0;
        let oy = (pg / 14) as f64 * 2200.0 + 80.0 + line as f64 * 44.0;
        let n = 40 + (rnd() * 80.0) as u64;
        let (w, loops, ph) = (60.0 + rnd() * 40.0, 3.0 + rnd() * 4.0, rnd() * std::f64::consts::TAU);
        let samples = (0..n)
            .map(|j| {
                let u = j as f64 / (n - 1) as f64;
                let a = loops * std::f64::consts::TAU * u;
                Sample {
                    x: ox + w * u - 5.0 * a.sin(),
                    y: oy - 9.0 * (1.0 - (a + ph).cos()),
                    pressure: (0.35 + 0.4 * (u * 3.1 + ph).sin().abs()) as f32,
                    tilt_x: 15.0,
                    tilt_y: -10.0,
                    t_us: j * 5000,
                }
                .quantised()
            })
            .collect();
        let id = ObjectId(2_000_000 + i);
        objs.insert(
            id,
            CanvasObject {
                id,
                z: i as i64,
                kind: ObjectKind::Stroke(Stroke {
                    tool: Tool::Pen,
                    color: Color::INK,
                    width: 2.0,
                    samples,
                }),
            },
        );
    }
    ws.canvases.insert(note, Canvas { objects: objs });
    ws.next_id = 3_000_000;
    vibenote_storage::Store::open(path).unwrap().save_all(&ws).unwrap();
}

/// Loads an ink fixture from tests/fixtures/ink.
pub fn fixture(name: &str) -> vibenote_ink::input::Fixture {
    let p = root().join("tests/fixtures/ink").join(name);
    vibenote_ink::input::Fixture::read(std::io::BufReader::new(std::fs::File::open(&p).unwrap())).unwrap()
}

/// Feeds a fixture to `input.pen` in batches.
pub fn replay(e: &mut Engine, f: &vibenote_ink::input::Fixture) -> Vec<Value> {
    f.events
        .chunks(64)
        .map(|c| run(e, "input.pen", json!({"events": c})))
        .collect()
}
