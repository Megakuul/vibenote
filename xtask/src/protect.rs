//! Protected test data (AGENTS.md §3 "Tests are protected"): goldens, fixtures, eval sets,
//! budgets, parity floor. `cargo xtask protect --write` records SHA-256 hashes in
//! `tests/PROTECTED.sha256` (done at Gate C by the owner's approval); `ci` reports any change.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROTECTED: &[&str] = &[
    "tests/golden",
    "tests/fixtures",
    "tests/eval",
    "tests/budgets.toml",
    "tests/parity_floor.txt",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn files(p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if p.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        v.sort();
        for c in v {
            files(&c, out)?;
        }
    } else if p.is_file() {
        out.push(p.to_path_buf());
    }
    Ok(())
}

fn hashes() -> Result<BTreeMap<String, String>> {
    let mut all = vec![];
    for p in PROTECTED {
        files(&root().join(p), &mut all)?;
    }
    let mut m = BTreeMap::new();
    for chunk in all.chunks(200) {
        let out = Command::new("sha256sum").args(chunk).output().context("sha256sum")?;
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((h, f)) = l.split_once("  ") {
                let rel = Path::new(f)
                    .strip_prefix(root())
                    .map(|r| r.display().to_string())
                    .unwrap_or_else(|_| f.to_string());
                m.insert(rel, h.to_string());
            }
        }
    }
    Ok(m)
}

fn manifest() -> PathBuf {
    root().join("tests/PROTECTED.sha256")
}

pub fn write() -> Result<()> {
    let m = hashes()?;
    let body: String = m.iter().map(|(f, h)| format!("{h}  {f}\n")).collect();
    std::fs::write(manifest(), body)?;
    eprintln!("protect: recorded {} files in tests/PROTECTED.sha256", m.len());
    Ok(())
}

/// Lists added/changed/removed protected files relative to the manifest (empty if none or no manifest).
pub fn report() -> Result<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(manifest()) else {
        return Ok(vec![
            "(no tests/PROTECTED.sha256 — run `cargo xtask protect --write`)".into(),
        ]);
    };
    let old: BTreeMap<String, String> = text
        .lines()
        .filter_map(|l| l.split_once("  ").map(|(h, f)| (f.to_string(), h.to_string())))
        .collect();
    let new = hashes()?;
    let mut out = vec![];
    for (f, h) in &new {
        match old.get(f) {
            None => out.push(format!("added    {f}")),
            Some(o) if o != h => out.push(format!("changed  {f}")),
            _ => {}
        }
    }
    for f in old.keys() {
        if !new.contains_key(f) {
            out.push(format!("removed  {f}"));
        }
    }
    Ok(out)
}
