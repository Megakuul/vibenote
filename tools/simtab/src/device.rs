//! uinput tablet device and real-time playback (binary only; needs /dev/uinput access).

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode as A, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode as K, PropType,
    UinputAbsSetup,
};
use simtab::stroke::{Region, Sample, Strokes, Tool};
use simtab::{f64_to_i32, mono_ns};
use std::io::Write;
use std::time::Duration;

// 330 × 210 mm at 100 units/mm (the owner's panel size).
pub const X_MAX: i32 = 33_000;
pub const Y_MAX: i32 = 21_000;
const XY_RES: i32 = 100;
pub const P_MAX: i32 = 8191;
// libinput reads non-zero tilt resolution as units/radian (evdev-tablet.c adjust_tilt()).
const TILT_RES: i32 = 5730;
const TILT_MAX: i32 = 9000;
const D_MAX: i32 = 63;

#[derive(clap::Args, Debug)]
pub struct DeviceOpts {
    #[arg(long, default_value = "simtab virtual pen")]
    pub name: String,
    /// Set INPUT_PROP_DIRECT (pen display) instead of an external tablet.
    #[arg(long)]
    pub direct: bool,
    /// Leave out tilt axes.
    #[arg(long)]
    pub no_tilt: bool,
}

pub fn create(o: &DeviceOpts) -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<K>::new();
    for k in [
        K::BTN_TOOL_PEN,
        K::BTN_TOOL_RUBBER,
        K::BTN_TOUCH,
        K::BTN_STYLUS,
        K::BTN_STYLUS2,
    ] {
        keys.insert(k);
    }
    let mut props = AttributeSet::<PropType>::new();
    props.insert(if o.direct { PropType::DIRECT } else { PropType::POINTER });
    let abs = |c, min, max, res| UinputAbsSetup::new(c, AbsInfo::new(0, min, max, 0, 0, res));
    let mut b = VirtualDevice::builder()
        .context("opening /dev/uinput (see tools/simtab/README.md for permissions)")?
        .name(o.name.as_str())
        .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1d50, 0x5154, 1))
        .with_keys(&keys)?
        .with_properties(&props)?
        .with_absolute_axis(&abs(A::ABS_X, 0, X_MAX, XY_RES))?
        .with_absolute_axis(&abs(A::ABS_Y, 0, Y_MAX, XY_RES))?
        .with_absolute_axis(&abs(A::ABS_PRESSURE, 0, P_MAX, 0))?
        .with_absolute_axis(&abs(A::ABS_DISTANCE, 0, D_MAX, 0))?;
    if !o.no_tilt {
        b = b
            .with_absolute_axis(&abs(A::ABS_TILT_X, -TILT_MAX, TILT_MAX, TILT_RES))?
            .with_absolute_axis(&abs(A::ABS_TILT_Y, -TILT_MAX, TILT_MAX, TILT_RES))?;
    }
    Ok(b.build()?)
}

/// Sleep until `deadline` (CLOCK_MONOTONIC ns): coarse sleep, then spin the last 300 µs.
fn wait_until(deadline: u64) {
    loop {
        let n = mono_ns();
        if n >= deadline {
            return;
        }
        let rem = deadline.saturating_sub(n);
        if rem > 300_000 {
            std::thread::sleep(Duration::from_nanos(rem.saturating_sub(300_000)));
        } else {
            std::hint::spin_loop();
        }
    }
}

#[derive(Default, Clone, PartialEq)]
struct State {
    tool: Option<Tool>,
    x: i32,
    y: i32,
    p: i32,
    tx: i32,
    ty: i32,
    d: i32,
    touch: bool,
    b1: bool,
    b2: bool,
}

fn to_state(s: &Sample, r: Region) -> State {
    let cl = |v: f64| v.clamp(0.0, 1.0);
    let p = f64_to_i32(cl(s.p) * f64::from(P_MAX), 0, P_MAX);
    State {
        tool: s.tool,
        x: f64_to_i32(cl(r.x + s.x * r.w) * f64::from(X_MAX), 0, X_MAX),
        y: f64_to_i32(cl(r.y + s.y * r.h) * f64::from(Y_MAX), 0, Y_MAX),
        p,
        tx: f64_to_i32(s.tx.clamp(-90.0, 90.0) * 100.0, -TILT_MAX, TILT_MAX),
        ty: f64_to_i32(s.ty.clamp(-90.0, 90.0) * 100.0, -TILT_MAX, TILT_MAX),
        d: if p > 0 {
            0
        } else {
            f64_to_i32(cl(s.d) * f64::from(D_MAX), 0, D_MAX)
        },
        touch: p > 0,
        b1: s.b1,
        b2: s.b2,
    }
}

fn tool_key(t: Tool) -> K {
    match t {
        Tool::Pen => K::BTN_TOOL_PEN,
        Tool::Eraser => K::BTN_TOOL_RUBBER,
    }
}

/// Plays `s` in real time. Each emitted frame is logged as JSON with its CLOCK_MONOTONIC time.
/// Plays `s` in real time. `abort` is polled before every frame (set by the session-safety
/// watcher in main.rs: screen locked, target window moved/hidden); then the pen is lifted and
/// playback stops with an error.
pub fn play(
    dev: &mut VirtualDevice,
    s: &Strokes,
    r: Region,
    mut log: Option<&mut dyn Write>,
    abort: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    r.check()?;
    let ev = |t: EventType, c: u16, v: i32| InputEvent::new(t.0, c, v);
    let key = |k: K, on: bool| ev(EventType::KEY, k.code(), i32::from(on));
    let abs = |a: A, v: i32| ev(EventType::ABSOLUTE, a.0, v);
    let mut prev = State::default();
    let start = mono_ns().saturating_add(50_000_000);
    for (seq, smp) in s.samples.iter().enumerate() {
        if abort.load(std::sync::atomic::Ordering::Acquire) {
            lift(dev, &prev)?;
            anyhow::bail!("playback aborted by the session-safety check");
        }
        let cur = to_state(smp, r);
        let at = start.saturating_add(smp.t_us.saturating_mul(1000));
        let mut evs: Vec<InputEvent> = Vec::with_capacity(12);
        if let Some(pt) = prev.tool
            && prev.tool != cur.tool
        {
            if prev.touch {
                evs.push(abs(A::ABS_PRESSURE, 0));
                evs.push(key(K::BTN_TOUCH, false));
            }
            evs.push(abs(A::ABS_DISTANCE, 0));
            evs.push(key(tool_key(pt), false));
            wait_until(at);
            dev.emit(&evs)?;
            evs.clear();
            prev = State::default();
        }
        if let Some(t) = cur.tool {
            let entering = prev.tool.is_none();
            for (changed, axis, v) in [
                (cur.x != prev.x, A::ABS_X, cur.x),
                (cur.y != prev.y, A::ABS_Y, cur.y),
                (cur.p != prev.p, A::ABS_PRESSURE, cur.p),
                (cur.d != prev.d, A::ABS_DISTANCE, cur.d),
                (cur.tx != prev.tx, A::ABS_TILT_X, cur.tx),
                (cur.ty != prev.ty, A::ABS_TILT_Y, cur.ty),
            ] {
                if entering || changed {
                    evs.push(abs(axis, v));
                }
            }
            if entering {
                evs.push(key(tool_key(t), true));
            }
            if cur.touch != prev.touch {
                evs.push(key(K::BTN_TOUCH, cur.touch));
            }
            if cur.b1 != prev.b1 {
                evs.push(key(K::BTN_STYLUS, cur.b1));
            }
            if cur.b2 != prev.b2 {
                evs.push(key(K::BTN_STYLUS2, cur.b2));
            }
        }
        wait_until(at);
        if !evs.is_empty() {
            let t = mono_ns();
            dev.emit(&evs)?;
            if let Some(w) = log.as_mut() {
                let tool = match cur.tool {
                    Some(Tool::Pen) => "\"pen\"",
                    Some(Tool::Eraser) => "\"eraser\"",
                    None => "null",
                };
                writeln!(
                    w,
                    r#"{{"seq":{seq},"t_us":{},"mono_ns":{t},"tool":{tool},"x":{},"y":{},"p":{},"tx":{},"ty":{},"d":{},"touch":{}}}"#,
                    smp.t_us, cur.x, cur.y, cur.p, cur.tx, cur.ty, cur.d, cur.touch
                )?;
            }
        }
        prev = cur;
    }
    lift(dev, &prev)?;
    if let Some(w) = log.as_mut() {
        w.flush()?;
    }
    Ok(())
}

/// Ends contact and proximity so no stroke or tool is left active.
fn lift(dev: &mut VirtualDevice, prev: &State) -> Result<()> {
    let Some(t) = prev.tool else { return Ok(()) };
    let ev = |t: EventType, c: u16, v: i32| InputEvent::new(t.0, c, v);
    let mut evs = vec![];
    if prev.touch {
        evs.push(ev(EventType::ABSOLUTE, A::ABS_PRESSURE.0, 0));
        evs.push(ev(EventType::KEY, K::BTN_TOUCH.code(), 0));
    }
    evs.push(ev(EventType::KEY, tool_key(t).code(), 0));
    dev.emit(&evs)?;
    Ok(())
}
