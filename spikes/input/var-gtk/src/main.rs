//! Variant (a): GTK4 with the toolkit's own tablet input and GSK rendering.
//!
//! Input: EventControllerLegacy (sees every GdkEvent incl. motion history from GTK's motion
//! compression). Rendering: custom widget snapshot with GskPath fills (GPU via GSK renderer);
//! completed strokes are cached as render nodes. Timing: GdkFrameClock frame timings
//! (presentation_time comes from wp_presentation on Wayland).
//!
//! Knobs (env): INK_LOG=path, INK_PREDICT_US=0

use common::ink::{self, Pt, Stroke};
use common::log::{Ev, Log};
use common::{env_or, mono_ns};
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
pub struct State {
    log: Option<Log>,
    strokes: Vec<Stroke>,
    cached: Vec<gsk::RenderNode>,
    down: bool,
    pending_frames: Vec<i64>,
    predict_ns: u64,
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct InkArea {
        pub st: RefCell<Option<Rc<RefCell<State>>>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for InkArea {
        const NAME: &'static str = "InkArea";
        type Type = super::InkArea;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for InkArea {}
    impl WidgetImpl for InkArea {
        fn snapshot(&self, snap: &gtk::Snapshot) {
            let Some(st) = self.st.borrow().clone() else { return };
            let mut s = st.borrow_mut();
            let w = self.obj();
            let (ww, wh) = (w.width() as f32, w.height() as f32);
            let p = ink::PAPER;
            snap.append_color(&gdk::RGBA::new(p[0], p[1], p[2], 1.0), &graphene::Rect::new(0.0, 0.0, ww, wh));
            // Cache finished strokes as render nodes.
            let n_done = if s.down { s.strokes.len().saturating_sub(1) } else { s.strokes.len() };
            while s.cached.len() < n_done {
                let i = s.cached.len();
                let sn = gtk::Snapshot::new();
                fill_stroke(&sn, &s.strokes[i], &[]);
                if let Some(node) = sn.to_node() {
                    s.cached.push(node);
                } else {
                    break;
                }
            }
            for n in &s.cached {
                snap.append_node(n);
            }
            if s.down {
                if let Some(cur) = s.strokes.last() {
                    let pred = ink::predict(cur, s.predict_ns, 8_000_000);
                    fill_stroke(snap, cur, &pred);
                }
            }
            if let Some(fc) = w.frame_clock() {
                let f = fc.frame_counter();
                if let Some(l) = &s.log {
                    l.draw(f as u64, mono_ns(), l.last_ev());
                }
                s.pending_frames.push(f);
            }
        }
    }
}

glib::wrapper! {
    pub struct InkArea(ObjectSubclass<imp::InkArea>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Fill a pressure-width stroke as one GskPath (union of segment quads + discs).
fn fill_stroke(snap: &gtk::Snapshot, s: &Stroke, pred: &[Pt]) {
    let b = gsk::PathBuilder::new();
    let mut pts: Vec<Pt> = s.pts.clone();
    pts.extend_from_slice(pred);
    for (i, p) in pts.iter().enumerate() {
        let r = ink::width(p.p, 1.0) * 0.5;
        b.add_circle(&graphene::Point::new(p.x, p.y), r);
        if i > 0 {
            let a = pts[i - 1];
            let ra = ink::width(a.p, 1.0) * 0.5;
            let (dx, dy) = (p.x - a.x, p.y - a.y);
            let len = (dx * dx + dy * dy).sqrt();
            if len > 1e-3 {
                let (nx, ny) = (-dy / len, dx / len);
                // Same orientation as add_circle so nonzero winding unions instead of cancelling.
                b.move_to(a.x + nx * ra, a.y + ny * ra);
                b.line_to(a.x - nx * ra, a.y - ny * ra);
                b.line_to(p.x - nx * r, p.y - ny * r);
                b.line_to(p.x + nx * r, p.y + ny * r);
                b.close();
            }
        }
    }
    let c = if s.eraser { ink::ERASE } else { ink::INK };
    snap.append_fill(&b.to_path(), gsk::FillRule::Winding, &gdk::RGBA::new(c[0], c[1], c[2], 1.0));
}

fn axes_from_event(e: &gdk::Event) -> Ev {
    let (x, y) = e.position().unwrap_or((0.0, 0.0));
    Ev {
        x,
        y,
        p: e.axis(gdk::AxisUse::Pressure).unwrap_or(-1.0),
        tx: e.axis(gdk::AxisUse::Xtilt).unwrap_or(f64::NAN),
        ty: e.axis(gdk::AxisUse::Ytilt).unwrap_or(f64::NAN),
        d: e.axis(gdk::AxisUse::Distance).unwrap_or(f64::NAN),
    }
}

fn axes_from_tc(tc: &gdk::TimeCoord) -> Ev {
    let a = tc.axes();
    let g = |u: gdk::AxisUse| a.get(u.into_glib() as usize).copied().unwrap_or(f64::NAN);
    Ev {
        x: g(gdk::AxisUse::X),
        y: g(gdk::AxisUse::Y),
        p: g(gdk::AxisUse::Pressure),
        tx: g(gdk::AxisUse::Xtilt),
        ty: g(gdk::AxisUse::Ytilt),
        d: g(gdk::AxisUse::Distance),
    }
}

fn tool_name(e: &gdk::Event) -> &'static str {
    match e.device_tool().map(|t| t.tool_type()) {
        Some(gdk::DeviceToolType::Eraser) => "eraser",
        Some(gdk::DeviceToolType::Pen) => "pen",
        Some(_) => "other",
        None => "mouse",
    }
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id("dev.inkwell.spike.gtk").build();
    app.connect_activate(|app| {
        let st = Rc::new(RefCell::new(State {
            log: Some(Log::from_env()),
            predict_ns: env_or("INK_PREDICT_US", 0u64) * 1000,
            ..Default::default()
        }));
        let area: InkArea = glib::Object::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        *area.imp().st.borrow_mut() = Some(st.clone());

        let ctl = gtk::EventControllerLegacy::new();
        let (st2, area2) = (st.clone(), area.clone());
        ctl.connect_event(move |_, e| {
            let ns = mono_ns();
            let mut s = st2.borrow_mut();
            let tool = tool_name(e);
            let ms = Some(e.time());
            let et = e.event_type();
            let mut redraw = false;
            let mut to_add: Vec<(Ev, bool)> = vec![];
            match et {
                gdk::EventType::MotionNotify => {
                    for tc in e.history() {
                        to_add.push((axes_from_tc(&tc), true));
                    }
                    to_add.push((axes_from_event(e), false));
                }
                gdk::EventType::ButtonPress | gdk::EventType::ButtonRelease => {
                    to_add.push((axes_from_event(e), false));
                }
                _ => {}
            }
            let Some(log) = s.log.as_mut() else { return glib::Propagation::Proceed };
            match et {
                gdk::EventType::ProximityIn => {
                    log.ev(ns, "prox_in", tool, axes_from_event(e), ms, false);
                }
                gdk::EventType::ProximityOut => {
                    log.ev(ns, "prox_out", tool, axes_from_event(e), ms, false);
                }
                gdk::EventType::ButtonPress => {
                    let b = e.downcast_ref::<gdk::ButtonEvent>().map(|b| b.button()).unwrap_or(0);
                    let ev = to_add[0].0;
                    if b == gdk::BUTTON_PRIMARY {
                        log.ev(ns, "down", tool, ev, ms, false);
                        s.down = true;
                        s.strokes.push(Stroke {
                            pts: vec![Pt { x: ev.x as f32, y: ev.y as f32, p: ev.p.max(0.0) as f32, ns }],
                            eraser: tool == "eraser",
                        });
                        redraw = true;
                    } else {
                        log.ev(ns, "btn_down", tool, Ev { d: b as f64, ..ev }, ms, false);
                    }
                }
                gdk::EventType::ButtonRelease => {
                    let b = e.downcast_ref::<gdk::ButtonEvent>().map(|b| b.button()).unwrap_or(0);
                    let ev = to_add[0].0;
                    if b == gdk::BUTTON_PRIMARY {
                        log.ev(ns, "up", tool, ev, ms, false);
                        s.down = false;
                        redraw = true;
                    } else {
                        log.ev(ns, "btn_up", tool, Ev { d: b as f64, ..ev }, ms, false);
                    }
                }
                gdk::EventType::MotionNotify => {
                    let down = s.down;
                    for (ev, hist) in to_add {
                        let log = s.log.as_mut().map(|l| l.ev(ns, if down { "motion" } else { "hover" }, tool, ev, ms, hist));
                        let _ = log;
                        if down {
                            if let Some(st) = s.strokes.last_mut() {
                                st.pts.push(Pt { x: ev.x as f32, y: ev.y as f32, p: ev.p.max(0.0) as f32, ns });
                            }
                            redraw = true;
                        }
                    }
                }
                _ => {}
            }
            drop(s);
            if redraw {
                area2.queue_draw();
            }
            glib::Propagation::Proceed
        });
        area.add_controller(ctl);

        // Collect presentation times for drawn frames.
        let st3 = st.clone();
        area.add_tick_callback(move |_, fc| {
            let mut s = st3.borrow_mut();
            let pend = std::mem::take(&mut s.pending_frames);
            let mut keep = vec![];
            for f in pend {
                match fc.timings(f) {
                    Some(t) if t.is_complete() => {
                        if let Some(l) = &s.log {
                            let pt = t.presentation_time();
                            if pt > 0 {
                                l.pres(f as u64, pt as u64 * 1000, t.refresh_interval() as u64 * 1000, 0);
                            } else {
                                l.disc(f as u64);
                            }
                        }
                    }
                    Some(_) => keep.push(f),
                    None => {
                        if let Some(l) = &s.log {
                            l.disc(f as u64);
                        }
                    }
                }
            }
            s.pending_frames = keep;
            glib::ControlFlow::Continue
        });

        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("inkwell spike (a) gtk4")
            .decorated(false)
            .default_width(1200)
            .default_height(800)
            .child(&area)
            .build();
        win.present();
        if let Some(l) = &st.borrow().log {
            let renderer = win.renderer().map(|r| r.type_().name().to_string());
            l.meta(serde_json::json!({"variant":"gtk","gtk": format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version()), "renderer": renderer, "predict_us": env_or("INK_PREDICT_US", 0u64)}));
        }
    });
    app.run_with_args::<&str>(&[])
}
