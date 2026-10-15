//! simtab — virtual pen tablet over Linux uinput, driven by stroke files, plus fixture generator.

mod device;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use simtab::generate::{GenOpts, generate};
use simtab::stroke::{Region, read_file, write_file};
use std::io::Write;
use std::path::PathBuf;
use vibenote_ink::input::Surface;

#[derive(Parser)]
#[command(about = "Virtual pen tablet (uinput), stroke generator and ink-fixture generator for vibenote")]
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
    /// Generate a stroke file (device input).
    Gen {
        #[command(flatten)]
        g: GenOpts,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Generate an ink fixture (what the app receives) from a pattern or a stroke file.
    Fixture {
        /// Stroke file; if absent, `--pattern` is generated.
        #[arg(long)]
        from: Option<PathBuf>,
        #[command(flatten)]
        g: GenOpts,
        /// Surface size in logical px, "WxH".
        #[arg(long, default_value = "1200x800")]
        surface: String,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Replay a stroke file (or a generated pattern) through the virtual tablet.
    Play {
        file: Option<PathBuf>,
        #[command(flatten)]
        g: GenOpts,
        /// Map strokes into this normalised tablet rectangle "x,y,w,h" (must be fully on the tablet).
        #[arg(long)]
        region: Option<String>,
        /// Map strokes into the Hyprland window with this class (10 % margin).
        #[arg(long)]
        window: Option<String>,
        /// With --window: only the window of this process (refuses if several windows match
        /// the class without it).
        #[arg(long, requires = "window")]
        window_pid: Option<i64>,
        #[command(flatten)]
        dev: device::DeviceOpts,
        /// JSONL log of every emitted frame with CLOCK_MONOTONIC ns.
        #[arg(long)]
        log: Option<PathBuf>,
        #[arg(long, default_value_t = 1500)]
        settle_ms: u64,
    },
}

fn parse_surface(s: &str) -> Result<Surface> {
    let (w, h) = s.split_once('x').context("surface must be WxH")?;
    let (width, height): (f64, f64) = (w.trim().parse()?, h.trim().parse()?);
    if !(1.0..=100_000.0).contains(&width) || !(1.0..=100_000.0).contains(&height) {
        bail!("surface out of range");
    }
    Ok(Surface {
        width,
        height,
        scale: 1.0,
    })
}

/// Which window to draw into: Hyprland class, optionally narrowed to one process.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    class: String,
    pid: Option<i64>,
}

fn hypr(a: &str) -> Result<serde_json::Value> {
    let o = std::process::Command::new("hyprctl").args([a, "-j"]).output()?;
    Ok(serde_json::from_slice(&o.stdout)?)
}

/// Hyprland maps an unconfigured tablet onto the monitor: compute the window rectangle relative
/// to it with a 10 % inner margin. Refuses ambiguous targets (several matching windows), windows
/// not fully on the monitor, and windows whose workspace is not the one shown on it.
fn hypr_window_region(t: &Target) -> Result<Region> {
    let clients = hypr("clients")?;
    let mons = hypr("monitors")?;
    let g = |v: &serde_json::Value, p: &str| {
        v.pointer(p)
            .and_then(serde_json::Value::as_f64)
            .context(format!("missing {p}"))
    };
    let matches: Vec<&serde_json::Value> = clients
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c.get("class").and_then(|x| x.as_str()) == Some(t.class.as_str()))
                .filter(|c| t.pid.is_none() || c.get("pid").and_then(serde_json::Value::as_i64) == t.pid)
                .collect()
        })
        .unwrap_or_default();
    let c = match matches.as_slice() {
        [c] => *c,
        [] => bail!("no window matches {t:?}"),
        _ => bail!("{} windows match {t:?}; pass --window-pid to pick one", matches.len()),
    };
    let mon_id = c.get("monitor").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let m = mons
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|m| m.get("id").and_then(serde_json::Value::as_i64) == Some(mon_id))
        })
        .context("monitor not found")?;
    let shown = m.pointer("/activeWorkspace/id").and_then(serde_json::Value::as_i64);
    let ws = c.pointer("/workspace/id").and_then(serde_json::Value::as_i64);
    if shown.is_none() || shown != ws {
        bail!("target window is not on the workspace shown on its monitor");
    }
    let scale = g(m, "/scale").unwrap_or(1.0);
    let (mw, mh) = (g(m, "/width")? / scale, g(m, "/height")? / scale);
    let (wx, wy) = (g(c, "/at/0")? - g(m, "/x")?, g(c, "/at/1")? - g(m, "/y")?);
    let (ww, wh) = (g(c, "/size/0")?, g(c, "/size/1")?);
    let mg = 0.1;
    let r = Region {
        x: (wx + ww * mg) / mw,
        y: (wy + wh * mg) / mh,
        w: ww * (1.0 - 2.0 * mg) / mw,
        h: wh * (1.0 - 2.0 * mg) / mh,
    };
    r.check()?;
    Ok(r)
}

/// A screen locker is running (pen input must never reach a lock screen).
fn session_locked() -> bool {
    const LOCKERS: &[&str] = &["hyprlock", "swaylock", "gtklock", "waylock"];
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return true;
    };
    rd.filter_map(Result::ok)
        .any(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| LOCKERS.contains(&c.trim())))
}

/// Checks the session every 100 ms while playing; sets `abort` when it becomes unsafe.
fn spawn_watcher(
    target: Option<(Target, Region)>,
    abort: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    std::thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
            let problem = if session_locked() {
                Some("session locked".to_string())
            } else if let Some((t, r)) = &target {
                match hypr_window_region(t) {
                    Ok(now) if now == *r => None,
                    Ok(_) => Some("target window moved or resized".into()),
                    Err(e) => Some(e.to_string()),
                }
            } else {
                None
            };
            if let Some(p) = problem {
                eprintln!("simtab: stopping playback: {p}");
                abort.store(true, Ordering::Release);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    })
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Hold { secs, dev } => {
            let _d = device::create(&dev)?;
            eprintln!("simtab: device '{}' created; holding {secs}s", dev.name);
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
        Cmd::Gen { g, out } => {
            let s = generate(&g)?;
            write_file(&out, &s)?;
            eprintln!("simtab: wrote {} samples to {}", s.samples.len(), out.display());
        }
        Cmd::Fixture { from, g, surface, out } => {
            let (s, note) = match from {
                Some(p) => (read_file(&p)?, format!("from {}", p.display())),
                None => (generate(&g)?, format!("{g:?}")),
            };
            let f = simtab::fixture::to_fixture(&s, parse_surface(&surface)?, &note);
            let mut w = std::io::BufWriter::new(std::fs::File::create(&out)?);
            f.write(&mut w)?;
            w.flush()?;
            eprintln!("simtab: wrote {} events to {}", f.events.len(), out.display());
        }
        Cmd::Play {
            file,
            g,
            region,
            window,
            window_pid,
            dev,
            log,
            settle_ms,
        } => {
            let s = match file {
                Some(p) => read_file(&p)?,
                None => generate(&g)?,
            };
            if session_locked() {
                bail!("session is locked; refusing to inject pen input");
            }
            let target = window.map(|class| Target { class, pid: window_pid });
            let r = match (region, &target) {
                (Some(r), _) => Region::parse(&r)?,
                (None, Some(t)) => hypr_window_region(t)?,
                (None, None) => Region::FULL,
            };
            let mut d = device::create(&dev)?;
            std::thread::sleep(std::time::Duration::from_millis(settle_ms));
            let mut logw = match log {
                Some(p) => Some(std::io::BufWriter::new(std::fs::File::create(p)?)),
                None => None,
            };
            let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Re-check after the settle time, then keep checking during playback.
            let watched = target.map(|t| (t, r));
            if let Some((t, r0)) = &watched
                && hypr_window_region(t)? != *r0
            {
                bail!("target window moved during settle");
            }
            if session_locked() {
                bail!("session is locked; refusing to inject pen input");
            }
            let w = spawn_watcher(watched, abort.clone(), done.clone());
            let res = device::play(&mut d, &s, r, logw.as_mut().map(|w| w as &mut dyn Write), &abort);
            done.store(true, std::sync::atomic::Ordering::Release);
            let _ = w.join();
            res?;
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    Ok(())
}
