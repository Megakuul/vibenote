//! `cargo xtask parity [--raise]` — requirement parity catalog (AGENTS.md §2 Phase 3 item 10).
//!
//! Runs the acceptance suite (`vibenote-acceptance`), maps every test `r_<area>_<nnn>_…` to its
//! SPEC requirement id, adds the commands that declare each requirement (`engine.commands`),
//! writes `docs/parity.md`, and checks the floor `tests/parity_floor.txt`: every requirement
//! listed there must still pass (the set may only grow; `--raise` adds newly passing ids).

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[derive(Default)]
struct Req {
    tests: Vec<(String, bool, String)>,
    commands: Vec<(String, bool)>,
}

fn req_of(test: &str) -> Option<String> {
    // r_pen_007_stroke_eraser → R-PEN-007
    let mut it = test.split('_');
    let (r, area, num) = (it.next()?, it.next()?, it.next()?);
    if r != "r" || num.len() != 3 || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("R-{}-{num}", area.to_uppercase()))
}

fn spec_ids() -> Result<BTreeSet<String>> {
    let spec = std::fs::read_to_string(root().join("SPEC.md"))?;
    let mut ids = BTreeSet::new();
    for l in spec.lines() {
        let l = l.trim_start_matches(['*', '|', ' ']);
        if let Some(rest) = l.strip_prefix("R-") {
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            let id = format!("R-{}", id.trim_end_matches('-'));
            if id.split('-').count() == 3 {
                ids.insert(id);
            }
        }
    }
    ids.remove("R-PERF-000"); // workload definition, not a testable requirement
    Ok(ids)
}

pub fn main(args: &[String]) -> Result<()> {
    let raise = args.iter().any(|a| a == "--raise");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // The PDF worker binary, which PDF tests start (ADR 0004).
    let built = Command::new(&cargo)
        .args(["build", "-q", "-p", "vibenote-pdfd"])
        .current_dir(root())
        .status()
        .context("building vibenote-pdfd")?;
    if !built.success() {
        anyhow::bail!("building vibenote-pdfd failed");
    }
    let out = Command::new(&cargo)
        .args([
            "test",
            "-p",
            "vibenote-acceptance",
            "--no-fail-fast",
            "--",
            "--test-threads=1",
        ])
        .current_dir(root())
        .output()
        .context("running acceptance tests")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    // failure messages: "---- name stdout ----" followed by the panic line
    let mut msgs: BTreeMap<String, String> = BTreeMap::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if let Some(name) = l.strip_prefix("---- ").and_then(|s| s.strip_suffix(" stdout ----")) {
            let msg = lines
                .iter()
                .skip(i.saturating_add(1))
                .take(6)
                .find(|x| !x.trim().is_empty() && !x.starts_with("thread "))
                .map_or("", |v| v)
                .trim();
            let short: String = msg.chars().take(160).collect();
            msgs.insert(name.rsplit("::").next().unwrap_or(name).to_string(), short);
        }
    }
    let mut reqs: BTreeMap<String, Req> = BTreeMap::new();
    for id in spec_ids()? {
        reqs.entry(id).or_default();
    }
    for l in &lines {
        let Some(rest) = l.strip_prefix("test ") else { continue };
        let Some((name, status)) = rest.split_once(" ... ") else {
            continue;
        };
        let ok = status.trim() == "ok";
        let short = name.rsplit("::").next().unwrap_or(name).to_string();
        let Some(id) = req_of(&short) else { continue };
        let msg = msgs.get(&short).cloned().unwrap_or_default();
        reqs.entry(id).or_default().tests.push((short, ok, msg));
    }
    // commands per requirement
    let cmds = Command::new(&cargo)
        .args(["run", "-q", "-p", "vibenote-cli", "--", "commands"])
        .current_dir(root())
        .output()?;
    let cmds: serde_json::Value = serde_json::from_slice(&cmds.stdout).context("vibenote-cli commands")?;
    for c in cmds.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let implemented = c
            .get("implemented")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        for r in c
            .get("reqs")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if let Some(r) = r.as_str() {
                reqs.entry(r.to_string())
                    .or_default()
                    .commands
                    .push((id.clone(), implemented));
            }
        }
    }
    let passing: BTreeSet<String> = reqs
        .iter()
        .filter(|(_, r)| !r.tests.is_empty() && r.tests.iter().all(|t| t.1))
        .map(|(k, _)| k.clone())
        .collect();
    let untested: Vec<&String> = reqs
        .iter()
        .filter(|(_, r)| r.tests.is_empty())
        .map(|(k, _)| k)
        .collect();
    // report
    let mut md = format!(
        "# Requirement parity\n\nGenerated by `cargo xtask parity` — do not edit. Passing: **{} / {}** requirements \
         (a requirement passes when all its acceptance tests pass). Untested: {}.\n\n\
         | Requirement | Status | Acceptance tests | Commands (✓ implemented) |\n|---|---|---|---|\n",
        passing.len(),
        reqs.len(),
        if untested.is_empty() {
            "none".to_string()
        } else {
            untested.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        }
    );
    for (id, r) in &reqs {
        let status = if r.tests.is_empty() {
            "⚠ untested"
        } else if passing.contains(id) {
            "✅ pass"
        } else {
            "❌ red"
        };
        let tests = r
            .tests
            .iter()
            .map(|(n, ok, m)| {
                if *ok {
                    format!("`{n}` ✓")
                } else {
                    format!("`{n}` ✗ {}", m.replace('|', "\\|"))
                }
            })
            .collect::<Vec<_>>()
            .join("<br>");
        let cmds = r
            .commands
            .iter()
            .map(|(c, i)| format!("`{c}`{}", if *i { " ✓" } else { "" }))
            .collect::<Vec<_>>()
            .join(", ");
        md.push_str(&format!("| {id} | {status} | {tests} | {cmds} |\n"));
    }
    std::fs::write(root().join("docs/parity.md"), md)?;
    // floor
    let floor_path = root().join("tests/parity_floor.txt");
    let floor: BTreeSet<String> = std::fs::read_to_string(&floor_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let lost: Vec<&String> = floor.difference(&passing).collect();
    let gained: Vec<&String> = passing.difference(&floor).collect();
    eprintln!(
        "parity: {} / {} requirements pass; floor {}; docs/parity.md written",
        passing.len(),
        reqs.len(),
        floor.len()
    );
    if !untested.is_empty() {
        bail!("requirements without acceptance tests: {untested:?}");
    }
    if !lost.is_empty() {
        bail!("REGRESSION: requirements in the floor no longer pass: {lost:?}");
    }
    if !gained.is_empty() {
        if raise {
            let mut all: Vec<&String> = floor.iter().chain(gained.iter().copied()).collect();
            all.sort();
            all.dedup();
            let body = all.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
            std::fs::write(
                &floor_path,
                format!("# Requirements that pass and must keep passing (AGENTS.md §3; only ever grows).\n{body}\n"),
            )?;
            eprintln!("parity: floor raised by {gained:?}");
        } else {
            eprintln!("parity: newly passing {gained:?} — run `cargo xtask parity --raise` to lock them in");
        }
    }
    Ok(())
}
