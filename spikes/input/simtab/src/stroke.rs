//! Stroke file format v1 (JSONL).
//!
//! Line 1: header `{"format":"simtab-strokes","version":1,"note":...}`
//! Lines 2..: one `Sample` per line. Coordinates are normalised to the mapped region
//! (0..1). `t_us` is relative to the start of the file and must be non-decreasing.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Pen,
    Eraser,
}

/// One input frame. `tool: None` = out of proximity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub t_us: u64,
    pub tool: Option<Tool>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// 0..1; > 0 means tip contact.
    #[serde(default)]
    pub p: f64,
    /// Tilt in degrees, -90..90.
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub note: String,
}

pub struct Strokes {
    pub header: Header,
    pub samples: Vec<Sample>,
}

pub fn write_file(path: &Path, s: &Strokes) -> Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(&mut w, &s.header)?;
    writeln!(w)?;
    for smp in &s.samples {
        serde_json::to_writer(&mut w, smp)?;
        writeln!(w)?;
    }
    Ok(())
}

pub fn read_file(path: &Path) -> Result<Strokes> {
    let r = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut lines = r.lines();
    let header: Header = serde_json::from_str(&lines.next().context("empty file")??)?;
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
        let s: Sample = serde_json::from_str(&l).with_context(|| format!("line {}", i + 2))?;
        if s.t_us < last {
            bail!("line {}: t_us decreases", i + 2);
        }
        last = s.t_us;
        samples.push(s);
    }
    Ok(Strokes { header, samples })
}

/// Normalised sub-rectangle of the tablet area that stroke coordinates map into.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(clap::Args, Debug)]
pub struct MapOpts {
    /// Map strokes into this normalised tablet rectangle "x,y,w,h" (default: whole area).
    #[arg(long)]
    pub region: Option<String>,
    /// Map strokes into the window whose class matches (queries `hyprctl -j`), with a margin.
    #[arg(long)]
    pub window: Option<String>,
}

impl MapOpts {
    pub fn resolve(&self) -> Result<Region> {
        if let Some(r) = &self.region {
            let v: Vec<f64> = r.split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
            if v.len() != 4 {
                bail!("--region needs x,y,w,h");
            }
            return Ok(Region { x: v[0], y: v[1], w: v[2], h: v[3] });
        }
        if let Some(class) = &self.window {
            return hypr_window_region(class);
        }
        Ok(Region { x: 0.0, y: 0.0, w: 1.0, h: 1.0 })
    }
}

/// Hyprland maps an unconfigured tablet onto the monitor; compute the window's rectangle
/// relative to that monitor (10% inner margin).
fn hypr_window_region(class: &str) -> Result<Region> {
    let run = |a: &str| -> Result<serde_json::Value> {
        let o = std::process::Command::new("hyprctl").args([a, "-j"]).output()?;
        Ok(serde_json::from_slice(&o.stdout)?)
    };
    let clients = run("clients")?;
    let mons = run("monitors")?;
    let c = clients
        .as_array()
        .and_then(|a| a.iter().find(|c| c["class"].as_str() == Some(class)))
        .with_context(|| format!("no window with class {class}"))?;
    let mon_id = c["monitor"].as_i64().unwrap_or(0);
    let m = mons
        .as_array()
        .and_then(|a| a.iter().find(|m| m["id"].as_i64() == Some(mon_id)))
        .context("monitor not found")?;
    let f = |v: &serde_json::Value, i: usize| v[i].as_f64().unwrap_or(0.0);
    let scale = m["scale"].as_f64().unwrap_or(1.0);
    let (mx, my) = (m["x"].as_f64().unwrap_or(0.0), m["y"].as_f64().unwrap_or(0.0));
    let mw = m["width"].as_f64().unwrap_or(1.0) / scale;
    let mh = m["height"].as_f64().unwrap_or(1.0) / scale;
    let (wx, wy, ww, wh) = (f(&c["at"], 0) - mx, f(&c["at"], 1) - my, f(&c["size"], 0), f(&c["size"], 1));
    let mg = 0.1;
    let r = Region {
        x: (wx + ww * mg) / mw,
        y: (wy + wh * mg) / mh,
        w: ww * (1.0 - 2.0 * mg) / mw,
        h: wh * (1.0 - 2.0 * mg) / mh,
    };
    // Safety: never play if the target rectangle is not fully on the monitor; clamped
    // coordinates would press the pen onto whatever is at the screen edge.
    if r.x < 0.0 || r.y < 0.0 || r.x + r.w > 1.0 || r.y + r.h > 1.0 || r.w <= 0.0 || r.h <= 0.0 {
        bail!("window {class} is not fully on its monitor (region {r:?}); refusing to play");
    }
    Ok(r)
}
