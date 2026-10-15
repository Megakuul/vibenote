//! Persistent undo/redo log (schema v2, R-UNDO-001). One row per step:
//! `history(scope, kind, seq, label, ops)`; `kind` 0 = undo, 1 = redo; the step with the highest
//! `seq` is the top of its stack. History changes are written in the same transaction as the ops
//! they belong to, so after a crash the log always matches the stored state.
//!
//! Step ops are a compact binary record (`b"O1"` · n varint · per op: tag u8 + fields); objects
//! use the stroke codec (strokes) or JSON (other kinds), like the `object` table. Every read is
//! bounds-checked; malformed input is an error, never a panic.

use crate::StorageError;
use crate::codec::{Reader, put_varint, unzz, zz};
use crate::{decode_stroke, encode_stroke};
use vibenote_model::{Canvas, CanvasObject, FolderId, NoteId, ObjectId, ObjectKind, Op};

const MAGIC: &[u8; 2] = b"O1";
/// Ops per step (a hostile count must not allocate). A note delete inverse holds one op.
const MAX_OPS: u64 = 1 << 20;

/// Which stack of a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stack {
    Undo,
    Redo,
}

impl Stack {
    fn code(self) -> i64 {
        match self {
            Stack::Undo => 0,
            Stack::Redo => 1,
        }
    }
}

/// One stored step: label + the ops that perform it.
#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub label: String,
    pub ops: Vec<Op>,
}

/// A change to the stored history, mirroring what the engine did in memory.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryOp {
    Push {
        scope: String,
        stack: Stack,
        step: StepRecord,
    },
    Pop {
        scope: String,
        stack: Stack,
    },
    Clear {
        scope: String,
        stack: Stack,
    },
    /// Drops the oldest step (bound of 1,000 steps per scope).
    DropOldest {
        scope: String,
        stack: Stack,
    },
}

/// Both stacks of one scope, oldest step first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredHistory {
    pub undo: Vec<StepRecord>,
    pub redo: Vec<StepRecord>,
}

fn json_err(e: serde_json::Error) -> StorageError {
    StorageError::Corrupt(e.to_string())
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, u64::try_from(b.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(b);
}

fn put_json<T: serde::Serialize>(out: &mut Vec<u8>, v: &T) -> Result<(), StorageError> {
    put_bytes(out, &serde_json::to_vec(v).map_err(json_err)?);
    Ok(())
}

fn put_object(out: &mut Vec<u8>, o: &CanvasObject) -> Result<(), StorageError> {
    put_varint(out, o.id.0);
    put_varint(out, zz(o.z));
    match &o.kind {
        ObjectKind::Stroke(s) => {
            out.push(0);
            put_bytes(out, &encode_stroke(s)?);
        }
        other => {
            out.push(1);
            put_json(out, other)?;
        }
    }
    Ok(())
}

pub fn encode_ops(ops: &[Op]) -> Result<Vec<u8>, StorageError> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(MAGIC);
    put_varint(&mut out, u64::try_from(ops.len()).unwrap_or(u64::MAX));
    for op in ops {
        match op {
            Op::CreateFolder { folder } => {
                out.push(0);
                put_json(&mut out, folder)?;
            }
            Op::DeleteFolder { folder } => {
                out.push(1);
                put_json(&mut out, folder)?;
            }
            Op::UpdateFolder { after } => {
                out.push(2);
                put_json(&mut out, after)?;
            }
            Op::CreateNote { note, canvas } => {
                out.push(3);
                put_json(&mut out, note)?;
                put_varint(&mut out, u64::try_from(canvas.objects.len()).unwrap_or(u64::MAX));
                for o in canvas.objects.values() {
                    put_object(&mut out, o)?;
                }
            }
            Op::DeleteNote { note } => {
                out.push(4);
                put_json(&mut out, note)?;
            }
            Op::UpdateNote { after } => {
                out.push(5);
                put_json(&mut out, after)?;
            }
            Op::InsertObject { note, object } => {
                out.push(6);
                put_varint(&mut out, note.0);
                put_object(&mut out, object)?;
            }
            Op::RemoveObject { note, object } => {
                out.push(7);
                put_varint(&mut out, note.0);
                put_object(&mut out, object)?;
            }
            Op::ReplaceObject { note, after } => {
                out.push(8);
                put_varint(&mut out, note.0);
                put_object(&mut out, after)?;
            }
            Op::TrashNote { note } => {
                out.push(9);
                put_varint(&mut out, note.0);
            }
            Op::RestoreNote { note } => {
                out.push(10);
                put_varint(&mut out, note.0);
            }
            Op::TrashFolder { folder } => {
                out.push(11);
                put_varint(&mut out, folder.0);
            }
            Op::RestoreFolder { folder } => {
                out.push(12);
                put_varint(&mut out, folder.0);
            }
            Op::SetNoteBackground { note, background } => {
                out.push(13);
                put_varint(&mut out, note.0);
                put_json(&mut out, background)?;
            }
            Op::SetPrettify { note, block, after } => {
                out.push(14);
                put_varint(&mut out, note.0);
                put_varint(&mut out, block.0);
                put_json(&mut out, after)?;
            }
            Op::SetAudioMark { note, stroke, after } => {
                out.push(15);
                put_varint(&mut out, note.0);
                put_varint(&mut out, stroke.0);
                put_json(&mut out, after)?;
            }
        }
    }
    Ok(out)
}

fn json<T: serde::de::DeserializeOwned>(r: &mut Reader<'_>) -> Result<T, StorageError> {
    let n = r.varint()?;
    serde_json::from_slice(r.take(n)?).map_err(json_err)
}

fn object(r: &mut Reader<'_>) -> Result<CanvasObject, StorageError> {
    let id = ObjectId(r.varint()?);
    let z = unzz(r.varint()?);
    let kind = r.byte()?;
    let n = r.varint()?;
    let data = r.take(n)?;
    let kind = match kind {
        0 => ObjectKind::Stroke(decode_stroke(data)?),
        1 => serde_json::from_slice(data).map_err(json_err)?,
        k => return Err(StorageError::Corrupt(format!("unknown object kind {k}"))),
    };
    Ok(CanvasObject { id, z, kind })
}

pub fn decode_ops(b: &[u8]) -> Result<Vec<Op>, StorageError> {
    let mut r = Reader::new(b);
    if r.bytes::<2>()? != *MAGIC {
        return Err(StorageError::Corrupt("bad step record".into()));
    }
    let n = r.varint()?;
    if n > MAX_OPS {
        return Err(StorageError::Corrupt("too many ops in step".into()));
    }
    let mut ops = Vec::new();
    for _ in 0..n {
        let op = match r.byte()? {
            0 => Op::CreateFolder { folder: json(&mut r)? },
            1 => Op::DeleteFolder { folder: json(&mut r)? },
            2 => Op::UpdateFolder { after: json(&mut r)? },
            3 => {
                let note = json(&mut r)?;
                let k = r.varint()?;
                let mut canvas = Canvas::default();
                for _ in 0..k {
                    // Each object consumes bytes, so a hostile count fails at the end of input.
                    let o = object(&mut r)?;
                    canvas.objects.insert(o.id, o);
                }
                Op::CreateNote { note, canvas }
            }
            4 => Op::DeleteNote { note: json(&mut r)? },
            5 => Op::UpdateNote { after: json(&mut r)? },
            6 => Op::InsertObject {
                note: NoteId(r.varint()?),
                object: object(&mut r)?,
            },
            7 => Op::RemoveObject {
                note: NoteId(r.varint()?),
                object: object(&mut r)?,
            },
            8 => Op::ReplaceObject {
                note: NoteId(r.varint()?),
                after: object(&mut r)?,
            },
            9 => Op::TrashNote {
                note: NoteId(r.varint()?),
            },
            10 => Op::RestoreNote {
                note: NoteId(r.varint()?),
            },
            11 => Op::TrashFolder {
                folder: FolderId(r.varint()?),
            },
            12 => Op::RestoreFolder {
                folder: FolderId(r.varint()?),
            },
            13 => Op::SetNoteBackground {
                note: NoteId(r.varint()?),
                background: json(&mut r)?,
            },
            14 => Op::SetPrettify {
                note: NoteId(r.varint()?),
                block: ObjectId(r.varint()?),
                after: json(&mut r)?,
            },
            15 => Op::SetAudioMark {
                note: NoteId(r.varint()?),
                stroke: ObjectId(r.varint()?),
                after: json(&mut r)?,
            },
            t => return Err(StorageError::Corrupt(format!("unknown op tag {t}"))),
        };
        ops.push(op);
    }
    if !r.at_end() {
        return Err(StorageError::Corrupt("trailing bytes in step record".into()));
    }
    Ok(ops)
}

pub(crate) fn write(tx: &rusqlite::Transaction<'_>, h: &HistoryOp) -> Result<(), StorageError> {
    use rusqlite::params;
    match h {
        HistoryOp::Push { scope, stack, step } => {
            tx.execute(
                "INSERT INTO history(scope, kind, seq, label, ops)
                 VALUES(?1, ?2, COALESCE((SELECT MAX(seq) FROM history WHERE scope = ?1 AND kind = ?2), 0) + 1, ?3, ?4)",
                params![scope, stack.code(), step.label, encode_ops(&step.ops)?],
            )?;
        }
        HistoryOp::Pop { scope, stack } => {
            tx.execute(
                "DELETE FROM history WHERE scope = ?1 AND kind = ?2
                 AND seq = (SELECT MAX(seq) FROM history WHERE scope = ?1 AND kind = ?2)",
                params![scope, stack.code()],
            )?;
        }
        HistoryOp::Clear { scope, stack } => {
            tx.execute(
                "DELETE FROM history WHERE scope = ?1 AND kind = ?2",
                params![scope, stack.code()],
            )?;
        }
        HistoryOp::DropOldest { scope, stack } => {
            tx.execute(
                "DELETE FROM history WHERE scope = ?1 AND kind = ?2
                 AND seq = (SELECT MIN(seq) FROM history WHERE scope = ?1 AND kind = ?2)",
                params![scope, stack.code()],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn load(conn: &rusqlite::Connection, scope: &str) -> Result<StoredHistory, StorageError> {
    let mut st = conn.prepare("SELECT kind, label, ops FROM history WHERE scope = ?1 ORDER BY kind, seq")?;
    let rows = st.query_map(rusqlite::params![scope], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
    })?;
    let mut h = StoredHistory::default();
    for row in rows {
        let (kind, label, ops) = row?;
        let step = StepRecord {
            label,
            ops: decode_ops(&ops)?,
        };
        match kind {
            0 => h.undo.push(step),
            1 => h.redo.push(step),
            k => return Err(StorageError::Corrupt(format!("history kind {k}"))),
        }
    }
    Ok(h)
}
