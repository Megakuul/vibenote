//! Layer check: internal dependency directions + no UI/windowing crates below `ui`.
//!
//! Layering (AGENTS.md §2 Phase 3 item 1, ADRs 0001–0009):
//!   geom → model → {storage, ink, pdf, audio} → hwr → render → engine → control → ui → apps

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// Allowed internal (workspace) dependencies per crate. Anything not listed is an error.
const ALLOWED: &[(&str, &[&str])] = &[
    ("vibenote-geom", &[]),
    ("vibenote-model", &["vibenote-geom"]),
    ("vibenote-storage", &["vibenote-geom", "vibenote-model"]),
    ("vibenote-ink", &["vibenote-geom", "vibenote-model"]),
    ("vibenote-pdf", &["vibenote-geom", "vibenote-model"]),
    ("vibenote-audio", &["vibenote-model"]),
    ("vibenote-backup", &["vibenote-storage", "vibenote-model"]),
    ("vibenote-hwr", &["vibenote-geom", "vibenote-model", "vibenote-ink"]),
    ("vibenote-render", &["vibenote-geom", "vibenote-model", "vibenote-ink"]),
    (
        "vibenote-engine",
        &[
            "vibenote-geom",
            "vibenote-model",
            "vibenote-storage",
            "vibenote-backup",
            "vibenote-ink",
            "vibenote-hwr",
            "vibenote-pdf",
            "vibenote-audio",
            "vibenote-render",
        ],
    ),
    ("vibenote-control", &["vibenote-engine"]),
    (
        "vibenote-ui",
        &[
            "vibenote-engine",
            "vibenote-render",
            "vibenote-control",
            "vibenote-geom",
            "vibenote-model",
            "vibenote-ink",
        ],
    ),
    ("vibenote", &["vibenote-ui", "vibenote-engine"]),
    (
        "vibenote-cli",
        &[
            "vibenote-engine",
            "vibenote-control",
            "vibenote-render",
            "vibenote-ink",
            "vibenote-model",
            "vibenote-hwr",
            "vibenote-backup",
        ],
    ),
    ("vibenote-pdfd", &["vibenote-pdf"]),
    ("xtask", &[]),
    ("simtab", &["vibenote-ink"]),
    (
        "vibenote-acceptance",
        &[
            "vibenote-engine",
            "vibenote-model",
            "vibenote-storage",
            "vibenote-render",
            "vibenote-ink",
            "vibenote-control",
            "vibenote-backup",
        ],
    ),
];

/// Crates that may only appear in the dependency closure of `vibenote-ui` and apps.
const UI_ONLY: &[&str] = &[
    "wayland-client",
    "wayland-backend",
    "wayland-protocols",
    "wayland-sys",
    "wayland-cursor",
    "smithay-client-toolkit",
    "winit",
    "egui",
    "egui-wgpu",
    "epaint",
    "gtk4",
    "gdk4",
    "xkbcommon",
    "accesskit",
    "accesskit_unix",
];

/// Crates allowed to depend on UI-only crates (transitively).
const UI_LAYER: &[&str] = &["vibenote-ui", "vibenote"];

/// JSON field access without indexing (`Value::Null` when missing).
fn at<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

pub fn check() -> Result<()> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let mut errors = check_metadata(&meta)?;
    errors.extend(check_unsafe(std::path::Path::new("."))?);
    errors.extend(check_manifests(std::path::Path::new("."))?);
    if errors.is_empty() {
        eprintln!("layers: ok");
        Ok(())
    } else {
        for e in &errors {
            eprintln!("layers: {e}");
        }
        bail!("{} layering violation(s)", errors.len())
    }
}

pub fn check_metadata(meta: &Value) -> Result<Vec<String>> {
    let packages = at(meta, "packages").as_array().ok_or_else(|| anyhow!("no packages"))?;
    let name_of: BTreeMap<&str, &str> = packages
        .iter()
        .filter_map(|p| Some((at(p, "id").as_str()?, at(p, "name").as_str()?)))
        .collect();
    let members: BTreeSet<&str> = at(meta, "workspace_members")
        .as_array()
        .ok_or_else(|| anyhow!("no workspace_members"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // normal (non-dev, non-build) dependency edges
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in at(at(meta, "resolve"), "nodes")
        .as_array()
        .ok_or_else(|| anyhow!("no resolve"))?
    {
        let Some(id) = at(node, "id").as_str() else { continue };
        let deps = at(node, "deps").as_array().map(Vec::as_slice).unwrap_or(&[]);
        let normal: Vec<&str> = deps
            .iter()
            .filter(|d| {
                at(d, "dep_kinds")
                    .as_array()
                    .is_some_and(|ks| ks.iter().any(|k| at(k, "kind").is_null()))
            })
            .filter_map(|d| at(d, "pkg").as_str())
            .collect();
        edges.insert(id, normal);
    }
    let allowed: BTreeMap<&str, &[&str]> = ALLOWED.iter().copied().collect();
    let mut errors = vec![];
    for &m in &members {
        let name = name_of.get(m).copied().unwrap_or(m);
        let Some(ok) = allowed.get(name) else {
            errors.push(format!(
                "{name}: not in the layer table (add it to xtask/src/layers.rs)"
            ));
            continue;
        };
        // 1. direct internal deps must be allowed
        for d in edges.get(m).map(Vec::as_slice).unwrap_or(&[]) {
            if members.contains(d) {
                let dn = name_of.get(d).copied().unwrap_or(d);
                if !ok.contains(&dn) {
                    errors.push(format!(
                        "{name} → {dn}: internal dependency not allowed by the layering"
                    ));
                }
            }
        }
        // 2. no UI-only crates in the transitive closure below the UI layer
        if !UI_LAYER.contains(&name) {
            let mut seen = BTreeSet::new();
            let mut stack = vec![m];
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                let nn = name_of.get(n).copied().unwrap_or(n);
                if n != m && UI_ONLY.contains(&nn) {
                    errors.push(format!("{name} depends (transitively) on UI-only crate `{nn}`"));
                }
                stack.extend(edges.get(n).map(Vec::as_slice).unwrap_or(&[]));
            }
        }
    }
    Ok(errors)
}

/// The only file allowed to contain `unsafe` (ADR 0010), with exactly this many uses.
const UNSAFE_ALLOWED: (&str, usize) = ("crates/ui/src/gpu_surface.rs", 1);
const UNSAFE_ROOTS: &[&str] = &["crates", "apps", "tools", "xtask", "tests"];

/// `unsafe` keyword uses (blocks, fns, impls, traits, externs, `#[unsafe(..)]` attributes, also
/// inside `macro_rules!`) and `unsafe_code` lint relaxations (`allow`/`expect`/`warn`, also via
/// `cfg_attr` or spread over lines) in Rust source. Comments (incl. nested block comments),
/// string, raw-string, byte and char literals are skipped by a small lexer.
#[derive(Debug, Default, PartialEq)]
pub struct UnsafeUses {
    pub code: usize,
    pub allows: usize,
}

/// Identifiers of `src` outside comments and literals, in order.
#[allow(clippy::arithmetic_side_effects)] // indices stay within a few bytes past the source length
fn idents(src: &str) -> Vec<&str> {
    let b = src.as_bytes();
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = at(i);
        if c == b'/' && at(i + 1) == b'/' {
            while i < b.len() && at(i) != b'\n' {
                i += 1;
            }
        } else if c == b'/' && at(i + 1) == b'*' {
            let mut depth = 0usize;
            while i < b.len() {
                if at(i) == b'/' && at(i + 1) == b'*' {
                    depth += 1;
                    i += 2;
                } else if at(i) == b'*' && at(i + 1) == b'/' {
                    depth = depth.saturating_sub(1);
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'r' && (at(i + 1) == b'"' || (at(i + 1) == b'#' && matches!(at(i + 2), b'"' | b'#'))) {
            // Raw string r"..." / r#"..."#.
            let mut j = i + 1;
            let mut hashes = 0usize;
            while at(j) == b'#' {
                hashes += 1;
                j += 1;
            }
            j += 1; // opening quote
            loop {
                if j >= b.len() {
                    break;
                }
                if at(j) == b'"' && (1..=hashes).all(|k| at(j + k) == b'#') {
                    j += 1 + hashes;
                    break;
                }
                j += 1;
            }
            i = j;
        } else if c == b'"' {
            i += 1;
            while i < b.len() && at(i) != b'"' {
                i += if at(i) == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c == b'\'' {
            // Char literal ('x', '\n', '\u{..}', '"') or lifetime ('a).
            if at(i + 1) == b'\\' {
                i += 2;
                while i < b.len() && at(i) != b'\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == b'\'' {
                i += 3;
            } else {
                // Lifetime, or a multi-byte UTF-8 char literal: skip the quote only.
                let ch_len = src
                    .get(i + 1..)
                    .and_then(|r| r.chars().next())
                    .map_or(1, char::len_utf8);
                if at(i + 1 + ch_len) == b'\'' {
                    i += 2 + ch_len;
                } else {
                    i += 1;
                }
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let s0 = i;
            while i < b.len() && (at(i).is_ascii_alphanumeric() || at(i) == b'_') {
                i += 1;
            }
            // `b"..."` / `br"..."` prefixes: the quote is handled next round.
            if let Some(id) = src.get(s0..i) {
                out.push(id);
            }
        } else {
            i += 1;
        }
    }
    out
}

pub fn unsafe_uses(src: &str) -> UnsafeUses {
    let mut u = UnsafeUses::default();
    let ids = idents(src);
    for (k, id) in ids.iter().enumerate() {
        match *id {
            "unsafe" => u.code = u.code.saturating_add(1),
            "unsafe_code" => {
                // The nearest lint level before the lint name decides.
                let level = ids
                    .get(..k)
                    .unwrap_or(&[])
                    .iter()
                    .rev()
                    .find(|t| matches!(**t, "allow" | "expect" | "warn" | "deny" | "forbid"));
                if matches!(level, Some(&"allow" | &"expect" | &"warn")) {
                    u.allows = u.allows.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    u
}

/// Every workspace member must inherit the workspace lints (`unsafe_code = "deny"`) and may not
/// relax `unsafe_code` in its manifest.
pub fn check_manifests(root: &std::path::Path) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let ws = std::fs::read_to_string(root.join("Cargo.toml"))?;
    if !ws.lines().any(|l| l.replace(' ', "") == "unsafe_code=\"deny\"") {
        errors.push("Cargo.toml: [workspace.lints.rust] must set unsafe_code = \"deny\"".into());
    }
    let mut stack: Vec<std::path::PathBuf> = UNSAFE_ROOTS.iter().map(|r| root.join(r)).collect();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd {
            let p = e?.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    stack.push(p);
                }
                continue;
            }
            if name != "Cargo.toml" {
                continue;
            }
            let m = std::fs::read_to_string(&p)?;
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            let lines: Vec<String> = m
                .lines()
                .map(|l| l.split('#').next().unwrap_or("").replace(' ', ""))
                .collect();
            let inherits = lines
                .iter()
                .position(|l| l == "[lints]")
                .and_then(|i| lines.get(i.saturating_add(1)))
                .is_some_and(|l| l == "workspace=true");
            if !inherits {
                errors.push(format!(
                    "{rel}: needs `[lints]` + `workspace = true` (unsafe_code stays denied)"
                ));
            }
            if m.contains("unsafe_code") {
                errors.push(format!("{rel}: may not configure unsafe_code (ADR 0010)"));
            }
        }
    }
    Ok(errors)
}

/// Fails on `unsafe` outside [`UNSAFE_ALLOWED`] (AGENTS.md §3, ADR 0010). Spikes are excluded.
pub fn check_unsafe(root: &std::path::Path) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = UNSAFE_ROOTS.iter().map(|r| root.join(r)).collect();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd {
            let p = e?.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    stack.push(p);
                }
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let src = std::fs::read_to_string(&p)?;
            let u = unsafe_uses(&src);
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if rel == UNSAFE_ALLOWED.0 {
                if u.code != UNSAFE_ALLOWED.1 || u.allows > UNSAFE_ALLOWED.1 {
                    errors.push(format!(
                        "{rel}: expected exactly {} unsafe use(s) (ADR 0010), found {u:?}",
                        UNSAFE_ALLOWED.1
                    ));
                }
            } else if u != UnsafeUses::default() {
                errors.push(format!(
                    "{rel}: `unsafe` is only allowed in {} (ADR 0010)",
                    UNSAFE_ALLOWED.0
                ));
            }
        }
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(edges: &[(&str, &[&str])], members: &[&str]) -> Value {
        let pkgs: BTreeSet<&str> = edges
            .iter()
            .flat_map(|(a, bs)| std::iter::once(*a).chain(bs.iter().copied()))
            .collect();
        json!({
            "packages": pkgs.iter().map(|p| json!({"id": p, "name": p})).collect::<Vec<_>>(),
            "workspace_members": members,
            "resolve": {"nodes": edges.iter().map(|(a, bs)| json!({
                "id": a, "deps": bs.iter().map(|b| json!({"pkg": b, "dep_kinds": [{"kind": null}]})).collect::<Vec<_>>()
            })).collect::<Vec<_>>()}
        })
    }

    #[test]
    fn unsafe_detection() {
        let n = |c, a| UnsafeUses { code: c, allows: a };
        assert_eq!(unsafe_uses("let x = unsafe { f() };"), n(1, 0));
        assert_eq!(unsafe_uses("unsafe fn g() {}\nunsafe impl Send for X {}"), n(2, 0));
        assert_eq!(unsafe_uses("// unsafe { not code }\nlet s = \"unsafe {\";"), n(0, 0));
        assert_eq!(unsafe_uses("//! no `unsafe` via rustix"), n(0, 0));
        assert_eq!(unsafe_uses("#[allow(unsafe_code)]\nfn f() {}"), n(0, 1));
        assert_eq!(unsafe_uses("#![deny(unsafe_code)]"), n(0, 0));
        // M1 review: bypasses of the old line heuristic.
        assert_eq!(unsafe_uses("#[expect(unsafe_code)]"), n(0, 1));
        assert_eq!(unsafe_uses("#[warn(unsafe_code)]"), n(0, 1));
        assert_eq!(unsafe_uses("#[allow(\n    unsafe_code\n)]"), n(0, 1));
        assert_eq!(unsafe_uses("#[cfg_attr(test, allow(dead_code, unsafe_code))]"), n(0, 1));
        assert_eq!(unsafe_uses("let q = '\"'; let x = unsafe\n{ f() };"), n(1, 0));
        assert_eq!(
            unsafe_uses("let s = r#\"unsafe { \" \"#; unsafe extern \"C\" {}"),
            n(1, 0)
        );
        assert_eq!(
            unsafe_uses("/* outer /* unsafe {} */ still comment */ fn f() {}"),
            n(0, 0)
        );
        assert_eq!(unsafe_uses("macro_rules! m { () => { unsafe { g() } } }"), n(1, 0));
        assert_eq!(unsafe_uses("#[unsafe(no_mangle)] pub fn f() {}"), n(1, 0));
        assert_eq!(
            unsafe_uses("fn f<'a>(x: &'a str) -> char { let s = \"unsafe\"; '\\u{1F600}' }"),
            n(0, 0)
        );
        assert_eq!(unsafe_uses("let c = 'ü'; let u = create_surface_unsafe();"), n(0, 0));
    }

    #[test]
    fn detects_upward_internal_dependency() {
        let m = meta(
            &[("vibenote-geom", &["vibenote-engine"]), ("vibenote-engine", &[])],
            &["vibenote-geom", "vibenote-engine"],
        );
        let e = check_metadata(&m).unwrap();
        assert!(e.iter().any(|s| s.contains("vibenote-geom → vibenote-engine")), "{e:?}");
    }

    #[test]
    fn detects_transitive_ui_crate_below_ui() {
        let m = meta(
            &[("vibenote-render", &["foo"]), ("foo", &["egui"]), ("egui", &[])],
            &["vibenote-render"],
        );
        let e = check_metadata(&m).unwrap();
        assert!(e.iter().any(|s| s.contains("UI-only crate `egui`")), "{e:?}");
    }

    #[test]
    fn ui_layer_may_use_ui_crates() {
        let m = meta(&[("vibenote-ui", &["egui"]), ("egui", &[])], &["vibenote-ui"]);
        assert!(check_metadata(&m).unwrap().is_empty());
    }

    #[test]
    fn unknown_member_is_reported() {
        let m = meta(&[("vibenote-new", &[])], &["vibenote-new"]);
        assert!(!check_metadata(&m).unwrap().is_empty());
    }
}
