//! Stroke file format v1 (JSONL): line 1 header `{"format":"simtab-strokes","version":1,"note":…}`,
//! then one [`Sample`] per line. Coordinates are normalised to the mapped region (0..1); `t_us`
//! is non-decreasing.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

pub const MAX_SAMPLES: usize = 50_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Pen,
    Eraser,
}

/// One input frame. `tool: None` = out of proximity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub t_us: u64,
    pub tool: Option<Tool>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// 0..1; > 0 means tip contact on the device.
    #[serde(default)]
    pub p: f64,
    /// Tilt in degrees, −90..90.
    #[serde(default)]
    pub tx: f64,
    #[serde(default)]
    pub ty: f64,
    /// Hover distance 0..1 (0 when touching).
    #[serde(default)]
    pub d: f64,
    #[serde(default)]
    pub b1: bool,
    #[serde(default)]
    pub b2: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Strokes {
    pub header: Header,
    pub samples: Vec<Sample>,
}

pub fn header(note: String) -> Header {
    Header {
        format: "simtab-strokes".into(),
        version: 1,
        note,
    }
}

pub fn write(w: &mut impl Write, s: &Strokes) -> Result<()> {
    serde_json::to_writer(&mut *w, &s.header)?;
    writeln!(w)?;
    for smp in &s.samples {
        serde_json::to_writer(&mut *w, smp)?;
        writeln!(w)?;
    }
    Ok(())
}

pub fn write_file(path: &Path, s: &Strokes) -> Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    write(&mut w, s)?;
    w.flush()?;
    Ok(())
}

pub fn read(r: impl BufRead) -> Result<Strokes> {
    let mut lines = r.lines();
    let first = lines.next().context("empty file")??;
    let header: Header = serde_json::from_str(&first)?;
    if header.format != "simtab-strokes" || header.version != 1 {
        bail!("unsupported stroke file {} v{}", header.format, header.version);
    }
    let mut samples = Vec::new();
    let mut last = 0;
    for (i, l) in lines.enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let lno = i.saturating_add(2);
        let s: Sample = serde_json::from_str(&l).with_context(|| format!("line {lno}"))?;
        let finite = [s.x, s.y, s.p, s.tx, s.ty, s.d].iter().all(|v| v.is_finite());
        if !finite {
            bail!("line {lno}: non-finite value");
        }
        if s.t_us < last {
            bail!("line {lno}: t_us decreases");
        }
        if samples.len() >= MAX_SAMPLES {
            bail!("too many samples");
        }
        last = s.t_us;
        samples.push(s);
    }
    Ok(Strokes { header, samples })
}

pub fn read_file(path: &Path) -> Result<Strokes> {
    read(std::io::BufReader::new(
        std::fs::File::open(path).with_context(|| path.display().to_string())?,
    ))
}

/// Normalised sub-rectangle of the tablet area that stroke coordinates map into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Region {
    pub const FULL: Region = Region {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub fn parse(s: &str) -> Result<Region> {
        let v: Vec<f64> = s.split(',').map(|t| t.trim().parse()).collect::<Result<_, _>>()?;
        let [x, y, w, h] = v.as_slice() else {
            bail!("region needs x,y,w,h")
        };
        let r = Region {
            x: *x,
            y: *y,
            w: *w,
            h: *h,
        };
        r.check()?;
        Ok(r)
    }

    /// Must lie fully inside the tablet area: clamped coordinates would press the pen onto
    /// whatever is at the screen edge (lesson from Phase 0).
    pub fn check(&self) -> Result<()> {
        let ok = [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.w > 0.0
            && self.h > 0.0
            && self.x + self.w <= 1.0
            && self.y + self.h <= 1.0;
        if ok {
            Ok(())
        } else {
            bail!("region {self:?} is not fully inside the tablet area; refusing")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_region_checks() {
        let s = Strokes {
            header: header("t".into()),
            samples: vec![Sample {
                t_us: 0,
                tool: Some(Tool::Pen),
                x: 0.1,
                y: 0.2,
                p: 0.5,
                tx: 1.0,
                ty: 2.0,
                d: 0.0,
                b1: false,
                b2: true,
            }],
        };
        let mut buf = Vec::new();
        write(&mut buf, &s).unwrap();
        assert_eq!(read(buf.as_slice()).unwrap(), s);
        assert!(Region::parse("0.1,0.1,0.5,0.5").is_ok());
        assert!(Region::parse("0.6,0.1,0.5,0.5").is_err());
        assert!(Region::parse("-0.1,0,0.5,0.5").is_err());
        assert!(Region::parse("0,0,1").is_err());
    }
}
