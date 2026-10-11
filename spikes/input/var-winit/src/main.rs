//! Variant (b): winit 0.31.0-beta.3 (the first winit line with Wayland tablet support) + wgpu.
//! winit does not expose wp_presentation, so this variant binds it on a second
//! wayland-client connection object wrapping winit's own wl_display (guest backend).
//!
//! Knobs (env): INK_LOG, INK_SCHED=redraw|immediate (default redraw), INK_PRESENT, INK_FRAME_LATENCY,
//! INK_PREDICT_US

use common::ink::{self, Pt, Stroke};
use common::log::{Ev, Log};
use common::{env_or, mono_ns};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback as pfb};
use winit::application::ApplicationHandler;
use winit::event::{ButtonSource, ElementState, Force, PointerKind, PointerSource, TabletToolButton, TabletToolKind, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

struct Pres {
    log_tx: Option<Log>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Pres {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wp_presentation::WpPresentation, ()> for Pres {
    fn event(_: &mut Self, _: &wp_presentation::WpPresentation, _: wp_presentation::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<pfb::WpPresentationFeedback, u64> for Pres {
    fn event(s: &mut Self, _: &pfb::WpPresentationFeedback, e: pfb::Event, f: &u64, _: &Connection, _: &QueueHandle<Self>) {
        let Some(log) = &s.log_tx else { return };
        match e {
            pfb::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, flags, .. } => {
                let ns = (((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000 + tv_nsec as u64;
                let fl = match flags {
                    WEnum::Value(v) => v.bits(),
                    WEnum::Unknown(u) => u,
                };
                log.pres(*f, ns, refresh as u64, fl);
            }
            pfb::Event::Discarded => log.disc(*f),
            _ => {}
        }
    }
}

struct Gfx {
    // Field order = drop order: renderer before window.
    r: common::gpu::Renderer,
    pres_q: EventQueue<Pres>,
    pres: Pres,
    presentation: wp_presentation::WpPresentation,
    surface: WlSurface,
    _conn: Connection,
    window: Box<dyn Window>,
}

struct App {
    log: Log,
    gfx: Option<Gfx>,
    strokes: Vec<Stroke>,
    uploaded: (usize, usize),
    down: bool,
    immediate: bool,
    predict_ns: u64,
    next_frame: u64,
    mouse_down: bool,
}

impl App {
    fn render(&mut self) {
        let Some(g) = self.gfx.as_mut() else { return };
        let _ = g.pres_q.dispatch_pending(&mut g.pres);
        let scale = g.window.scale_factor() as f32;
        // upload new geometry
        let mut v = vec![];
        let (mut si, mut pi) = self.uploaded;
        while si < self.strokes.len() {
            ink::stroke_mesh(&mut v, &self.strokes[si], pi, scale);
            if si + 1 < self.strokes.len() {
                si += 1;
                pi = 0;
            } else {
                pi = self.strokes[si].pts.len();
                break;
            }
        }
        self.uploaded = (si, pi);
        g.r.append(&v);
        let mut trans = vec![];
        if self.down && self.predict_ns > 0 {
            if let Some(s) = self.strokes.last() {
                let mut prev = s.pts.last().copied().unwrap_or_default();
                for p in ink::predict(s, self.predict_ns, 8_000_000) {
                    ink::segment(&mut trans, prev, p, scale, ink::PREDICT);
                    prev = p;
                }
            }
        }
        let f = self.next_frame;
        self.next_frame += 1;
        let last = self.log.last_ev();
        g.window.pre_present_notify();
        let (p, s, qh) = (g.presentation.clone(), g.surface.clone(), g.pres_q.handle());
        if g.r.render(&trans, || {
            p.feedback(&s, &qh, f);
        }) {
            self.log.draw(f, mono_ns(), last);
        }
    }

    fn ink_changed(&mut self) {
        if self.immediate {
            self.render();
        } else if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }
}

fn kind_name(k: TabletToolKind) -> &'static str {
    match k {
        TabletToolKind::Eraser => "eraser",
        TabletToolKind::Pen => "pen",
        _ => "other",
    }
}

fn ev_from(pos: (f64, f64), scale: f64, d: &winit::event::TabletToolData) -> Ev {
    let p = match d.force {
        Some(Force::Normalized(v)) => v,
        Some(Force::Calibrated { force, .. }) => force,
        None => -1.0,
    };
    let (tx, ty) = d.tilt.map(|t| (t.x as f64, t.y as f64)).unwrap_or((f64::NAN, f64::NAN));
    Ev { x: pos.0 / scale, y: pos.1 / scale, p, tx, ty, d: f64::NAN }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, el: &dyn ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("inkwell spike (b) winit")
            .with_platform_attributes(Box::new(winit::platform::wayland::WindowAttributesWayland::default().with_name("inkspike-winit", "")));
        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("create_window: {e}");
                el.exit();
                return;
            }
        };
        let (Ok(dh), Ok(wh)) = (window.rwh_06_display_handle().display_handle(), window.rwh_06_window_handle().window_handle()) else {
            el.exit();
            return;
        };
        let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) = (dh.as_raw(), wh.as_raw()) else {
            eprintln!("not on wayland");
            el.exit();
            return;
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // SAFETY: window outlives the renderer (drop order in Gfx).
        let surf = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(dh.as_raw()),
                raw_window_handle: wh.as_raw(),
            })
        };
        let sz = window.surface_size();
        let r = match surf.map_err(anyhow::Error::from).and_then(|s| common::gpu::Renderer::new(&instance, s, sz.width, sz.height)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("renderer: {e}");
                el.exit();
                return;
            }
        };
        // Guest wayland-client connection on winit's display for wp_presentation.
        // SAFETY: winit's display lives as long as the event loop.
        let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(d.display.as_ptr() as *mut _) };
        let conn = Connection::from_backend(backend);
        let Ok((globals, pres_q)) = registry_queue_init::<Pres>(&conn) else {
            el.exit();
            return;
        };
        let qh = pres_q.handle();
        let Ok(presentation) = globals.bind::<wp_presentation::WpPresentation, _, _>(&qh, 1..=1, ()) else {
            eprintln!("no wp_presentation");
            el.exit();
            return;
        };
        // SAFETY: pointer is winit's live wl_surface.
        let sid = unsafe {
            wayland_backend::client::ObjectId::from_ptr(WlSurface::interface(), w.surface.as_ptr() as *mut _)
        };
        let Ok(surface) = sid.and_then(|id| WlSurface::from_id(&conn, id)) else {
            el.exit();
            return;
        };
        self.log.meta(serde_json::json!({
            "variant":"winit","winit":"0.31.0-beta.3","sched": if self.immediate {"immediate"} else {"redraw"},
            "present": format!("{:?}", r.config.present_mode), "frame_latency": r.config.desired_maximum_frame_latency,
            "predict_us": self.predict_ns/1000, "scale": window.scale_factor(),
        }));
        self.gfx = Some(Gfx { r, pres_q, pres: Pres { log_tx: Some(Log::from_env_suffix(".pres")) }, presentation, surface, _conn: conn, window });
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &dyn ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let ns = mono_ns();
        let scale = self.gfx.as_ref().map(|g| g.window.scale_factor()).unwrap_or(1.0);
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::SurfaceResized(s) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.r.resize(s.width, s.height);
                    g.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::PointerEntered { kind: PointerKind::TabletTool(k), position, .. } => {
                self.log.ev(ns, "prox_in", kind_name(k), Ev { x: position.x / scale, y: position.y / scale, ..Default::default() }, None, false);
            }
            WindowEvent::PointerLeft { kind: PointerKind::TabletTool(k), .. } => {
                self.log.ev(ns, "prox_out", kind_name(k), Ev::default(), None, false);
            }
            WindowEvent::PointerMoved { position, source: PointerSource::TabletTool { kind, data }, .. } => {
                let ev = ev_from((position.x, position.y), scale, &data);
                if self.down {
                    self.log.ev(ns, "motion", kind_name(kind), ev, None, false);
                    if let Some(s) = self.strokes.last_mut() {
                        s.pts.push(Pt { x: position.x as f32, y: position.y as f32, p: ev.p.max(0.0) as f32, ns });
                    }
                    self.ink_changed();
                } else {
                    self.log.ev(ns, "hover", kind_name(kind), ev, None, false);
                }
            }
            WindowEvent::PointerButton { state, position, button: ButtonSource::TabletTool { kind, button, data }, .. } => {
                let ev = ev_from((position.x, position.y), scale, &data);
                let pressed = state == ElementState::Pressed;
                match button {
                    TabletToolButton::Contact if pressed && !self.down => {
                        // NB: winit maps BTN_STYLUS (0x14b) to Contact as well; distinguish by state.
                        self.down = true;
                        self.log.ev(ns, "down", kind_name(kind), ev, None, false);
                        self.strokes.push(Stroke {
                            pts: vec![Pt { x: position.x as f32, y: position.y as f32, p: ev.p.max(0.0) as f32, ns }],
                            eraser: kind == TabletToolKind::Eraser,
                        });
                        self.ink_changed();
                    }
                    TabletToolButton::Contact if !pressed && self.down => {
                        self.down = false;
                        self.log.ev(ns, "up", kind_name(kind), ev, None, false);
                        self.ink_changed();
                    }
                    b => {
                        self.log.ev(ns, if pressed { "btn_down" } else { "btn_up" }, kind_name(kind), Ev { d: f64::NAN, ..ev }, None, false);
                        let _ = b;
                    }
                }
            }
            // Mouse drawing (fixed pressure) for the owner's rough feel test; logged as m_*.
            WindowEvent::PointerButton { state, position, button: ButtonSource::Mouse(winit::event::MouseButton::Left), .. } => {
                let down = state == ElementState::Pressed;
                self.mouse_down = down;
                let ev = Ev { x: position.x / scale, y: position.y / scale, p: 0.5, ..Default::default() };
                self.log.ev(ns, if down { "m_down" } else { "m_up" }, "mouse", ev, None, false);
                if down {
                    self.strokes.push(Stroke { pts: vec![Pt { x: position.x as f32, y: position.y as f32, p: 0.5, ns }], eraser: false });
                }
                self.ink_changed();
            }
            WindowEvent::PointerMoved { position, source: PointerSource::Mouse, .. } if self.mouse_down => {
                let ev = Ev { x: position.x / scale, y: position.y / scale, p: 0.5, ..Default::default() };
                self.log.ev(ns, "m_motion", "mouse", ev, None, false);
                if let Some(s) = self.strokes.last_mut() {
                    s.pts.push(Pt { x: position.x as f32, y: position.y as f32, p: 0.5, ns });
                }
                self.ink_changed();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &dyn ActiveEventLoop) {
        if let Some(g) = self.gfx.as_mut() {
            let _ = g.pres_q.dispatch_pending(&mut g.pres);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let el = EventLoop::new()?;
    let app = App {
        log: Log::from_env(),
        gfx: None,
        strokes: vec![],
        uploaded: (0, 0),
        down: false,
        immediate: std::env::var("INK_SCHED").as_deref() == Ok("immediate"),
        predict_ns: env_or("INK_PREDICT_US", 0u64) * 1000,
        next_frame: 0,
        mouse_down: false,
    };
    el.run_app(app)?;
    Ok(())
}
