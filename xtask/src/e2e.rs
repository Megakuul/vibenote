//! App-level benches through the real Wayland path (`cargo xtask bench <app-smoke|latency|startup>`).
//! They drive the app with simtab on the owner's session (`tools/harness/live-pen.sh`: refuses
//! while locked, dedicated workspace, restores the previous one) and write reports to
//! `<repo>/target/e2e-*.json`, which the acceptance tests read.

// `serde_json::Value[key]` returns `Null` for missing keys and never panics; clippy's
// indexing lint cannot tell it apart from slice indexing.
#![allow(clippy::indexing_slicing)]

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from)
}

fn run(cmd: &mut Command) -> Result<()> {
    let st = cmd.current_dir(root()).status().with_context(|| format!("{cmd:?}"))?;
    if !st.success() {
        bail!("{cmd:?} failed: {st}");
    }
    Ok(())
}

/// Builds the app (debug) and simtab (release).
fn build() -> Result<()> {
    run(Command::new(cargo()).args(["build", "-q", "-p", "vibenote"]))?;
    run(Command::new(cargo()).args(["build", "-q", "--release", "-p", "simtab"]))
}

/// Number of canvas objects in note 1 of a workspace (via the CLI's `inspect`).
fn objects(ws: &Path) -> Result<usize> {
    let out = Command::new(cargo())
        .args(["run", "-q", "-p", "vibenote-cli", "--", "--workspace"])
        .arg(ws)
        .args(["inspect", "/ws/canvases/1/objects"])
        .current_dir(root())
        .output()?;
    if !out.status.success() {
        bail!("inspect failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let v: Value = serde_json::from_slice(&out.stdout)?;
    Ok(v.as_object().map_or(0, serde_json::Map::len))
}

fn write_report(name: &str, v: &Value) -> Result<()> {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        // Provenance: when the report was made (stale reports are visible).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        o.insert("generated_unix".into(), json!(now));
    }
    let dir = root().join("target");
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(format!("e2e-{name}.json"));
    std::fs::write(&p, serde_json::to_vec_pretty(&v)?)?;
    eprintln!("bench: wrote {}", p.display());
    Ok(())
}

/// R-PLAT-002: the app opens a window and receives one pen stroke through the real path.
pub fn app_smoke() -> Result<()> {
    build()?;
    let out = target_dir().join("e2e/app-smoke");
    std::fs::create_dir_all(&out)?;
    let st = Command::new(root().join("tools/harness/live-pen.sh"))
        .arg(&out)
        .args(["--pattern", "lines", "--strokes", "1"])
        .current_dir(root())
        .status()?;
    let window_opened = st.success();
    let strokes = if window_opened {
        objects(&out.join("ws.sqlite"))?
    } else {
        0
    };
    let log = std::fs::read_to_string(out.join("app.stderr")).unwrap_or_default();
    let report = json!({
        "window_opened": window_opened,
        "pen_strokes_received": strokes,
        "app_log": log.lines().take(5).collect::<Vec<_>>(),
        "measured": "[sim] simtab → uinput → libinput → compositor → zwp_tablet_v2 → app",
    });
    write_report("app-smoke", &report)?;
    if !window_opened || strokes != 1 {
        bail!("app-smoke: window_opened={window_opened}, strokes={strokes}");
    }
    Ok(())
}

// ---- latency (R-PEN-001 / R-PERF-001) -------------------------------------------------------

/// simtab device extents (tools/simtab: 330 × 210 mm at 100 units/mm).
const X_MAX: f64 = 33_000.0;
const Y_MAX: f64 = 21_000.0;

fn jsonl(p: &Path) -> Result<Vec<Value>> {
    let s = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
    Ok(s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// Nearest-rank percentiles of a sample (ms), plus count and max.
pub fn stats(mut v: Vec<f64>) -> Value {
    v.retain(|x| x.is_finite());
    v.sort_by(f64::total_cmp);
    let pct = |q: f64| -> Value {
        if v.is_empty() {
            return Value::Null;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let rank = (q * v.len() as f64).ceil() as usize;
        let i = rank.clamp(1, v.len()).saturating_sub(1);
        v.get(i).map_or(Value::Null, |x| json!((x * 1000.0).round() / 1000.0))
    };
    json!({"n": v.len(), "p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99), "max": pct(1.0)})
}

fn strokes<'a>(it: impl Iterator<Item = &'a Value>, starts: impl Fn(&Value) -> Option<bool>) -> Vec<Vec<&'a Value>> {
    // `starts`: Some(true) = contact sample, Some(false) = not in contact, None = ignore.
    let mut out = Vec::new();
    let mut cur: Vec<&Value> = Vec::new();
    for v in it {
        match starts(v) {
            Some(true) => cur.push(v),
            Some(false) if !cur.is_empty() => out.push(std::mem::take(&mut cur)),
            Some(false) | None => {}
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// App contact strokes: `down` starts one, `motion` extends it only while in contact (hover
/// motions before/after are not contact samples), `up` ends it.
pub fn app_strokes<'a>(evs: &[&'a Value]) -> Vec<Vec<&'a Value>> {
    let mut out = Vec::new();
    let mut cur: Option<Vec<&Value>> = None;
    for e in evs {
        match e["t"].as_str() {
            Some("down") => {
                if let Some(c) = cur.take().filter(|c| !c.is_empty()) {
                    out.push(c);
                }
                cur = Some(vec![*e]);
            }
            Some("motion") => {
                if let Some(c) = cur.as_mut() {
                    c.push(*e);
                }
            }
            Some("up" | "prox_out") => {
                if let Some(c) = cur.take().filter(|c| !c.is_empty()) {
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    if let Some(c) = cur.filter(|c| !c.is_empty()) {
        out.push(c);
    }
    out
}

/// Matches simtab emits with app events and presentation times. Method:
/// docs/spikes/input-results.md §3 (e2e = presented − simtab emit).
pub fn analyze_latency(dir: &Path) -> Result<Value> {
    let sim = jsonl(&dir.join("sim.jsonl"))?;
    let app = jsonl(&dir.join("app.jsonl"))?;
    require_monotonic(&app)?;
    let win: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("window.json"))?)?;
    let mons: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("monitors.json"))?)?;
    let mon = mons
        .as_array()
        .and_then(|a| a.iter().find(|m| m.get("id") == win.get("monitor")))
        .cloned()
        .context("window's monitor not found")?;
    let scale = mon.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
    let (mx, my) = (num(&mon, "x"), num(&mon, "y"));
    let (mw, mh) = (num(&mon, "width") / scale, num(&mon, "height") / scale);
    let at = |i: usize| {
        win.get("at")
            .and_then(|a| a.get(i))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let (wx, wy) = (at(0) - mx, at(1) - my);
    let expect = |s: &Value| (num(s, "x") / X_MAX * mw - wx, num(s, "y") / Y_MAX * mh - wy);

    let sstrokes = strokes(sim.iter(), |s| s.get("touch").and_then(Value::as_bool));
    let evs: Vec<&Value> = app.iter().filter(|v| v["k"] == "ev").collect();
    let astrokes = app_strokes(&evs);
    let mut draws: Vec<(u64, u64, i64)> = app
        .iter()
        .filter(|v| v["k"] == "draw")
        .map(|v| {
            (
                v["f"].as_u64().unwrap_or(0),
                v["ns"].as_u64().unwrap_or(0),
                v["last"].as_i64().unwrap_or(-1),
            )
        })
        .collect();
    draws.sort_unstable();
    let pres: std::collections::HashMap<u64, u64> = app
        .iter()
        .filter(|v| v["k"] == "pres")
        .map(|v| (v["f"].as_u64().unwrap_or(0), v["ns"].as_u64().unwrap_or(0)))
        .collect();
    let discarded = app.iter().filter(|v| v["k"] == "disc").count();
    let display_of = |i: i64| -> Option<(u64, u64)> {
        draws
            .iter()
            .filter(|d| d.2 >= i)
            .find_map(|d| pres.get(&d.0).map(|p| (*p, d.1)))
    };

    #[allow(clippy::cast_precision_loss)]
    let ms = |a: u64, b: u64| (a as f64 - b as f64) / 1e6;
    let (mut e2e, mut inp, mut app_sub, mut sub_pres, mut pos_err) = (vec![], vec![], vec![], vec![], vec![]);
    let mut matched = 0usize;
    for (ss, aa) in sstrokes.iter().zip(&astrokes) {
        let mut j = 0usize;
        for (idx, e) in aa.iter().enumerate() {
            let d = |s: &Value| {
                let (x, y) = expect(s);
                ((x - num(e, "x")).powi(2) + (y - num(e, "y")).powi(2)).sqrt()
            };
            if ss.len() == aa.len() {
                j = idx; // 1:1 delivery: by index
            } else {
                // bounded forward search (paths may self-intersect)
                let lim = j.saturating_add(8).min(ss.len().saturating_sub(1));
                let mut best = j;
                for c in j..=lim {
                    if let (Some(a), Some(b)) = (ss.get(c), ss.get(best))
                        && d(a) < d(b)
                    {
                        best = c;
                    }
                }
                j = best;
            }
            let Some(s) = ss.get(j) else { continue };
            matched = matched.saturating_add(1);
            pos_err.push(d(s));
            let sns = s["mono_ns"].as_u64().unwrap_or(0);
            let ans = e["ns"].as_u64().unwrap_or(0);
            inp.push(ms(ans, sns));
            if let Some((pns, subns)) = display_of(e["i"].as_i64().unwrap_or(-1)) {
                e2e.push(ms(pns, sns));
                app_sub.push(ms(subns, ans));
                sub_pres.push(ms(pns, subns));
            }
        }
    }
    let sim_contact: usize = sstrokes.iter().map(Vec::len).sum();
    let app_contact: usize = astrokes.iter().map(Vec::len).sum();
    let e2e_stats = stats(e2e);
    Ok(json!({
        "p50_ms": e2e_stats["p50"], "p95_ms": e2e_stats["p95"], "p99_ms": e2e_stats["p99"],
        "e2e_ms": e2e_stats,
        "input_to_app_ms": stats(inp), "app_to_submit_ms": stats(app_sub), "submit_to_present_ms": stats(sub_pres),
        "pos_err_px": stats(pos_err),
        "strokes": {"sim": sstrokes.len(), "app": astrokes.len()},
        "contact_events": {"sim": sim_contact, "app": app_contact, "matched": matched},
        "frames": {"drawn": draws.len(), "presented": pres.len(), "discarded": discarded},
        "meta": app.iter().filter(|v| v["k"] == "meta").collect::<Vec<_>>(),
        "measured": "[sim] simtab emit (CLOCK_MONOTONIC) → wp_presentation presented time; method docs/spikes/input-results.md §3",
    }))
}

/// R-PEN-001 / R-PERF-001: e2e software latency [sim], release build, Phase 0 workload
/// (scribble, 200 Hz, pressure 0.5) with 10 strokes × 1.5 s.
/// `reference`: the app shows the 100k-stroke reference note (a copy of the workspace built by
/// `cargo xtask parity`); the report goes to `target/e2e-latency-reference.json`.
pub fn latency(reference: bool) -> Result<()> {
    run(Command::new(cargo()).args(["build", "-q", "--release", "-p", "vibenote", "-p", "simtab"]))?;
    let out = target_dir().join(if reference {
        "e2e/latency-reference"
    } else {
        "e2e/latency"
    });
    std::fs::create_dir_all(&out)?;
    let mut cmd = Command::new(root().join("tools/harness/live-pen.sh"));
    if reference {
        let db = target_dir().join("acceptance-ref/reference-v2.sqlite");
        if !db.exists() {
            bail!(
                "latency: no reference workspace at {} — run `cargo xtask parity` first",
                db.display()
            );
        }
        cmd.env("LIVE_WORKSPACE_FROM", db)
            .env("LIVE_APP_ARGS", "--note 1000000");
    }
    let st = cmd
        .arg(&out)
        .args([
            "--pattern",
            "scribble",
            "--strokes",
            "10",
            "--stroke-ms",
            "1500",
            "--rate",
            "200",
        ])
        .env("VIBENOTE_BIN", target_dir().join("release/vibenote"))
        .env("VIBENOTE_LATENCY_LOG", out.join("app.jsonl"))
        .current_dir(root())
        .status()?;
    if !st.success() {
        bail!("latency: live run failed ({st})");
    }
    for f in ["ws.sqlite", "ws.sqlite-wal", "ws.sqlite-shm"] {
        let _ = std::fs::remove_file(out.join(f));
    }
    report_latency(&out, if reference { "latency-reference" } else { "latency" })
}

/// Analyses a recorded run (`cargo xtask bench latency --analyze <dir>`) and writes the report.
pub fn report_latency(out: &Path, name: &str) -> Result<()> {
    let report = analyze_latency(out)?;
    let sim = report["contact_events"]["sim"].as_u64().unwrap_or(0);
    let matched = report["contact_events"]["matched"].as_u64().unwrap_or(0);
    let shown = report["e2e_ms"]["n"].as_u64().unwrap_or(0);
    let pos95 = report["pos_err_px"]["p95"].as_f64().unwrap_or(f64::INFINITY);
    eprintln!("{}", serde_json::to_string_pretty(&report)?);
    // Sanity: a broken mapping or lost events must not produce a "good" number.
    // Events never shown on a presented frame have no e2e sample; they must not vanish silently.
    let low = |n: u64| n.saturating_mul(10) < sim.saturating_mul(9);
    if sim == 0 || low(matched) || low(shown) || pos95 > 2.0 {
        bail!("latency: implausible match (sim {sim}, matched {matched}, shown {shown}, pos err p95 {pos95} px)");
    }
    write_report(name, &report)
}

// ---- startup (R-PERF-003) -------------------------------------------------------------------

/// simtab and the launcher stamp CLOCK_MONOTONIC; presentation times are only comparable when
/// the compositor's wp_presentation clock is the same (clock_id 1).
fn require_monotonic(app: &[Value]) -> Result<()> {
    let id = app.iter().find_map(|v| v["v"]["clock_id"].as_u64());
    if id != Some(1) {
        bail!("presentation clock is {id:?}, not CLOCK_MONOTONIC (1): times are not comparable");
    }
    Ok(())
}

/// Launch → first presented frame, from one run's latency log.
fn startup_ms(dir: &Path) -> Result<f64> {
    let app = jsonl(&dir.join("app.jsonl"))?;
    require_monotonic(&app)?;
    let launch = app
        .iter()
        .find_map(|v| v["v"]["launch_ns"].as_u64())
        .context("no launch_ns in the app log")?;
    let first = app
        .iter()
        .filter(|v| v["k"] == "pres")
        .filter_map(|v| v["ns"].as_u64())
        .min()
        .context("no presented frame")?;
    #[allow(clippy::cast_precision_loss)]
    let ms = (first as f64 - launch as f64) / 1e6;
    Ok(ms)
}

/// R-PERF-003: cold start (warm disk cache) to the first presented frame, release build, 5 runs
/// each with a fresh workspace and with a copy of the reference workspace (R-PERF-000, built by
/// the acceptance suite) showing the reference note; the report's `cold_start_ms` is the worst.
pub fn startup() -> Result<()> {
    let reference = target_dir().join("acceptance-ref/reference-v2.sqlite");
    if !reference.exists() {
        bail!(
            "startup: no reference workspace at {} — run `cargo xtask parity` first (it builds it)",
            reference.display()
        );
    }
    run(Command::new(cargo()).args(["build", "-q", "--release", "-p", "vibenote"]))?;
    let mut fresh = Vec::new();
    let mut populated = Vec::new();
    for k in 0..10 {
        let with_ref = k % 2 == 1;
        let out = target_dir().join(format!("e2e/startup-{k}"));
        std::fs::create_dir_all(&out)?;
        let mut cmd = Command::new(root().join("tools/harness/live-pen.sh"));
        cmd.arg(&out)
            .env("LIVE_NO_PEN", "1")
            .env("VIBENOTE_BIN", target_dir().join("release/vibenote"))
            .env("VIBENOTE_LATENCY_LOG", out.join("app.jsonl"))
            .current_dir(root());
        if with_ref {
            // The reference note (id fixed by the acceptance suite's generator).
            cmd.env("LIVE_WORKSPACE_FROM", &reference)
                .env("LIVE_APP_ARGS", "--note 1000000");
        }
        let st = cmd.status()?;
        // The copy is 120 MB of RAM-backed target dir; drop it right away.
        for f in ["ws.sqlite", "ws.sqlite-wal", "ws.sqlite-shm"] {
            let _ = std::fs::remove_file(out.join(f));
        }
        if !st.success() {
            bail!("startup: run {k} failed ({st})");
        }
        let ms = startup_ms(&out)?;
        if with_ref {
            populated.push(ms);
        } else {
            fresh.push(ms);
        }
    }
    let worst = fresh.iter().chain(&populated).copied().fold(f64::NAN, f64::max);
    let report = json!({
        "cold_start_ms": (worst * 1000.0).round() / 1000.0,
        "runs_ms": {"fresh": fresh, "reference": populated},
        "workspace": "fresh (empty) and the reference workspace (R-PERF-000 notes/folders + the 100k-stroke note, opened at start)",
        "measured": "[sim] launcher CLOCK_MONOTONIC before exec → first wp_presentation presented frame; release build, warm disk cache",
    });
    eprintln!("{}", serde_json::to_string_pretty(&report)?);
    write_report("startup", &report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_motion_is_not_contact() {
        let evs: Vec<Value> = [
            "prox_in", "motion", "motion", "down", "motion", "motion", "up", "motion", "prox_out",
        ]
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"t": t, "i": i}))
        .collect();
        let refs: Vec<&Value> = evs.iter().collect();
        let s = app_strokes(&refs);
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].iter().map(|e| e["i"].as_u64().unwrap()).collect::<Vec<_>>(),
            [3, 4, 5]
        );
    }

    #[test]
    fn percentiles_nearest_rank() {
        let v = stats((1..=100).map(f64::from).collect());
        assert_eq!(v["p50"], json!(50.0));
        assert_eq!(v["p95"], json!(95.0));
        assert_eq!(v["max"], json!(100.0));
        assert_eq!(stats(vec![])["p50"], Value::Null);
    }
}
