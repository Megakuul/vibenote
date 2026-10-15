#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
//! The PDF worker end to end (M6.2, ADR 0004, R-DATA-004): pages, rendering, text, hostile
//! files, the sandbox, and restarts after a crash.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use vibenote_pdf::proto::Response;
use vibenote_pdf::{PdfClient, PdfError};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn client() -> PdfClient {
    PdfClient::new(Path::new(env!("CARGO_BIN_EXE_vibenote-pdfd")))
}

fn fixture(name: &str) -> Arc<[u8]> {
    Arc::from(std::fs::read(root().join("tests/fixtures/pdf").join(name)).unwrap())
}

#[test]
fn opens_renders_and_extracts_text() {
    let mut c = client();
    let pages = c.open("doc", fixture("three-pages.pdf")).unwrap();
    assert_eq!(pages.len(), 3);
    // A5 = 419.53 × 595.28 pt.
    assert!(
        (pages[0].width - 419.5).abs() < 1.0 && (pages[0].height - 595.3).abs() < 1.0,
        "{pages:?}"
    );
    let b = c.render("doc", 0, 620).unwrap();
    assert_eq!(b.width, 620);
    assert!((f64::from(b.height) - 620.0 * 595.28 / 419.53).abs() < 2.0);
    assert_eq!(b.rgba.len(), (b.width * b.height * 4) as usize);
    // Paper is white and opaque; the page has some ink.
    assert_eq!(&b.rgba[..4], &[255, 255, 255, 255]);
    assert!(b.rgba.chunks(4).any(|p| p[0] < 128));
    let t = c.text("doc", 1).unwrap();
    assert!(t.contains("Zebrafisch"), "{t}");
    assert!(matches!(c.render("doc", 3, 100), Err(PdfError::Document(_))));
    assert!(matches!(c.text("nope", 0), Err(PdfError::Document(_))));
}

#[test]
fn hostile_files_are_clean_errors() {
    let mut c = client();
    for f in ["corrupt.pdf", "truncated.pdf"] {
        assert!(matches!(c.open(f, fixture(f)), Err(PdfError::Document(_))), "{f}");
    }
    assert!(matches!(
        c.open("empty", Arc::from(Vec::new())),
        Err(PdfError::Document(_))
    ));
    // The worker survived and still works.
    assert_eq!(c.open("doc", fixture("three-pages.pdf")).unwrap().len(), 3);
}

#[test]
fn a_killed_worker_is_restarted_and_documents_reopen() {
    let mut c = client();
    c.open("doc", fixture("three-pages.pdf")).unwrap();
    let pid = c.worker_pid().unwrap();
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let b = c.render("doc", 2, 200).unwrap();
    assert_eq!(b.width, 200);
    assert_ne!(c.worker_pid(), Some(pid));
    assert_eq!(c.starts, 2);
}

#[test]
fn the_sandbox_blocks_network_programs_and_writes() {
    let exe = env!("CARGO_BIN_EXE_vibenote-pdfd");
    // The probe hook is enabled through the worker's environment: run it via a wrapper path.
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("pdfd-test");
    std::fs::write(&wrapper, format!("#!/bin/sh\nVIBENOTE_PDFD_TEST=1 exec {exe}\n")).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", wrapper.to_str().unwrap()])
        .status()
        .unwrap();
    let mut c = PdfClient::new(&wrapper);
    match c.probe().unwrap() {
        Response::Probe {
            network_blocked,
            exec_blocked,
            write_blocked,
            kill_blocked,
            openat2_blocked,
        } => assert!(
            network_blocked && exec_blocked && write_blocked && kill_blocked && openat2_blocked,
            "{network_blocked} {exec_blocked} {write_blocked} {kill_blocked} {openat2_blocked}"
        ),
        other => panic!("{other:?}"),
    }
}
