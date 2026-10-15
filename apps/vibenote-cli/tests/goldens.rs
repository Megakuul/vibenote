#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]
//! Golden images (AGENTS.md §2 Phase 3 item 6). Each case in `tests/golden/cases.json` replays an
//! ink fixture and renders it headlessly; the result must match `tests/golden/<name>.png`
//! (per-channel tolerance 6, at most 0.1 % pixels differing).
//! On mismatch or missing golden: writes `<out>/<name>.new.png` + `<name>.diff.png` and fails.
//! `<out>` = $VIBENOTE_GOLDEN_OUT or $CARGO_TARGET_DIR/golden-new. Only the owner approves new
//! goldens (`cargo xtask golden --approve`).

#[path = "../src/headless.rs"]
mod headless;

use serde_json::Value;
use std::path::PathBuf;
use vibenote_render::{Headless, Image, Viewport, compare};

const CHANNEL_TOL: u8 = 6;
const MAX_FRACTION: f64 = 0.001;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn out_dir() -> PathBuf {
    std::env::var_os("VIBENOTE_GOLDEN_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| root().join("target"))
                .join("golden-new")
        })
}

/// Replays a case's fixture, runs its optional `commands` and renders it.
fn render_case(r: &mut Headless, c: &Value) -> Image {
    let f = headless::read_fixture(&root().join("tests/fixtures/ink").join(c["fixture"].as_str().unwrap())).unwrap();
    let (mut e, note) = headless::replay_fixture(&f).unwrap();
    if let Some(cmds) = c.get("commands") {
        // "$ROOT" in a command stands for the repository root (fixture paths).
        let text = cmds.to_string().replace("$ROOT", root().to_str().unwrap());
        let cmds: Vec<Value> = serde_json::from_str(&text).unwrap();
        headless::run_commands(&mut e, &cmds).unwrap();
    }
    let vp = Viewport {
        origin_x: c["origin"][0].as_f64().unwrap(),
        origin_y: c["origin"][1].as_f64().unwrap(),
        zoom: c["zoom"].as_f64().unwrap(),
        width: c["size"][0].as_u64().unwrap() as u32,
        height: c["size"][1].as_u64().unwrap() as u32,
    };
    headless::render_note(r, &e, note, &vp).unwrap()
}

fn cases(path: &str) -> Vec<Value> {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).unwrap()).unwrap()
}

/// Proposed goldens (not yet approved; `apps/vibenote-cli/tests/golden-proposals.json`) are
/// rendered to `<out>/<name>.new.png` for the owner's review (`cargo xtask golden --review`).
/// On approval the owner moves the case into `tests/golden/cases.json` and runs
/// `cargo xtask golden --approve <name>` (a proposal whose name is approved is skipped here).
/// Fails only if a proposal cannot render or names no requirement.
#[test]
fn proposed_goldens_render() {
    let approved: Vec<String> = cases("tests/golden/cases.json")
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    let out = out_dir();
    std::fs::create_dir_all(&out).unwrap();
    let mut r = Headless::new().unwrap();
    for c in cases("apps/vibenote-cli/tests/golden-proposals.json") {
        let name = c["name"].as_str().unwrap();
        if approved.iter().any(|a| a == name) {
            continue; // approved since: the case runs in `goldens_match`
        }
        assert!(
            c["reqs"].as_array().is_some_and(|r| !r.is_empty()),
            "{name}: no requirement ids"
        );
        let img = render_case(&mut r, &c);
        std::fs::write(out.join(format!("{name}.new.png")), img.encode_png().unwrap()).unwrap();
    }
}

#[test]
fn goldens_match() {
    let cases = cases("tests/golden/cases.json");
    let out = out_dir();
    std::fs::create_dir_all(&out).unwrap();
    let mut r = Headless::new().unwrap();
    let mut failures = vec![];
    for c in &cases {
        let name = c["name"].as_str().unwrap();
        let img = render_case(&mut r, c);
        let golden_path = root().join("tests/golden").join(format!("{name}.png"));
        let new_path = out.join(format!("{name}.new.png"));
        let diff_path = out.join(format!("{name}.diff.png"));
        let _ = std::fs::remove_file(&diff_path);
        match std::fs::read(&golden_path) {
            Err(_) => {
                std::fs::write(&new_path, img.encode_png().unwrap()).unwrap();
                failures.push(format!(
                    "{name}: no approved golden (new image: {})",
                    new_path.display()
                ));
            }
            Ok(bytes) => {
                let golden = Image::decode_png(&bytes).unwrap();
                match compare(&golden, &img, CHANNEL_TOL) {
                    Ok(d) if d.fraction() <= MAX_FRACTION => {
                        let _ = std::fs::remove_file(&new_path);
                    }
                    Ok(d) => {
                        std::fs::write(&new_path, img.encode_png().unwrap()).unwrap();
                        std::fs::write(&diff_path, d.diff.encode_png().unwrap()).unwrap();
                        failures.push(format!(
                            "{name}: {} px differ ({:.4} %, max channel diff {})",
                            d.differing,
                            d.fraction() * 100.0,
                            d.max_channel_diff
                        ));
                    }
                    Err(e) => {
                        std::fs::write(&new_path, img.encode_png().unwrap()).unwrap();
                        failures.push(format!("{name}: {e}"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches (adapter {}):\n{}",
        r.adapter,
        failures.join("\n")
    );
}
