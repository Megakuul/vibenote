#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::infinite_loop,
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects
)]
//! Blob store crash safety (M6.1, ADR 0005, R-DATA-001): a writer killed at any moment never
//! leaves a blob file whose content does not match its name; leftovers are temp files, which
//! garbage collection removes.

use std::collections::HashSet;
use vibenote_storage::BlobStore;
use vibenote_storage::blobs::{hash, is_hash};

const CHILD: &str = "VIBENOTE_BLOB_WRITER_DIR";

/// Child mode: write distinct 1–2 MB blobs until killed.
#[test]
fn blob_writer_child() {
    let Some(dir) = std::env::var_os(CHILD) else { return };
    let s = BlobStore::open(std::path::Path::new(&dir)).unwrap();
    for k in 0u64..u64::MAX {
        let len = 1_000_000 + (k as usize % 7) * 150_000;
        let bytes: Vec<u8> = (0..len).map(|i| (i as u64 ^ k.wrapping_mul(0x9e37)) as u8).collect();
        s.put(&bytes).unwrap();
    }
}

#[test]
fn killed_writers_never_leave_damaged_blobs() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("blobs");
    for round in 0..12u64 {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "blob_writer_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40 + round * 23));
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();
    }
    let mut blobs = 0;
    let mut temps = 0;
    for sub in std::fs::read_dir(&dir).unwrap() {
        for f in std::fs::read_dir(sub.unwrap().path()).unwrap() {
            let p = f.unwrap().path();
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            if is_hash(&name) {
                assert_eq!(hash(&std::fs::read(&p).unwrap()), name, "damaged blob {name}");
                blobs += 1;
            } else {
                assert!(name.contains(".tmp-"), "unexpected file {name}");
                temps += 1;
            }
        }
    }
    assert!(blobs > 0, "the writer never finished a blob");
    let s = BlobStore::open(&dir).unwrap();
    assert_eq!(s.gc(&HashSet::new()).unwrap(), blobs);
    let left: usize = std::fs::read_dir(&dir)
        .unwrap()
        .map(|sub| std::fs::read_dir(sub.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(left, 0, "{temps} temp files and all blobs collected");
}
