//! Latency/axis analysis: joins a simtab emit log with an app log (common::log format).
//!
//! Usage: analyze <results/name>   (reads name.sim.jsonl, name.app.jsonl[.pres], name.window.json,
//! name.monitors.json) → prints a JSON summary line.
//!
//! Method: simtab logs CLOCK_MONOTONIC right before writing each evdev frame to uinput. The app
//! logs every contact event it receives (index i) and, per submitted frame f, the last event
//! index included; wp_presentation gives f's presentation time (CLOCK_MONOTONIC, clock_id 1).
//! Event i is "on screen" at the presentation time of the first presented frame whose
//! last >= i. Latency(i) = that time − simtab emit time of the matched evdev frame.
//! Matching: per stroke (simtab touch runs ↔ app down..up), each app event is matched to the
//! simtab frame whose expected surface position (tablet→monitor→window mapping) is nearest,
//! scanning forward only.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

const X_MAX: f64 = 33_000.0;
const Y_MAX: f64 = 21_000.0;
const P_MAX: f64 = 8191.0;

fn lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(&l.replace("NaN", "null").replace("inf", "null")).ok())
        .collect()
}

fn f(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(f64::NAN)
}

fn stats(mut v: Vec<f64>) -> Value {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return json!(null);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let r = |x: f64| (x * 100.0).round() / 100.0;
    json!({"n": v.len(), "min": r(v[0]), "p50": r(q(0.5)), "p90": r(q(0.9)), "p95": r(q(0.95)), "p99": r(q(0.99)), "max": r(v[v.len()-1]), "mean": r(mean)})
}

fn main() -> Result<()> {
    let base = std::env::args().nth(1).context("usage: analyze <results/name>")?;
    let sim = lines(&format!("{base}.sim.jsonl"));
    let mut app = lines(&format!("{base}.app.jsonl"));
    app.extend(lines(&format!("{base}.app.jsonl.pres")));
    let win: Value = serde_json::from_str(&std::fs::read_to_string(format!("{base}.window.json")).unwrap_or("{}".into()))?;
    let mons: Value = serde_json::from_str(&std::fs::read_to_string(format!("{base}.monitors.json")).unwrap_or("[]".into()))?;
    let mon = mons.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
    let scale = mon["scale"].as_f64().unwrap_or(1.0);
    let (mx, my) = (f(&mon, "x"), f(&mon, "y"));
    let (mw, mh) = (f(&mon, "width") / scale, f(&mon, "height") / scale);
    let (wx, wy) = (win["at"][0].as_f64().unwrap_or(0.0) - mx, win["at"][1].as_f64().unwrap_or(0.0) - my);
    let expect = |s: &Value| (f(s, "x") / X_MAX * mw - wx, f(s, "y") / Y_MAX * mh - wy);

    // simtab strokes: runs of touch=true
    let mut sstrokes: Vec<Vec<&Value>> = vec![];
    let mut cur: Vec<&Value> = vec![];
    for s in &sim {
        if s["touch"].as_bool() == Some(true) {
            cur.push(s);
        } else if !cur.is_empty() {
            sstrokes.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        sstrokes.push(cur);
    }

    // app strokes + frames
    let meta: Vec<&Value> = app.iter().filter(|v| v["k"] == "meta").collect();
    let evs: Vec<&Value> = app.iter().filter(|v| v["k"] == "ev").collect();
    let mut astrokes: Vec<Vec<&Value>> = vec![];
    let mut cur: Vec<&Value> = vec![];
    for e in &evs {
        match e["t"].as_str() {
            Some("down") => {
                if !cur.is_empty() {
                    astrokes.push(std::mem::take(&mut cur));
                }
                cur.push(e)
            }
            Some("motion") => cur.push(e),
            Some("up") => {
                if !cur.is_empty() {
                    astrokes.push(std::mem::take(&mut cur));
                }
            }
            _ => {}
        }
    }
    if !cur.is_empty() {
        astrokes.push(cur);
    }
    let mut draws: Vec<(u64, u64, i64)> = app
        .iter()
        .filter(|v| v["k"] == "draw")
        .map(|v| (v["f"].as_u64().unwrap_or(0), v["ns"].as_u64().unwrap_or(0), v["last"].as_i64().unwrap_or(-1)))
        .collect();
    draws.sort();
    let pres: HashMap<u64, (u64, u64)> = app
        .iter()
        .filter(|v| v["k"] == "pres")
        .map(|v| (v["f"].as_u64().unwrap_or(0), (v["ns"].as_u64().unwrap_or(0), v["refresh"].as_u64().unwrap_or(0))))
        .collect();
    let n_disc = app.iter().filter(|v| v["k"] == "disc").count();
    // event index -> display time
    let display_of = |i: i64| -> Option<(u64, u64)> {
        draws.iter().filter(|d| d.2 >= i).find_map(|d| pres.get(&d.0).map(|p| (p.0, d.1)))
    };

    let mut e2e = vec![];
    let mut inp = vec![];
    let mut app_to_submit = vec![];
    let mut submit_to_pres = vec![];
    let mut pos_err = vec![];
    let mut p_err = vec![];
    let mut tilt_err = vec![];
    let mut matched = 0usize;
    let mut exact_idx = 0usize;
    let n_pairs = sstrokes.len().min(astrokes.len());
    for k in 0..n_pairs {
        let (ss, aa) = (&sstrokes[k], &astrokes[k]);
        let mut j = 0usize;
        for (idx, e) in aa.iter().enumerate() {
            let (ex, ey) = (f(e, "x"), f(e, "y"));
            let d = |s: &Value| {
                let (x, y) = expect(s);
                ((x - ex).powi(2) + (y - ey).powi(2)).sqrt()
            };
            if ss.len() == aa.len() {
                // 1:1 delivery (verified by count): match by index.
                j = idx;
            } else {
                // Bounded forward search (path may self-intersect).
                let lim = (j + 8).min(ss.len().saturating_sub(1));
                let mut best = j;
                for c in j..=lim {
                    if d(ss[c]) < d(ss[best]) {
                        best = c;
                    }
                }
                j = best;
            }
            let s = ss[j];
            matched += 1;
            if ss.len() == aa.len() { exact_idx += 1; }
            pos_err.push(d(s));
            let lo = (P_MAX * 0.01).round();
            let p_exp = ((f(s, "p") - lo) / (P_MAX - lo)).clamp(0.0, 1.0);
            if f(e, "p") >= 0.0 {
                p_err.push((f(e, "p") - p_exp).abs());
            }
            if f(e, "tx").is_finite() {
                tilt_err.push((f(e, "tx") - f(s, "tx") / 100.0).abs().max((f(e, "ty") - f(s, "ty") / 100.0).abs()));
            }
            let sns = s["mono_ns"].as_u64().unwrap_or(0) as f64;
            let ans = e["ns"].as_u64().unwrap_or(0) as f64;
            inp.push((ans - sns) / 1e6);
            if let Some((pns, subns)) = display_of(e["i"].as_i64().unwrap_or(-1)) {
                e2e.push((pns as f64 - sns) / 1e6);
                app_to_submit.push((subns as f64 - ans) / 1e6);
                submit_to_pres.push((pns as f64 - subns as f64) / 1e6);
            }
        }
    }
    // Visible tip vs. true pen position at presentation time (captures prediction).
    let simpath: Vec<(f64, f64, f64)> = sstrokes
        .iter()
        .flatten()
        .map(|s| {
            let (x, y) = expect(s);
            (s["mono_ns"].as_u64().unwrap_or(0) as f64, x, y)
        })
        .collect();
    let mut tip_lag_ms = vec![];
    let mut tip_lag_px = vec![];
    for t in app.iter().filter(|v| v["k"] == "tip") {
        let Some(&(pns, _)) = pres.get(&t["f"].as_u64().unwrap_or(u64::MAX)) else { continue };
        let pns = pns as f64;
        let (tx, ty) = (f(t, "x"), f(t, "y"));
        // pen position at presentation time = last emitted frame before it
        let Some(k) = simpath.iter().rposition(|p| p.0 <= pns) else { continue };
        if pns - simpath[k].0 > 20e6 {
            continue; // pen already lifted
        }
        tip_lag_px.push(((simpath[k].1 - tx).powi(2) + (simpath[k].2 - ty).powi(2)).sqrt());
        // time at which the pen was nearest to the drawn tip (search ±100 ms)
        let best = simpath
            .iter()
            .filter(|p| (p.0 - pns).abs() < 100e6)
            .min_by(|a, b| {
                let da = (a.1 - tx).powi(2) + (a.2 - ty).powi(2);
                let db = (b.1 - tx).powi(2) + (b.2 - ty).powi(2);
                da.total_cmp(&db)
            });
        if let Some(b) = best {
            tip_lag_ms.push((pns - b.0) / 1e6);
        }
    }
    let sim_contact: usize = sstrokes.iter().map(|s| s.len()).sum();
    let app_contact: usize = astrokes.iter().map(|s| s.len()).sum();
    let hist = evs.iter().filter(|e| e["hist"] == true).count();
    // present interval
    let mut pt: Vec<u64> = pres.values().map(|p| p.0).collect();
    pt.sort();
    let pint: Vec<f64> = pt.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e6).collect();
    let refresh = pres.values().map(|p| p.1).max().unwrap_or(0) as f64 / 1e6;
    let out = json!({
        "name": base, "meta": meta.last(),
        "strokes": {"sim": sstrokes.len(), "app": astrokes.len()},
        "contact_events": {"sim_frames": sim_contact, "app_events": app_contact, "app_from_history": hist, "matched": matched, "matched_by_index": exact_idx},
        "frames": {"drawn": draws.len(), "presented": pres.len(), "discarded": n_disc, "refresh_ms": refresh, "present_interval_ms": stats(pint)},
        "latency_ms": {"e2e_sim_to_present": stats(e2e), "input_sim_to_app": stats(inp), "app_to_submit": stats(app_to_submit), "submit_to_present": stats(submit_to_pres)},
        "visible_tip": {"lag_ms": stats(tip_lag_ms), "lag_px": stats(tip_lag_px)},
        "accuracy": {"pos_err_px": stats(pos_err), "pressure_abs_err": stats(p_err), "tilt_abs_err_deg": stats(tilt_err)},
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
