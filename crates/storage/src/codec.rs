//! Lossless stroke codec for quantised samples (see `vibenote_model::Sample::quantised`).
//!
//! Layout (v1): `b"S1"` · tool u8 · colour u32 LE · width f32 LE · n varint · per sample:
//! zigzag-varint Δ(x·1024), Δ(y·1024), Δ(p·65535), tilt_x·100, tilt_y·100 (zigzag), Δt_us varint.
//! Every read is bounds-checked; malformed input is an error, never a panic.

use crate::StorageError;
use vibenote_model::{Color, Sample, Stroke, Tool, grid};

const MAGIC: &[u8; 2] = b"S1";
const MAX_SAMPLES: u64 = vibenote_model::limits::MAX_SAMPLES_PER_STROKE as u64;

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(u8::try_from(v & 0x7f).unwrap_or(0) | 0x80);
        v >>= 7;
    }
    out.push(u8::try_from(v).unwrap_or(0));
}
pub(crate) fn zz(v: i64) -> u64 {
    (v.wrapping_shl(1) ^ (v >> 63)).cast_unsigned()
}
pub(crate) fn unzz(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ (v & 1).cast_signed().wrapping_neg()
}

/// Exact integer for a quantised value `v · g` (|v·g| < 2^53 guaranteed by model limits).
#[allow(clippy::cast_possible_truncation)] // value is rounded and range-checked first
fn q64(v: f64, g: f64) -> Result<i64, StorageError> {
    let k = (v * g).round();
    if !k.is_finite() || k.abs() > 9.0e15 {
        return Err(StorageError::Corrupt("sample out of codec range".into()));
    }
    Ok(k as i64)
}

fn q32(v: f32, g: f32) -> Result<i64, StorageError> {
    q64(f64::from(v), f64::from(g))
}

pub fn encode_stroke(s: &Stroke) -> Result<Vec<u8>, StorageError> {
    let mut out = Vec::with_capacity(s.samples.len().saturating_mul(8).saturating_add(16));
    out.extend_from_slice(MAGIC);
    out.push(match s.tool {
        Tool::Pen => 0,
        Tool::Highlighter => 1,
    });
    out.extend_from_slice(&s.color.0.to_le_bytes());
    out.extend_from_slice(&s.width.to_bits().to_le_bytes());
    put_varint(&mut out, u64::try_from(s.samples.len()).unwrap_or(u64::MAX));
    let (mut px, mut py, mut pp, mut pt) = (0i64, 0i64, 0i64, 0u64);
    for smp in &s.samples {
        // Snap with the model's own arithmetic first: rounding the raw f32 pressure in f64
        // here can land one grid step away from the model's f32 rounding (found by the
        // engine persistence proptest), so the stored value would differ from memory.
        let smp = &smp.quantised();
        let (x, y, p) = (
            q64(smp.x, grid::XY)?,
            q64(smp.y, grid::XY)?,
            q32(smp.pressure, grid::P)?,
        );
        put_varint(&mut out, zz(x.wrapping_sub(px)));
        put_varint(&mut out, zz(y.wrapping_sub(py)));
        put_varint(&mut out, zz(p.wrapping_sub(pp)));
        put_varint(&mut out, zz(q32(smp.tilt_x, grid::TILT)?));
        put_varint(&mut out, zz(q32(smp.tilt_y, grid::TILT)?));
        put_varint(&mut out, smp.t_us.wrapping_sub(pt));
        (px, py, pp, pt) = (x, y, p, smp.t_us);
    }
    Ok(out)
}

pub(crate) struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Reader { b, i: 0 }
    }
    /// `n` bytes, bounds-checked (a hostile length cannot allocate or read past the end).
    pub(crate) fn take(&mut self, n: u64) -> Result<&'a [u8], StorageError> {
        let n = usize::try_from(n).map_err(|_| StorageError::Corrupt("length out of range".into()))?;
        let end = self
            .i
            .checked_add(n)
            .ok_or_else(|| StorageError::Corrupt("length out of range".into()))?;
        let v = self
            .b
            .get(self.i..end)
            .ok_or_else(|| StorageError::Corrupt("truncated record".into()))?;
        self.i = end;
        Ok(v)
    }
    pub(crate) fn at_end(&self) -> bool {
        self.i >= self.b.len()
    }
    pub(crate) fn byte(&mut self) -> Result<u8, StorageError> {
        let v = *self
            .b
            .get(self.i)
            .ok_or_else(|| StorageError::Corrupt("truncated stroke".into()))?;
        self.i = self.i.saturating_add(1);
        Ok(v)
    }
    pub(crate) fn bytes<const N: usize>(&mut self) -> Result<[u8; N], StorageError> {
        let mut a = [0u8; N];
        for x in &mut a {
            *x = self.byte()?;
        }
        Ok(a)
    }
    pub(crate) fn varint(&mut self) -> Result<u64, StorageError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let c = self.byte()?;
            v |= u64::from(c & 0x7f).checked_shl(shift).unwrap_or(0);
            if c < 0x80 {
                return Ok(v);
            }
        }
        Err(StorageError::Corrupt("varint too long".into()))
    }
}

/// Lossy by design only for the f32 axes, and only when *decoding values the encoder never
/// produced* (corrupt input); valid input round-trips exactly.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn deq(k: i64, g: f64) -> f64 {
    k as f64 / g
}
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn deq32(k: i64, g: f32) -> f32 {
    k as f32 / g
}

pub fn decode_stroke(b: &[u8]) -> Result<Stroke, StorageError> {
    let mut r = Reader { b, i: 0 };
    if &r.bytes::<2>()? != MAGIC {
        return Err(StorageError::Corrupt("bad stroke magic".into()));
    }
    let tool = match r.byte()? {
        0 => Tool::Pen,
        1 => Tool::Highlighter,
        t => return Err(StorageError::Corrupt(format!("tool {t}"))),
    };
    let color = Color(u32::from_le_bytes(r.bytes::<4>()?));
    let width = f32::from_bits(u32::from_le_bytes(r.bytes::<4>()?));
    let n = r.varint()?;
    if n > MAX_SAMPLES {
        return Err(StorageError::Corrupt("too many samples".into()));
    }
    let mut samples = Vec::with_capacity(usize::try_from(n.min(65_536)).unwrap_or(0));
    let (mut x, mut y, mut p, mut t) = (0i64, 0i64, 0i64, 0u64);
    for _ in 0..n {
        x = x.wrapping_add(unzz(r.varint()?));
        y = y.wrapping_add(unzz(r.varint()?));
        p = p.wrapping_add(unzz(r.varint()?));
        let tx = unzz(r.varint()?);
        let ty = unzz(r.varint()?);
        t = t.wrapping_add(r.varint()?);
        samples.push(Sample {
            x: deq(x, grid::XY),
            y: deq(y, grid::XY),
            pressure: deq32(p, grid::P),
            tilt_x: deq32(tx, grid::TILT),
            tilt_y: deq32(ty, grid::TILT),
            t_us: t,
        });
    }
    if r.i != b.len() {
        return Err(StorageError::Corrupt("trailing bytes after stroke".into()));
    }
    Ok(Stroke {
        tool,
        color,
        width,
        samples,
    })
}
