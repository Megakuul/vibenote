//! Variant (c): direct Wayland client. Binds zwp_tablet_manager_v2 itself, renders with wgpu
//! (Vulkan) on a raw wl_surface, measures with wp_presentation.
//!
//! Knobs (env):
//!   INK_LOG=path          JSONL log (see common::log)
//!   INK_SCHED=immediate|frame_cb|deadline   when to render after new ink (default immediate)
//!   INK_DEADLINE_US=2000  for `deadline`: render this long before the predicted next vblank
//!   INK_PRESENT=fifo|mailbox|immediate      Vulkan present mode (default fifo)
//!   INK_FRAME_LATENCY=1   wgpu desired_maximum_frame_latency
//!   INK_PREDICT_US=0      ink prediction horizon (0 = off; gpu only)
//!   INK_RENDER=gpu|shm    gpu = wgpu full redraw; shm = CPU raster + partial damage

mod shm;

use common::ink::{self, Pt, Stroke};
use common::log::{Ev, Log};
use common::{env_or, mono_ns};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_pointer, wl_compositor, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1 as fsm, wp_fractional_scale_v1 as fs};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback as pfb};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2 as tm, zwp_tablet_pad_v2 as pad, zwp_tablet_seat_v2 as tseat, zwp_tablet_tool_v2 as tool,
    zwp_tablet_v2 as tab,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sched {
    Immediate,
    FrameCb,
    Deadline,
}

#[derive(Default)]
struct ToolState {
    kind: &'static str,
    x: f64,
    y: f64,
    p: f64,
    tx: f64,
    ty: f64,
    d: f64,
    down: bool,
    // pending frame flags
    f_down: bool,
    f_up: bool,
    f_in: bool,
    f_out: bool,
    f_motion: bool,
    f_btn: Vec<(u32, bool)>,
}

struct App {
    log: Log,
    compositor: Option<wl_compositor::WlCompositor>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    tablet_mgr: Option<tm::ZwpTabletManagerV2>,
    presentation: Option<wp_presentation::WpPresentation>,
    clock_id: Option<u32>,
    fsm: Option<fsm::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    surface: Option<wl_surface::WlSurface>,
    viewport: Option<wp_viewport::WpViewport>,
    configured: bool,
    closed: bool,
    logical: (i32, i32),
    scale120: u32,
    tools: Vec<(tool::ZwpTabletToolV2, ToolState)>,
    strokes: Vec<Stroke>,
    uploaded: (usize, usize), // (stroke idx, pt idx) already uploaded to GPU
    dirty: bool,
    frame_cb_pending: bool,
    frame_cb_ready: bool,
    next_frame: u64,
    last_pres_ns: u64,
    refresh_ns: u64,
    resized: bool,
    wl_shm: Option<wl_shm::WlShm>,
    shm: Option<shm::Shm>,
    shm_drawn: (usize, usize),
    shm_wait: bool,
    deadline_fire: Option<u64>,
    mouse: (f64, f64, bool),
}

impl App {
    fn scale(&self) -> f64 {
        self.scale120 as f64 / 120.0
    }
    fn phys(&self) -> (u32, u32) {
        let s = self.scale();
        ((self.logical.0 as f64 * s).round() as u32, (self.logical.1 as f64 * s).round() as u32)
    }
}

fn main() -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    let display = conn.display();
    display.get_registry(&qh, ());
    let mut app = App {
        log: Log::from_env(),
        compositor: None,
        wm: None,
        seat: None,
        tablet_mgr: None,
        presentation: None,
        clock_id: None,
        fsm: None,
        viewporter: None,
        surface: None,
        viewport: None,
        configured: false,
        closed: false,
        logical: (1200, 800),
        scale120: 120,
        tools: vec![],
        strokes: vec![],
        uploaded: (0, 0),
        dirty: true,
        frame_cb_pending: false,
        frame_cb_ready: true,
        next_frame: 0,
        last_pres_ns: 0,
        refresh_ns: 0,
        resized: false,
        wl_shm: None,
        shm: None,
        shm_drawn: (0, 0),
        shm_wait: false,
        deadline_fire: None,
        mouse: (0.0, 0.0, false),
    };
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    let compositor = app.compositor.clone().ok_or_else(|| anyhow::anyhow!("no wl_compositor"))?;
    let wm = app.wm.clone().ok_or_else(|| anyhow::anyhow!("no xdg_wm_base"))?;
    let surface = compositor.create_surface(&qh, ());
    if let (Some(f), Some(vp)) = (&app.fsm, &app.viewporter) {
        f.get_fractional_scale(&surface, &qh, ());
        app.viewport = Some(vp.get_viewport(&surface, &qh, ()));
    }
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let top = xs.get_toplevel(&qh, ());
    top.set_title("inkwell spike (c) direct wayland".into());
    top.set_app_id("inkspike-wl".into());
    surface.commit();
    app.surface = Some(surface.clone());
    if let Some(s) = &app.seat {
        s.get_pointer(&qh, ());
    }
    if let (Some(m), Some(s)) = (&app.tablet_mgr, &app.seat) {
        m.get_tablet_seat(s, &qh, ());
    } else {
        eprintln!("var-wl: no tablet manager or seat");
    }
    while !app.configured {
        queue.blocking_dispatch(&mut app)?;
    }

    // wgpu surface from raw libwayland pointers.
    let backend = conn.backend();
    let dptr = NonNull::new(backend.display_ptr() as *mut std::ffi::c_void).ok_or_else(|| anyhow::anyhow!("null display"))?;
    let sptr = NonNull::new(surface.id().as_ptr() as *mut std::ffi::c_void).ok_or_else(|| anyhow::anyhow!("null surface"))?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    // SAFETY: display and surface outlive the renderer (both live until main returns).
    let wsurf = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(dptr))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(sptr)),
        })?
    };
    let (pw, ph) = app.phys();
    let use_shm = std::env::var("INK_RENDER").as_deref() == Ok("shm");
    let mut r = if use_shm {
        drop(wsurf);
        None
    } else {
        Some(common::gpu::Renderer::new(&instance, wsurf, pw, ph)?)
    };
    let sched = match std::env::var("INK_SCHED").as_deref() {
        Ok("frame_cb") => Sched::FrameCb,
        Ok("deadline") => Sched::Deadline,
        _ => Sched::Immediate,
    };
    let deadline_ns: u64 = env_or("INK_DEADLINE_US", 2000u64) * 1000;
    let predict_ns: u64 = env_or("INK_PREDICT_US", 0u64) * 1000;
    app.log.meta(serde_json::json!({
        "variant": "wl", "render": if use_shm {"shm"} else {"gpu"}, "sched": format!("{sched:?}"),
        "present": r.as_ref().map(|r| format!("{:?}", r.config.present_mode)),
        "frame_latency": r.as_ref().map(|r| r.config.desired_maximum_frame_latency),
        "format": r.as_ref().map(|r| format!("{:?}", r.config.format)),
        "predict_us": predict_ns / 1000, "deadline_us": deadline_ns / 1000,
        "adapter": r.as_ref().map(|r| r.adapter_info.name.clone()), "clock_id": app.clock_id, "scale120": app.scale120,
    }));
    eprintln!("var-wl: render={} sched={sched:?} clock_id={:?}", if use_shm {"shm"} else {"gpu"}, app.clock_id);

    let fd = conn.backend().poll_fd().as_raw_fd();
    while !app.closed {
        // Decide timeout.
        let now = mono_ns();
        let mut timeout_ms: i32 = -1;
        let mut render_now = false;
        if app.dirty && !app.shm_wait {
            match sched {
                Sched::Immediate => render_now = true,
                Sched::FrameCb => render_now = app.frame_cb_ready,
                Sched::Deadline => {
                    if app.refresh_ns == 0 || app.last_pres_ns == 0 {
                        render_now = true;
                    } else {
                        // Fix the render time once per frame: next vblank that is at least
                        // `deadline` away, minus the deadline margin.
                        let fire = *app.deadline_fire.get_or_insert_with(|| {
                            let k = (now.saturating_sub(app.last_pres_ns)) / app.refresh_ns + 1;
                            let mut target = app.last_pres_ns + k * app.refresh_ns;
                            if target < now + deadline_ns {
                                target += app.refresh_ns;
                            }
                            target - deadline_ns
                        });
                        if fire <= now {
                            render_now = true;
                            app.deadline_fire = None;
                        } else if fire - now < 1_000_000 {
                            timeout_ms = 0; // spin the last millisecond
                        } else {
                            timeout_ms = ((fire - now) / 1_000_000) as i32;
                        }
                    }
                }
            }
        }
        if render_now {
            // Coalesce: pull every event already queued on the socket before drawing.
            conn.flush().ok();
            if let Some(guard) = queue.prepare_read() {
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                // SAFETY: valid pollfd array of length 1.
                if unsafe { libc::poll(&mut pfd, 1, 0) } > 0 {
                    let _ = guard.read();
                }
            }
            queue.dispatch_pending(&mut app)?;
        }
        if render_now && r.is_none() {
            let (w, h) = app.phys();
            if app.resized || app.shm.is_none() {
                if let Some(vp) = &app.viewport {
                    vp.set_destination(app.logical.0, app.logical.1);
                }
                app.shm = None;
                if let Some(g) = &app.wl_shm {
                    app.shm = Some(shm::Shm::new(g, w as i32, h as i32, &qh)?);
                }
                app.shm_drawn = (0, 0);
                app.resized = false;
            }
            shm_draw(&mut app);
            let f = app.next_frame;
            let last = app.log.last_ev();
            if sched == Sched::FrameCb {
                surface.frame(&qh, ());
            }
            let pres = app.presentation.clone();
            let ok = app.shm.as_mut().is_some_and(|s| {
                s.present(&surface, || {
                    if let Some(p) = pres {
                        p.feedback(&surface, &qh, f);
                    }
                })
            });
            if ok {
                app.next_frame += 1;
                app.log.draw(f, mono_ns(), last);
                app.dirty = false;
                if sched == Sched::FrameCb {
                    app.frame_cb_ready = false;
                }
            } else {
                app.shm_wait = true; // both buffers busy: wait for a release
            }
            conn.flush().ok();
            continue;
        }
        if let (true, Some(r)) = (render_now, r.as_mut()) {
            if app.resized {
                let (w, h) = app.phys();
                r.resize(w, h);
                if let Some(vp) = &app.viewport {
                    vp.set_destination(app.logical.0, app.logical.1);
                }
                app.resized = false;
            }
            upload(&mut app, r);
            let mut trans = vec![];
            if predict_ns > 0 {
                if let Some(s) = app.strokes.last() {
                    if app.tools.iter().any(|t| t.1.down) {
                        let pr = ink::predict(s, predict_ns, 8_000_000);
                        let mut prev = s.pts.last().copied().unwrap_or_default();
                        for p in pr {
                            ink::segment(&mut trans, prev, p, app.scale() as f32, ink::PREDICT);
                            prev = p;
                        }
                    }
                }
            }
            let f = app.next_frame;
            app.next_frame += 1;
            let last = app.log.last_ev();
            if sched == Sched::FrameCb {
                surface.frame(&qh, ());
                app.frame_cb_ready = false;
            }
            let pres = app.presentation.clone();
            let surf = surface.clone();
            let qh2 = qh.clone();
            let tip = app.strokes.last().filter(|_| app.tools.iter().any(|t| t.1.down)).and_then(|s| {
                let p = if predict_ns > 0 { ink::predict(s, predict_ns, 8_000_000).last().copied() } else { None };
                p.or(s.pts.last().copied())
            });
            if let Some(t) = tip {
                let sc = app.scale() as f32;
                app.log.raw(format!(r#"{{"k":"tip","f":{f},"x":{:.3},"y":{:.3}}}"#, t.x / sc, t.y / sc));
            }
            let ok = r.render(&trans, || {
                if let Some(p) = pres {
                    p.feedback(&surf, &qh2, f);
                }
            });
            if ok {
                app.log.draw(f, mono_ns(), last);
            }
            app.dirty = false;
            conn.flush().ok();
            continue;
        }
        conn.flush().ok();
        if let Some(guard) = queue.prepare_read() {
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: valid pollfd array of length 1.
            let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if n > 0 {
                let _ = guard.read();
            } else {
                drop(guard);
            }
        }
        queue.dispatch_pending(&mut app)?;
    }
    Ok(())
}

fn shm_draw(app: &mut App) {
    let scale = app.scale() as f32;
    let Some(sh) = app.shm.as_mut() else { return };
    let (mut si, mut pi) = app.shm_drawn;
    while si < app.strokes.len() {
        let st = &app.strokes[si];
        let c = if st.eraser { ink::ERASE } else { ink::INK };
        for i in pi..st.pts.len() {
            let a = if i == 0 { st.pts[0] } else { st.pts[i - 1] };
            sh.segment(a, st.pts[i], scale, c);
        }
        if si + 1 < app.strokes.len() {
            si += 1;
            pi = 0;
        } else {
            pi = st.pts.len();
            break;
        }
    }
    app.shm_drawn = (si, pi);
}

fn upload(app: &mut App, r: &mut common::gpu::Renderer) {
    let s = app.scale() as f32;
    let mut v = vec![];
    let (mut si, mut pi) = app.uploaded;
    while si < app.strokes.len() {
        let st = &app.strokes[si];
        ink::stroke_mesh(&mut v, st, pi, s);
        if si + 1 < app.strokes.len() {
            si += 1;
            pi = 0;
        } else {
            pi = st.pts.len();
            break;
        }
    }
    app.uploaded = (si, pi);
    r.append(&v);
}

// ---- Wayland dispatch ----

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(a: &mut Self, reg: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "wl_compositor" => a.compositor = Some(reg.bind(name, version.min(6), qh, ())),
                "xdg_wm_base" => a.wm = Some(reg.bind(name, version.min(5), qh, ())),
                "wl_seat" => a.seat = Some(reg.bind(name, version.min(7), qh, ())),
                "zwp_tablet_manager_v2" => a.tablet_mgr = Some(reg.bind(name, version.min(1), qh, ())),
                "wp_presentation" => a.presentation = Some(reg.bind(name, version.min(1), qh, ())),
                "wp_fractional_scale_manager_v1" => a.fsm = Some(reg.bind(name, 1, qh, ())),
                "wp_viewporter" => a.viewporter = Some(reg.bind(name, 1, qh, ())),
                "wl_shm" => a.wl_shm = Some(reg.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(_: &mut Self, wm: &xdg_wm_base::XdgWmBase, e: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(a: &mut Self, xs: &xdg_surface::XdgSurface, e: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            xs.ack_configure(serial);
            a.configured = true;
            a.dirty = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(a: &mut Self, _: &xdg_toplevel::XdgToplevel, e: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                if (width, height) != a.logical {
                    a.logical = (width, height);
                    a.resized = true;
                }
            }
            xdg_toplevel::Event::Close => a.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<fs::WpFractionalScaleV1, ()> for App {
    fn event(a: &mut Self, _: &fs::WpFractionalScaleV1, e: fs::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let fs::Event::PreferredScale { scale } = e {
            a.scale120 = scale;
            a.resized = true;
            a.dirty = true;
            a.log.meta(serde_json::json!({"scale120": scale}));
        }
    }
}

impl Dispatch<wp_presentation::WpPresentation, ()> for App {
    fn event(a: &mut Self, _: &wp_presentation::WpPresentation, e: wp_presentation::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_presentation::Event::ClockId { clk_id } = e {
            a.clock_id = Some(clk_id);
        }
    }
}

impl Dispatch<pfb::WpPresentationFeedback, u64> for App {
    fn event(a: &mut Self, _: &pfb::WpPresentationFeedback, e: pfb::Event, f: &u64, _: &Connection, _: &QueueHandle<Self>) {
        match e {
            pfb::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, flags, .. } => {
                let ns = (((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000 + tv_nsec as u64;
                let fl = match flags {
                    WEnum::Value(v) => v.bits(),
                    WEnum::Unknown(u) => u,
                };
                a.log.pres(*f, ns, refresh as u64, fl);
                a.last_pres_ns = ns;
                a.refresh_ns = refresh as u64;
            }
            pfb::Event::Discarded => a.log.disc(*f),
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(a: &mut Self, _: &wl_callback::WlCallback, e: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = e {
            a.frame_cb_ready = true;
            a.frame_cb_pending = false;
        }
    }
}

impl Dispatch<tseat::ZwpTabletSeatV2, ()> for App {
    fn event(a: &mut Self, _: &tseat::ZwpTabletSeatV2, e: tseat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let tseat::Event::ToolAdded { id } = e {
            a.tools.push((id, ToolState { kind: "pen", ..Default::default() }));
        }
    }
    wayland_client::event_created_child!(App, tseat::ZwpTabletSeatV2, [
        tseat::EVT_TABLET_ADDED_OPCODE => (tab::ZwpTabletV2, ()),
        tseat::EVT_TOOL_ADDED_OPCODE => (tool::ZwpTabletToolV2, ()),
        tseat::EVT_PAD_ADDED_OPCODE => (pad::ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<tool::ZwpTabletToolV2, ()> for App {
    fn event(a: &mut Self, t: &tool::ZwpTabletToolV2, e: tool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let ns = mono_ns();
        let Some(idx) = a.tools.iter().position(|(x, _)| x == t) else { return };
        let scale = a.scale() as f32;
        let st = &mut a.tools[idx].1;
        match e {
            tool::Event::Type { tool_type } => {
                st.kind = match tool_type {
                    WEnum::Value(tool::Type::Eraser) => "eraser",
                    _ => "pen",
                };
            }
            tool::Event::ProximityIn { .. } => st.f_in = true,
            tool::Event::ProximityOut => st.f_out = true,
            tool::Event::Down { .. } => {
                st.f_down = true;
                st.down = true;
            }
            tool::Event::Up => {
                st.f_up = true;
                st.down = false;
            }
            tool::Event::Motion { x, y } => {
                st.x = x;
                st.y = y;
                st.f_motion = true;
            }
            tool::Event::Pressure { pressure } => {
                st.p = pressure as f64 / 65535.0;
                st.f_motion = true;
            }
            tool::Event::Tilt { tilt_x, tilt_y } => {
                st.tx = tilt_x;
                st.ty = tilt_y;
            }
            tool::Event::Distance { distance } => st.d = distance as f64 / 65535.0,
            tool::Event::Button { button, state, .. } => {
                st.f_btn.push((button, matches!(state, WEnum::Value(tool::ButtonState::Pressed))));
            }
            tool::Event::Frame { time } => {
                let ev = Ev { x: st.x, y: st.y, p: st.p, tx: st.tx, ty: st.ty, d: st.d };
                let kind = st.kind;
                let (fin, fout, fdown, fup, fmot) = (st.f_in, st.f_out, st.f_down, st.f_up, st.f_motion);
                let btns = std::mem::take(&mut st.f_btn);
                st.f_in = false;
                st.f_out = false;
                st.f_down = false;
                st.f_up = false;
                st.f_motion = false;
                let down_now = st.down;
                if fin {
                    a.log.ev(ns, "prox_in", kind, ev, Some(time), false);
                }
                for (b, s) in btns {
                    a.log.ev(ns, if s { "btn_down" } else { "btn_up" }, kind, Ev { d: b as f64, ..ev }, Some(time), false);
                }
                let pt = Pt { x: ev.x as f32 * scale, y: ev.y as f32 * scale, p: ev.p as f32, ns };
                if fdown {
                    a.log.ev(ns, "down", kind, ev, Some(time), false);
                    a.strokes.push(Stroke { pts: vec![pt], eraser: kind == "eraser" });
                    a.dirty = true;
                } else if fmot && down_now {
                    a.log.ev(ns, "motion", kind, ev, Some(time), false);
                    if let Some(s) = a.strokes.last_mut() {
                        s.pts.push(pt);
                    }
                    a.dirty = true;
                } else if fmot {
                    a.log.ev(ns, "hover", kind, ev, Some(time), false);
                }
                if fup {
                    a.log.ev(ns, "up", kind, ev, Some(time), false);
                }
                if fout {
                    a.log.ev(ns, "prox_out", kind, ev, Some(time), false);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(a: &mut Self, _: &wl_buffer::WlBuffer, e: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = e {
            if let Some(b) = a.shm.as_mut().and_then(|s| s.bufs.get_mut(*i)) {
                b.busy = false;
            }
            a.shm_wait = false;
        }
    }
}

/// Mouse drawing (fixed pressure 0.5) for the owner's rough feel test. Logged as m_* events so the
/// latency analysis (which only reads tablet down/motion) ignores it.
impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(a: &mut Self, _: &wl_pointer::WlPointer, e: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let ns = mono_ns();
        let sc = a.scale() as f32;
        match e {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let wl_pointer::Event::Motion { surface_x, surface_y, .. } = e {
                    a.mouse.0 = surface_x;
                    a.mouse.1 = surface_y;
                } else {
                    a.mouse.0 = surface_x;
                    a.mouse.1 = surface_y;
                }
                if a.mouse.2 {
                    a.log.ev(ns, "m_motion", "mouse", Ev { x: a.mouse.0, y: a.mouse.1, p: 0.5, ..Default::default() }, None, false);
                    if let Some(s) = a.strokes.last_mut() {
                        s.pts.push(Pt { x: a.mouse.0 as f32 * sc, y: a.mouse.1 as f32 * sc, p: 0.5, ns });
                    }
                    a.dirty = true;
                }
            }
            wl_pointer::Event::Button { button: 0x110, state, .. } => {
                let down = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                a.mouse.2 = down;
                a.log.ev(ns, if down { "m_down" } else { "m_up" }, "mouse", Ev { x: a.mouse.0, y: a.mouse.1, p: 0.5, ..Default::default() }, None, false);
                if down {
                    a.strokes.push(Stroke { pts: vec![Pt { x: a.mouse.0 as f32 * sc, y: a.mouse.1 as f32 * sc, p: 0.5, ns }], eraser: false });
                }
                a.dirty = true;
            }
            _ => {}
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    wl_seat::WlSeat,
    tm::ZwpTabletManagerV2,
    tab::ZwpTabletV2,
    fsm::WpFractionalScaleManagerV1,
    wp_viewporter::WpViewporter,
    wp_viewport::WpViewport,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool
);

impl Dispatch<pad::ZwpTabletPadV2, ()> for App {
    fn event(_: &mut Self, _: &pad::ZwpTabletPadV2, _: pad::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(App, pad::ZwpTabletPadV2, [
        pad::EVT_GROUP_OPCODE => (wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()),
    ]);
}
ignore!(wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2);
