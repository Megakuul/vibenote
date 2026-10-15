//! `cargo xtask golden [--review | --approve <name>… | --approve-all-new]`
//!
//! - no flag: runs the golden test (`vibenote-cli --test goldens`).
//! - `--review`: runs it, then writes `<out>/review.html` with approved / new / diff side by side.
//! - `--approve`: copies `<out>/<name>.new.png` to `tests/golden/<name>.png`.
//!   **After Gate C only the owner approves goldens** (AGENTS.md §3 "Tests are protected").

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
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

pub fn run_tests() -> Result<bool> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let st = Command::new(cargo)
        .args(["test", "-q", "-p", "vibenote-cli", "--test", "goldens"])
        .current_dir(root())
        .status()?;
    Ok(st.success())
}

pub fn main(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => {
            if !run_tests()? {
                bail!("golden test failed; run `cargo xtask golden --review`");
            }
            Ok(())
        }
        Some("--review") => {
            let ok = run_tests()?;
            let page = review_page()?;
            eprintln!("golden: review page {}", page.display());
            if !ok {
                bail!("golden mismatches — see the review page");
            }
            Ok(())
        }
        Some("--approve") => {
            if args.len() < 2 {
                bail!("usage: --approve <name>…");
            }
            for n in args.iter().skip(1) {
                approve(n)?;
            }
            Ok(())
        }
        Some("--approve-all-new") => {
            for e in std::fs::read_dir(out_dir())? {
                let name = e?.file_name().to_string_lossy().to_string();
                if let Some(n) = name.strip_suffix(".new.png") {
                    approve(n)?;
                }
            }
            Ok(())
        }
        Some(o) => bail!("unknown option {o}"),
    }
}

fn approve(name: &str) -> Result<()> {
    if name.contains('/') || name.contains("..") {
        bail!("bad name {name}");
    }
    let src = out_dir().join(format!("{name}.new.png"));
    let dst = root().join("tests/golden").join(format!("{name}.png"));
    std::fs::copy(&src, &dst).with_context(|| format!("{} → {}", src.display(), dst.display()))?;
    eprintln!("golden: approved {name}");
    Ok(())
}

fn img(p: &Path) -> String {
    if p.exists() {
        format!("<img src=\"file://{}\">", p.display())
    } else {
        "<em>—</em>".into()
    }
}

fn review_page() -> Result<PathBuf> {
    let cases: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("tests/golden/cases.json"))?)?;
    let out = out_dir();
    std::fs::create_dir_all(&out)?;
    let mut rows = String::new();
    for c in cases.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let reqs = c.get("reqs").map(|v| v.to_string()).unwrap_or_default();
        let golden = root().join("tests/golden").join(format!("{name}.png"));
        let new = out.join(format!("{name}.new.png"));
        let diff = out.join(format!("{name}.diff.png"));
        let status = if !golden.exists() {
            "NEW (no approved golden)"
        } else if new.exists() {
            "CHANGED"
        } else {
            "ok"
        };
        rows.push_str(&format!(
            "<tr><td><b>{name}</b><br>{status}<br><small>{reqs}</small></td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            img(&golden),
            img(&new),
            img(&diff)
        ));
    }
    // Proposed goldens (not approved yet): rendered by the golden tests to `<name>.new.png`.
    let proposals: serde_json::Value =
        std::fs::read_to_string(root().join("apps/vibenote-cli/tests/golden-proposals.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    let approved: Vec<&str> = cases
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| c.get("name").and_then(|v| v.as_str()))
        .collect();
    for c in proposals.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        if approved.contains(&name) {
            continue;
        }
        let reqs = c.get("reqs").map(|v| v.to_string()).unwrap_or_default();
        let new = out.join(format!("{name}.new.png"));
        rows.push_str(&format!(
            "<tr><td><b>{name}</b><br>PROPOSED (to approve: move the case from apps/vibenote-cli/tests/golden-proposals.json into tests/golden/cases.json, then <code>--approve {name}</code>)<br><small>{reqs}</small></td><td></td><td>{}</td><td></td></tr>\n",
            img(&new)
        ));
    }
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>vibenote golden review</title>\
         <style>body{{font:14px sans-serif;background:#f4f3ef}}td{{vertical-align:top;padding:6px;border-bottom:1px solid #ccc}}img{{max-width:420px;border:1px solid #bbb}}</style>\
         <h1>Golden review</h1><p>Approve with <code>cargo xtask golden --approve &lt;name&gt;</code> (owner only after Gate C).</p>\
         <table><tr><th>case</th><th>approved</th><th>new</th><th>diff</th></tr>{rows}</table>"
    );
    let p = out.join("review.html");
    std::fs::write(&p, html)?;
    Ok(p)
}
