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

/// R-PLAT-003 (revised by the owner 2026-10-18 for the cloud backup, ADR 0012): networking
/// crates appear only below `vibenote-backup`; with backup off (the default) no backup thread
/// starts, even when the app passes the config file.
#[test]
fn r_plat_003_no_network() {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).unwrap();
    for bad in [
        "name = \"reqwest\"",
        "name = \"hyper\"",
        "name = \"curl\"",
        "name = \"tokio-rustls\"",
        "name = \"native-tls\"",
    ] {
        assert!(!lock.contains(bad), "networking crate in Cargo.lock: {bad}");
    }
    // Who depends on the HTTP client directly: only the backup crate.
    let mut users = vec![];
    for block in lock.split("[[package]]") {
        let name = block
            .lines()
            .find_map(|l| l.strip_prefix("name = \""))
            .map(|n| n.trim_end_matches('"').to_string());
        if let Some(deps) = block.split("dependencies = [").nth(1)
            && deps.split(']').next().unwrap().lines().any(|l| {
                let d = l.trim().trim_matches(|c| c == '"' || c == ',');
                d == "ureq" || d.starts_with("ureq ")
            })
        {
            users.push(name.unwrap());
        }
    }
    assert_eq!(users, ["vibenote-backup"]);
    // Backup handling on, backup off: no thread.
    let dir = tempfile::tempdir().unwrap();
    let mut e = vibenote_engine::Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    e.use_backup_config(&dir.path().join("backup.json"), &dir.path().join("state.json"))
        .unwrap();
    assert!(e.backup_tick(std::time::SystemTime::now()).is_empty());
    assert!(!e.backup_active());
    let threads: Vec<String> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .collect();
    assert!(!threads.iter().any(|t| t.trim() == "vibenote-backup"), "{threads:?}");
}

/// R-PLAT-004 (owner 2026-10-22: "vibenote has no desktop icon"): the entry and every icon size
/// are installed for the user; a system-wide or foreign entry is left alone; a moved launcher
/// updates our entry.
#[test]
fn r_plat_004_desktop_entry_and_icons() {
    use vibenote_engine::desktop::{Dirs, Outcome, install};
    let t = tempfile::tempdir().unwrap();
    let dirs = Dirs {
        data_home: t.path().join("home"),
        data_dirs: vec![t.path().join("usr")],
    };
    let entry = dirs.data_home.join("applications/vibenote.desktop");
    let r = install(&dirs, std::path::Path::new("/opt/v/vibenote.AppImage")).unwrap();
    assert_eq!(r, Outcome::Installed(entry.clone()));
    let text = std::fs::read_to_string(&entry).unwrap();
    for line in [
        "Exec=/opt/v/vibenote.AppImage %U",
        "Icon=vibenote",
        "StartupWMClass=vibenote",
        "Type=Application",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in {text}");
    }
    for s in [16, 24, 32, 48, 64, 128, 256, 512] {
        let p = dirs.data_home.join(format!("icons/hicolor/{s}x{s}/apps/vibenote.png"));
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "{}", p.display());
        // PNG IHDR width.
        assert_eq!(u32::from_be_bytes(b[16..20].try_into().unwrap()), s);
    }
    assert!(
        dirs.data_home
            .join("icons/hicolor/scalable/apps/vibenote.svg")
            .is_file()
    );
    // Again: unchanged; moved: updated.
    let r = install(&dirs, std::path::Path::new("/opt/v/vibenote.AppImage")).unwrap();
    assert_eq!(r, Outcome::Unchanged(entry.clone()));
    let r = install(&dirs, std::path::Path::new("/home/me/Apps/vibenote 2.AppImage")).unwrap();
    assert_eq!(r, Outcome::Updated(entry.clone()));
    assert!(
        std::fs::read_to_string(&entry)
            .unwrap()
            .contains("Exec=\"/home/me/Apps/vibenote 2.AppImage\" %U")
    );
    // The user's own entry is not touched.
    std::fs::write(&entry, "[Desktop Entry]\nName=mine\n").unwrap();
    let r = install(&dirs, std::path::Path::new("/x")).unwrap();
    assert_eq!(r, Outcome::Foreign(entry.clone()));
    assert_eq!(std::fs::read_to_string(&entry).unwrap(), "[Desktop Entry]\nName=mine\n");
    // A system-wide entry (a package): nothing is written.
    let t2 = tempfile::tempdir().unwrap();
    let sys = t2.path().join("usr/applications/vibenote.desktop");
    std::fs::create_dir_all(sys.parent().unwrap()).unwrap();
    std::fs::write(&sys, "[Desktop Entry]\n").unwrap();
    let d2 = Dirs {
        data_home: t2.path().join("home"),
        data_dirs: vec![t2.path().join("usr")],
    };
    assert_eq!(
        install(&d2, std::path::Path::new("/x")).unwrap(),
        Outcome::SystemEntry(sys)
    );
    assert!(!d2.data_home.exists());
}
