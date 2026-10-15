//! Recovery file (M2 review M1, R-DATA-001): when the storage thread cannot commit at shutdown
//! (disk full, I/O errors), the changes that are not durable yet go to a small file instead of
//! being lost, and the next open applies them.
//!
//! Layout: `b"VNR1"` · next_id varint · settings (0 | 1 · len · JSON) · ops (len · step record,
//! see `history.rs`) · history op count varint · per history op: tag u8, scope (len · UTF-8),
//! stack u8, and for `Push` label (len · UTF-8) + ops (len · step record) · FNV-1a 64 of all
//! preceding bytes (LE). Written to a temporary file, synced, then renamed. Every read is
//! bounds-checked; a torn or corrupt file is an error, never a panic.

use crate::StorageError;
use crate::codec::{Reader, put_varint};
use crate::history::{HistoryOp, Stack, StepRecord, decode_ops, encode_ops};
use std::io::Write;
use std::path::Path;
use vibenote_model::{Op, Settings};

const MAGIC: &[u8; 4] = b"VNR1";
/// History ops in one file (a hostile count must not allocate).
const MAX_HIST: u64 = 1 << 24;

/// Changes that were not committed, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recovery {
    pub ops: Vec<Op>,
    pub hist: Vec<HistoryOp>,
    pub next_id: u64,
    pub settings: Option<Settings>,
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, u64::try_from(b.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(b);
}

fn stack_code(s: Stack) -> u8 {
    match s {
        Stack::Undo => 0,
        Stack::Redo => 1,
    }
}

fn corrupt(m: &str) -> StorageError {
    StorageError::Corrupt(format!("recovery file: {m}"))
}

pub fn encode_recovery(r: &Recovery) -> Result<Vec<u8>, StorageError> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(MAGIC);
    put_varint(&mut out, r.next_id);
    match &r.settings {
        None => out.push(0),
        Some(s) => {
            out.push(1);
            put_bytes(&mut out, &serde_json::to_vec(s).map_err(|e| corrupt(&e.to_string()))?);
        }
    }
    put_bytes(&mut out, &encode_ops(&r.ops)?);
    put_varint(&mut out, u64::try_from(r.hist.len()).unwrap_or(u64::MAX));
    for h in &r.hist {
        let (tag, scope, stack) = match h {
            HistoryOp::Push { scope, stack, .. } => (0u8, scope, *stack),
            HistoryOp::Pop { scope, stack } => (1, scope, *stack),
            HistoryOp::Clear { scope, stack } => (2, scope, *stack),
            HistoryOp::DropOldest { scope, stack } => (3, scope, *stack),
        };
        out.push(tag);
        put_bytes(&mut out, scope.as_bytes());
        out.push(stack_code(stack));
        if let HistoryOp::Push { step, .. } = h {
            put_bytes(&mut out, step.label.as_bytes());
            put_bytes(&mut out, &encode_ops(&step.ops)?);
        }
    }
    let sum = fnv(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

fn string(r: &mut Reader<'_>) -> Result<String, StorageError> {
    let n = r.varint()?;
    String::from_utf8(r.take(n)?.to_vec()).map_err(|_| corrupt("bad text"))
}

pub fn decode_recovery(b: &[u8]) -> Result<Recovery, StorageError> {
    let body_len = b.len().checked_sub(8).ok_or_else(|| corrupt("too short"))?;
    let (body, sum) = b.split_at(body_len);
    let want = u64::from_le_bytes(sum.try_into().map_err(|_| corrupt("checksum"))?);
    if fnv(body) != want {
        return Err(corrupt("checksum mismatch (torn or damaged)"));
    }
    let mut r = Reader::new(body);
    if r.bytes::<4>()? != *MAGIC {
        return Err(corrupt("bad magic"));
    }
    let next_id = r.varint()?;
    let settings = match r.byte()? {
        0 => None,
        1 => {
            let n = r.varint()?;
            Some(serde_json::from_slice(r.take(n)?).map_err(|e| corrupt(&e.to_string()))?)
        }
        _ => return Err(corrupt("bad settings flag")),
    };
    let n = r.varint()?;
    let ops = decode_ops(r.take(n)?)?;
    let count = r.varint()?;
    if count > MAX_HIST {
        return Err(corrupt("too many history ops"));
    }
    let mut hist = Vec::new();
    for _ in 0..count {
        let tag = r.byte()?;
        let scope = string(&mut r)?;
        let stack = match r.byte()? {
            0 => Stack::Undo,
            1 => Stack::Redo,
            _ => return Err(corrupt("bad stack")),
        };
        hist.push(match tag {
            0 => {
                let label = string(&mut r)?;
                let n = r.varint()?;
                let ops = decode_ops(r.take(n)?)?;
                HistoryOp::Push {
                    scope,
                    stack,
                    step: StepRecord { label, ops },
                }
            }
            1 => HistoryOp::Pop { scope, stack },
            2 => HistoryOp::Clear { scope, stack },
            3 => HistoryOp::DropOldest { scope, stack },
            _ => return Err(corrupt("bad history tag")),
        });
    }
    if !r.at_end() {
        return Err(corrupt("trailing bytes"));
    }
    Ok(Recovery {
        ops,
        hist,
        next_id,
        settings,
    })
}

/// Writes `r` to `path` atomically (temporary file, fsync, rename, directory fsync).
pub fn write_recovery(path: &Path, r: &Recovery) -> Result<(), StorageError> {
    let io = |e: std::io::Error| StorageError::Corrupt(format!("writing {}: {e}", path.display()));
    let bytes = encode_recovery(r)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(&bytes).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(io)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir).and_then(|d| d.sync_all()).map_err(io)?;
    }
    Ok(())
}

/// Reads a recovery file; `Ok(None)` if there is none. Files larger than 1 GiB are refused.
pub fn read_recovery(path: &Path) -> Result<Option<Recovery>, StorageError> {
    let io = |e: std::io::Error| StorageError::Corrupt(format!("reading {}: {e}", path.display()));
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    if len > 1 << 30 {
        return Err(corrupt("larger than 1 GiB"));
    }
    let b = std::fs::read(path).map_err(io)?;
    decode_recovery(&b).map(Some)
}
