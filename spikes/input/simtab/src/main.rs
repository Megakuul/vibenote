//! simtab — virtual pen tablet over Linux uinput, driven by stroke files.
//!
//! Spike tool (Phase 0). Events take the real path:
//! kernel → libinput → compositor → zwp_tablet_v2 → app.

mod device;
mod generate;
mod stroke;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Virtual pen tablet (uinput) for Inkwell input spikes")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the virtual tablet and keep it alive (for libinput list-devices / manual tests).
    Hold {
        #[arg(long, default_value_t = 30)]
        secs: u64,
        #[command(flatten)]
        dev: device::DeviceOpts,
    },
    /// Generate a stroke file.
    Gen {
        #[command(flatten)]
        g: generate::GenOpts,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Replay a stroke file (or generate one inline) through the virtual tablet.
    Play {
        /// Stroke file (JSONL). If absent, `--pattern` is generated inline.
        file: Option<PathBuf>,
        #[command(flatten)]
        g: generate::GenOpts,
        #[command(flatten)]
        map: stroke::MapOpts,
        #[command(flatten)]
        dev: device::DeviceOpts,
        /// Write a JSONL log of every emitted frame with CLOCK_MONOTONIC ns.
        #[arg(long)]
        log: Option<PathBuf>,
        /// Wait this long after creating the device so libinput/compositor pick it up.
        #[arg(long, default_value_t = 1500)]
        settle_ms: u64,
        /// Repeat the file N times.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Hold { secs, dev } => {
            let _d = device::create(&dev)?;
            eprintln!("simtab: device '{}' created; holding {secs}s", dev.name);
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
        Cmd::Gen { g, out } => {
            let s = generate::generate(&g)?;
            stroke::write_file(&out, &s)?;
            eprintln!("simtab: wrote {} samples to {}", s.samples.len(), out.display());
        }
        Cmd::Play { file, g, map, dev, log, settle_ms, repeat } => {
            let s = match file {
                Some(p) => stroke::read_file(&p).with_context(|| format!("reading {}", p.display()))?,
                None => generate::generate(&g)?,
            };
            let region = map.resolve()?;
            let mut d = device::create(&dev)?;
            std::thread::sleep(std::time::Duration::from_millis(settle_ms));
            let mut logw = match log {
                Some(p) => Some(std::io::BufWriter::new(std::fs::File::create(p)?)),
                None => None,
            };
            for _ in 0..repeat {
                device::play(&mut d, &s, region, logw.as_mut())?;
            }
            // Let the last frames drain before the device disappears.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    Ok(())
}
