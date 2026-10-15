//! `cargo xtask ci` — the single gate (AGENTS.md §2 Phase 3 item 11):
//! fmt, clippy -D warnings, layers, tests (incl. goldens + crash), licences, parity (acceptance
//! suite + floor, incl. performance budgets of passing requirements), protected-file report.
//! `cargo xtask bench` runs the slow extras (crash ×1000, e2e latency once the app exists).

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn step(name: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<bool> {
    eprintln!("ci: ── {name}");
    let mut c = Command::new(cargo());
    c.args(args).current_dir(root());
    for (k, v) in envs {
        c.env(k, v);
    }
    let ok = c.status()?.success();
    eprintln!("ci: {} {name}", if ok { "✓" } else { "✗" });
    Ok(ok)
}

pub fn ci() -> Result<()> {
    let mut failed: Vec<&str> = vec![];
    let steps: &[(&str, &[&str])] = &[
        ("fmt", &["fmt", "--all", "--check"]),
        (
            "clippy",
            &["clippy", "-q", "--workspace", "--all-targets", "--", "-D", "warnings"],
        ),
        ("layers", &["run", "-q", "-p", "xtask", "--", "layers"]),
        // The PDF worker binary: engine and acceptance tests start it (ADR 0004).
        ("worker", &["build", "-q", "-p", "vibenote-pdfd"]),
        (
            "tests",
            &["test", "-q", "--workspace", "--exclude", "vibenote-acceptance"],
        ),
        ("licences", &["deny", "check", "licenses", "bans", "sources"]),
        ("parity", &["run", "-q", "-p", "xtask", "--", "parity"]),
    ];
    for (name, args) in steps {
        if !step(name, args, &[])? {
            failed.push(name);
        }
    }
    let protected = crate::protect::report()?;
    if !protected.is_empty() {
        eprintln!("ci: ⚠ PROTECTED FILES CHANGED since `cargo xtask protect --write` (owner must review):");
        for p in &protected {
            eprintln!("ci:   {p}");
        }
        // Protected tests may not change without the owner (AGENTS.md §3): fail, so the change
        // cannot slip through. The owner accepts a change by re-recording the manifest.
        eprintln!("ci: ✗ protected");
        failed.push("protected");
    }
    if failed.is_empty() {
        eprintln!("ci: ALL PASSED");
        Ok(())
    } else {
        bail!("ci failed: {failed:?}")
    }
}

pub fn bench(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("all") => {
            let a = step(
                "crash ×1000",
                &["test", "-q", "--release", "-p", "vibenote-cli", "--test", "crash"],
                &[("VIBENOTE_CRASH_ITERS", "1000")],
            )?;
            let b = step(
                "acceptance (budgets)",
                &[
                    "test",
                    "-q",
                    "--release",
                    "-p",
                    "vibenote-acceptance",
                    "--test",
                    "perf",
                    "--no-fail-fast",
                    "--",
                    "--test-threads=1",
                ],
                &[],
            )?;
            if !(a && b) {
                bail!("bench: failures (see above)");
            }
            Ok(())
        }
        Some("app-smoke") => crate::e2e::app_smoke(),
        Some("latency") => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some("--analyze"), Some(dir)) => crate::e2e::report_latency(std::path::Path::new(dir), "latency"),
            (Some("--reference"), _) => crate::e2e::latency(true),
            _ => crate::e2e::latency(false),
        },
        Some("startup") => crate::e2e::startup(),
        Some(o) => bail!("unknown bench `{o}` (all|latency|app-smoke|startup)"),
    }
}

pub fn prettify_gallery() -> Result<()> {
    // Renders before/after prettify for the eval blocks into an HTML page for owner review.
    let out = Command::new(cargo())
        .args([
            "run",
            "-q",
            "-p",
            "vibenote-cli",
            "--",
            "run",
            "hwr.prettify_block",
            "{}",
        ])
        .current_dir(root())
        .output()?;
    if !out.status.success() {
        bail!("prettify-gallery: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
