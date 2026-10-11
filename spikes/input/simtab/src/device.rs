//! uinput tablet device and timed playback.

use crate::stroke::{Region, Sample, Strokes, Tool};
use anyhow::Result;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode as A, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode as K,
    PropType, UinputAbsSetup,
};
use std::io::Write;
use std::time::Duration;

// Geometry: 330 x 210 mm at 100 units/mm (matches the owner's 330x210 mm panel).
pub const X_MAX: i32 = 33_000;
pub const Y_MAX: i32 = 21_000;
const XY_RES: i32 = 100; // units per mm
pub const P_MAX: i32 = 8191;
// Tilt in units/radian (libinput interprets non-zero tilt resolution as units/radian,
// see libinput src/evdev-tablet.c adjust_tilt()). 5730 units/rad ≈ 100 units/degree.
const TILT_RES: i32 = 5730;
const TILT_MAX: i32 = 9000;
const D_MAX: i32 = 63;

#[derive(clap::Args, Debug)]
pub struct DeviceOpts {
    #[arg(long, default_value = "simtab virtual pen")]
    pub name: String,
    /// Set INPUT_PROP_DIRECT (pen display) instead of an external (screenless) tablet.
    #[arg(long)]
    pub direct: bool,
    /// Leave out tilt axes (to emulate cheaper pens).
    #[arg(long)]
    pub no_tilt: bool,
}

pub fn create(o: &DeviceOpts) -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<K>::new();
    for k in [K::BTN_TOOL_PEN, K::BTN_TOOL_RUBBER, K::BTN_TOUCH, K::BTN_STYLUS, K::BTN_STYLUS2] {
        keys.insert(k);
    }
    let mut props = AttributeSet::<PropType>::new();
    props.insert(if o.direct { PropType::DIRECT } else { PropType::POINTER });
    let abs = |c, min, max, res| UinputAbsSetup::new(c, AbsInfo::new(0, min, max, 0, 0, res));
    let mut b = VirtualDevice::builder()?
        .name(o.name.as_str())
        // Vendor/product deliberately not Wacom so no libwacom/vendor quirks apply.
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

pub fn now_mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid out-pointer. (Spike code; unsafe needs ADR approval before graduating.)
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Sleep until `deadline` (CLOCK_MONOTONIC ns): coarse sleep, then spin the last 300 µs.
fn wait_until(deadline: u64) {
    loop {
        let n = now_mono_ns();
        if n >= deadline {
            return;
        }
        let rem = deadline - n;
        if rem > 300_000 {
            std::thread::sleep(Duration::from_nanos(rem - 300_000));
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
    let p = (cl(s.p) * P_MAX as f64).round() as i32;
    State {
        tool: s.tool,
        x: (cl(r.x + s.x * r.w) * X_MAX as f64).round() as i32,
        y: (cl(r.y + s.y * r.h) * Y_MAX as f64).round() as i32,
        p,
        tx: (s.tx.clamp(-90.0, 90.0) * 100.0).round() as i32,
        ty: (s.ty.clamp(-90.0, 90.0) * 100.0).round() as i32,
        d: if p > 0 { 0 } else { (cl(s.d) * D_MAX as f64).round() as i32 },
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

/// Plays `s` with real-time pacing. Each frame ends with SYN_REPORT; the log gets
/// `{"seq","t_us","mono_ns","x","y","p",...}` per frame (device units + normalised input).
pub fn play(dev: &mut VirtualDevice, s: &Strokes, r: Region, mut log: Option<&mut impl Write>) -> Result<()> {
    let ev = |t: EventType, c: u16, v: i32| InputEvent::new(t.0, c, v);
    let key = |k: K, on: bool| ev(EventType::KEY, k.code(), on as i32);
    let abs = |a: A, v: i32| ev(EventType::ABSOLUTE, a.0, v);
    let mut prev = State::default();
    let start = now_mono_ns() + 50_000_000;
    for (seq, smp) in s.samples.iter().enumerate() {
        let cur = to_state(smp, r);
        let mut evs: Vec<InputEvent> = Vec::with_capacity(12);
        // Tool change: leave proximity with the old tool first (as real hardware does).
        if prev.tool.is_some() && prev.tool != cur.tool {
            if prev.touch {
                evs.push(abs(A::ABS_PRESSURE, 0));
                evs.push(key(K::BTN_TOUCH, false));
            }
            evs.push(abs(A::ABS_DISTANCE, 0));
            evs.push(key(tool_key(prev.tool.unwrap_or(Tool::Pen)), false));
            wait_until(start + smp.t_us * 1000);
            dev.emit(&evs)?;
            evs.clear();
            prev = State::default();
        }
        if let Some(t) = cur.tool {
            let entering = prev.tool.is_none();
            if entering || cur.x != prev.x {
                evs.push(abs(A::ABS_X, cur.x));
            }
            if entering || cur.y != prev.y {
                evs.push(abs(A::ABS_Y, cur.y));
            }
            if entering || cur.p != prev.p {
                evs.push(abs(A::ABS_PRESSURE, cur.p));
            }
            if entering || cur.d != prev.d {
                evs.push(abs(A::ABS_DISTANCE, cur.d));
            }
            if entering || cur.tx != prev.tx {
                evs.push(abs(A::ABS_TILT_X, cur.tx));
            }
            if entering || cur.ty != prev.ty {
                evs.push(abs(A::ABS_TILT_Y, cur.ty));
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
        wait_until(start + smp.t_us * 1000);
        if !evs.is_empty() {
            let t = now_mono_ns();
            dev.emit(&evs)?;
            if let Some(w) = log.as_mut() {
                writeln!(
                    w,
                    r#"{{"seq":{seq},"t_us":{},"mono_ns":{t},"tool":{},"x":{},"y":{},"p":{},"tx":{},"ty":{},"d":{},"touch":{}}}"#,
                    smp.t_us,
                    match cur.tool { Some(Tool::Pen) => "\"pen\"", Some(Tool::Eraser) => "\"eraser\"", None => "null" },
                    cur.x, cur.y, cur.p, cur.tx, cur.ty, cur.d, cur.touch
                )?;
            }
        }
        prev = cur;
    }
    if let Some(t) = prev.tool {
        let mut evs = vec![];
        if prev.touch {
            evs.push(abs(A::ABS_PRESSURE, 0));
            evs.push(key(K::BTN_TOUCH, false));
        }
        evs.push(key(tool_key(t), false));
        dev.emit(&evs)?;
    }
    if let Some(w) = log.as_mut() {
        w.flush()?;
    }
    Ok(())
}
