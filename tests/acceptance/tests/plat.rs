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
//! R-PLAT-001..003 (SPEC §1).

use vibenote_acceptance::*;

fn sources() -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root().join("crates"), root().join("apps")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// R-PLAT-001: no code path exists only for another platform.
#[test]
fn r_plat_001_linux_only() {
    for p in sources() {
        let s = std::fs::read_to_string(&p).unwrap();
        for bad in [
            "target_os = \"windows\"",
            "target_os = \"macos\"",
            "target_os = \"android\"",
            "target_arch = \"wasm32\"",
        ] {
            assert!(!s.contains(bad), "{}: {bad}", p.display());
        }
    }
}

/// R-PLAT-002: the app opens a window on Wayland and accepts pen input (needs a session).
#[test]
fn r_plat_002_wayland_app_runs() {
    let report = root().join("target/e2e-app-smoke.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap_or_else(|_| {
        panic!(
            "no app smoke report at {} — run `cargo xtask bench app-smoke`",
            report.display()
        )
    }))
    .unwrap();
    assert_eq!(v["window_opened"], true);
    assert_eq!(v["pen_strokes_received"], 1);
}

/// R-PLAT-003: no networking crates in the product dependency tree.
#[test]
fn r_plat_003_no_network() {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).unwrap();
    for bad in [
        "name = \"reqwest\"",
        "name = \"hyper\"",
        "name = \"ureq\"",
        "name = \"curl\"",
        "name = \"tokio-rustls\"",
        "name = \"native-tls\"",
    ] {
        assert!(!lock.contains(bad), "networking crate in Cargo.lock: {bad}");
    }
}
