//! Content-addressed blob store (ADR 0005, R-DATA-001): large binary assets (PDFs, images,
//! audio) live in files `<dir>/<h[0..2]>/<blake3 hex>` next to the database. A blob is durable
//! (temp file → fsync → rename → fsync of the directory) before any transaction that refers to
//! it commits, so a crash leaves at most an unreferenced blob, which garbage collection removes.
//! Hashes are checked on read: a damaged file is an error, never wrong content.
//!
//! An in-memory store (no directory) serves engines without a workspace file.

use crate::StorageError;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Largest blob accepted (bytes).
pub const MAX_BLOB: u64 = 2 << 30;

/// A blake3 hash in lower-case hex (64 characters).
pub fn is_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[derive(Debug, Default)]
pub struct BlobStore {
    dir: Option<PathBuf>,
    mem: Mutex<HashMap<String, Arc<[u8]>>>,
}

fn io(what: &str, e: &std::io::Error) -> StorageError {
    StorageError::Corrupt(format!("blob store: {what}: {e}"))
}

impl BlobStore {
    /// A store in `dir` (created if missing).
    pub fn open(dir: &Path) -> Result<BlobStore, StorageError> {
        std::fs::create_dir_all(dir).map_err(|e| io("creating the directory", &e))?;
        Ok(BlobStore {
            dir: Some(dir.to_path_buf()),
            mem: Mutex::default(),
        })
    }

    /// A store that keeps blobs in memory only.
    pub fn in_memory() -> BlobStore {
        BlobStore::default()
    }

    /// The directory of a workspace database's blobs: `<db>.blobs`.
    pub fn dir_for(db: &Path) -> PathBuf {
        let mut s = db.as_os_str().to_os_string();
        s.push(".blobs");
        PathBuf::from(s)
    }

    fn path(&self, h: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        Some(dir.join(h.get(..2)?).join(h))
    }

    /// Stores `bytes` durably; returns their hash. Storing existing content is a no-op.
    pub fn put(&self, bytes: &[u8]) -> Result<String, StorageError> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BLOB {
            return Err(StorageError::Corrupt(format!("blob larger than {MAX_BLOB} bytes")));
        }
        let h = hash(bytes);
        let Some(path) = self.path(&h) else {
            self.mem
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(h.clone())
                .or_insert_with(|| Arc::from(bytes));
            return Ok(h);
        };
        if path.exists() {
            return Ok(h);
        }
        let parent = path
            .parent()
            .ok_or_else(|| StorageError::Corrupt("blob path has no directory".into()))?;
        std::fs::create_dir_all(parent).map_err(|e| io("creating a directory", &e))?;
        let tmp = parent.join(format!(".{h}.tmp-{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            std::fs::File::open(parent)?.sync_all()?;
            if let Some(top) = parent.parent() {
                std::fs::File::open(top)?.sync_all()?;
            }
            Ok(())
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io("writing", &e)
        })?;
        Ok(h)
    }

    /// The blob's bytes, checked against its hash.
    pub fn get(&self, h: &str) -> Result<Arc<[u8]>, StorageError> {
        if !is_hash(h) {
            return Err(StorageError::Corrupt(format!("not a blob hash: {h:?}")));
        }
        let Some(path) = self.path(h) else {
            return self
                .mem
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(h)
                .cloned()
                .ok_or_else(|| StorageError::Corrupt(format!("blob {h} missing")));
        };
        let len = std::fs::metadata(&path)
            .map_err(|e| io(&format!("blob {h}"), &e))?
            .len();
        if len > MAX_BLOB {
            return Err(StorageError::Corrupt(format!("blob {h} too large")));
        }
        let bytes = std::fs::read(&path).map_err(|e| io(&format!("reading blob {h}"), &e))?;
        if hash(&bytes) != h {
            return Err(StorageError::Corrupt(format!("blob {h} is damaged")));
        }
        Ok(Arc::from(bytes))
    }

    pub fn contains(&self, h: &str) -> bool {
        if !is_hash(h) {
            return false;
        }
        match self.path(h) {
            Some(p) => p.exists(),
            None => self.mem.lock().unwrap_or_else(PoisonError::into_inner).contains_key(h),
        }
    }

    /// Removes every blob not in `keep` (and leftover temp files); returns how many blobs
    /// went. The caller makes sure every reference is durable first (objects and history).
    pub fn gc(&self, keep: &HashSet<String>) -> Result<usize, StorageError> {
        let Some(dir) = &self.dir else {
            let mut m = self.mem.lock().unwrap_or_else(PoisonError::into_inner);
            let before = m.len();
            m.retain(|h, _| keep.contains(h));
            return Ok(before.saturating_sub(m.len()));
        };
        let mut removed = 0usize;
        for sub in std::fs::read_dir(dir).map_err(|e| io("listing", &e))? {
            let sub = sub.map_err(|e| io("listing", &e))?.path();
            if !sub.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&sub).map_err(|e| io("listing", &e))? {
                let p = f.map_err(|e| io("listing", &e))?.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
                let tmp = name.starts_with('.') && name.contains(".tmp-");
                if tmp || (is_hash(&name) && !keep.contains(&name)) {
                    std::fs::remove_file(&p).map_err(|e| io("removing", &e))?;
                    if !tmp {
                        removed = removed.saturating_add(1);
                    }
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn put_get_dedup_and_damage() {
        let d = tempfile::tempdir().unwrap();
        let s = BlobStore::open(&d.path().join("b")).unwrap();
        let h = s.put(b"hello").unwrap();
        assert_eq!(h, hash(b"hello"));
        assert_eq!(s.put(b"hello").unwrap(), h);
        assert_eq!(&*s.get(&h).unwrap(), b"hello");
        assert!(s.get("../../etc/passwd").is_err());
        assert!(s.get(&"0".repeat(64)).is_err());
        // A damaged file is an error, not wrong content.
        let p = d.path().join("b").join(&h[..2]).join(&h);
        std::fs::write(&p, b"jello").unwrap();
        assert!(s.get(&h).is_err());
    }

    #[test]
    fn gc_keeps_referenced_blobs_and_removes_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let s = BlobStore::open(&d.path().join("b")).unwrap();
        let a = s.put(b"a").unwrap();
        let b = s.put(b"b").unwrap();
        std::fs::write(d.path().join("b").join(&a[..2]).join(".x.tmp-1"), b"partial").unwrap();
        let keep: HashSet<String> = [a.clone()].into_iter().collect();
        assert_eq!(s.gc(&keep).unwrap(), 1);
        assert!(s.contains(&a) && !s.contains(&b));
        let m = BlobStore::in_memory();
        let x = m.put(b"x").unwrap();
        m.put(b"y").unwrap();
        assert_eq!(m.gc(&[x.clone()].into_iter().collect()).unwrap(), 1);
        assert_eq!(&*m.get(&x).unwrap(), b"x");
    }
}
