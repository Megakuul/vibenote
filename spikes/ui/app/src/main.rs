//! Phase 2 spike 1 — UI layer on the locked input/render path (ADR 0001).
//!
//! Direct Wayland client (tablet-v2, pointer, keyboard/xkb) + wgpu. Layers per frame:
//!   1. ink canvas (common::gpu pipeline)
//!   2. canvas rich-text box (cosmic-text layout, glyphon rendering)
//!   3. app chrome with egui (side panel folder tree, toolbar, dialog)
//! Pen events over the canvas draw immediately; egui runs only when UI input arrived or it
//! asked for a repaint — otherwise its last tessellation is re-drawn (UI_RERUN=always disables
//! that cache, for comparison).
//!
//! Env: INK_LOG, INK_PRESENT (default mailbox), UI_RERUN=always, UI_NOTES (default 10000),
//! UI_EXPAND=1 (expand all folders), UI_AUTOSCROLL=px/frame, TEXT_FOCUS=1, TEXT_WORDS=n

use common::ink::{self, Pt, Stroke};
use common::log::{Ev, Log};
use common::{env_or, mono_ns};
use glyphon::{Attrs, AttrsList, Buffer, Cache, Color, Edit, Editor, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use std::os::fd::{AsFd, AsRawFd};
use std::ptr::NonNull;
use wayland_client::protocol::{wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1 as fsm, wp_fractional_scale_v1 as fs};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback as pfb};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2 as tm, zwp_tablet_pad_group_v2 as padg, zwp_tablet_pad_v2 as pad, zwp_tablet_seat_v2 as tseat,
    zwp_tablet_tool_v2 as tool, zwp_tablet_v2 as tab,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use xkbcommon::xkb;

const PANEL_W: f32 = 300.0;
const TOP_H: f32 = 40.0;

// ---------------- workspace model (fake data) ----------------

struct Folder {
    name: String,
    color: egui::Color32,
    notes: Vec<String>,
    open: bool,
}

enum Row {
    Folder(usize),
    Note(usize, usize),
}

fn make_tree(n_notes: usize) -> Vec<Folder> {
    let per = 20usize;
    let nf = n_notes.div_ceil(per);
    let pal = [egui::Color32::from_rgb(214, 96, 77), egui::Color32::from_rgb(90, 140, 210), egui::Color32::from_rgb(110, 170, 100), egui::Color32::from_rgb(200, 160, 60)];
    (0..nf)
        .map(|f| Folder {
            name: format!("Folder {f:05}"),
            color: pal[f % pal.len()],
            notes: (0..per.min(n_notes - f * per)).map(|n| format!("Note {f:05}-{n:02} Vorlesung / lecture")).collect(),
            open: false,
        })
        .collect()
}

fn rows(t: &[Folder]) -> Vec<Row> {
    let mut r = vec![];
    for (i, f) in t.iter().enumerate() {
        r.push(Row::Folder(i));
        if f.open {
            for j in 0..f.notes.len() {
                r.push(Row::Note(i, j));
            }
        }
    }
    r
}

// ---------------- app ----------------

#[derive(Default)]
struct ToolSt {
    x: f64,
    y: f64,
    p: f64,
    down: bool,
    f_down: bool,
    f_up: bool,
    f_motion: bool,
}

struct Ui {
    ctx: egui::Context,
    events: Vec<egui::Event>,
    mods: egui::Modifiers,
    tree: Vec<Folder>,
    rows: Vec<Row>,
    selected: Option<(usize, usize)>,
    dialog: bool,
    tool: usize,
    dirty: bool,
    prims: Vec<egui::ClippedPrimitive>,
    tex: egui::TexturesDelta,
    autoscroll: f32,
    scroll: f32,
    rerun_always: bool,
}

struct Text {
    fs: FontSystem,
    swash: SwashCache,
    editor: Editor<'static>,
    focused: bool,
    origin: (f32, f32),
    bold_next: bool,
}

struct App {
    log: Log,
    compositor: Option<wl_compositor::WlCompositor>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    tablet_mgr: Option<tm::ZwpTabletManagerV2>,
    presentation: Option<wp_presentation::WpPresentation>,
    fsm: Option<fsm::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    viewport: Option<wp_viewport::WpViewport>,
    configured: bool,
    closed: bool,
    resized: bool,
    logical: (i32, i32),
    scale120: u32,
    tools: Vec<(tool::ZwpTabletToolV2, ToolSt)>,
    strokes: Vec<Stroke>,
    uploaded: (usize, usize),
    ink_dirty: bool,
    pointer: (f32, f32),
    xkb_ctx: xkb::Context,
    xkb_state: Option<xkb::State>,
    ui: Ui,
    text: Text,
    text_dirty: bool,
    key_t: Option<u64>,
}

impl App {
    fn scale(&self) -> f32 {
        self.scale120 as f32 / 120.0
    }
    fn phys(&self) -> (u32, u32) {
        let s = self.scale();
        ((self.logical.0 as f32 * s).round() as u32, (self.logical.1 as f32 * s).round() as u32)
    }
    fn in_canvas(&self, x: f32, y: f32) -> bool {
        x > PANEL_W && y > TOP_H && !self.ui.dialog
    }
    fn in_text(&self, x: f32, y: f32) -> bool {
        let (ox, oy) = self.text.origin;
        let (w, h) = self.text.editor.with_buffer(|b| (b.size().0.unwrap_or(0.0), b.layout_runs().map(|r| r.line_height).sum::<f32>()));
        x >= ox && x <= ox + w && y >= oy && y <= oy + h.max(30.0)
    }
}

fn build_ui(u: &mut Ui, root: &mut egui::Ui, n_notes: usize) {
    let ctx = &root.ctx().clone();
    egui::Panel::top("top").exact_size(TOP_H).show(root, |ui| {
        ui.horizontal_centered(|ui| {
            for (i, n) in ["Pen", "Highlighter", "Eraser", "Lasso", "Shapes", "Ruler", "Text"].iter().enumerate() {
                if ui.selectable_label(u.tool == i, *n).clicked() {
                    u.tool = i;
                }
            }
            ui.separator();
            if ui.button("Settings…").clicked() {
                u.dialog = true;
            }
        });
    });
    egui::Panel::left("tree").exact_size(PANEL_W).resizable(false).show(root, |ui| {
        ui.heading(format!("Workspace ({n_notes} notes)"));
        let row_h = 22.0;
        let mut toggle = None;
        let mut sa = egui::ScrollArea::vertical().auto_shrink(false);
        if u.autoscroll > 0.0 {
            u.scroll += u.autoscroll;
            sa = sa.vertical_scroll_offset(u.scroll);
        }
        sa.show_rows(ui, row_h, u.rows.len(), |ui, range| {
            for r in &u.rows[range] {
                match *r {
                    Row::Folder(i) => {
                        let f = &u.tree[i];
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, f.color);
                            if ui.selectable_label(false, format!("{} {}", if f.open { "▾" } else { "▸" }, f.name)).clicked() {
                                toggle = Some(i);
                            }
                        });
                    }
                    Row::Note(i, j) => {
                        ui.horizontal(|ui| {
                            ui.add_space(22.0);
                            if ui.selectable_label(u.selected == Some((i, j)), &u.tree[i].notes[j]).clicked() {
                                u.selected = Some((i, j));
                            }
                        });
                    }
                }
            }
        });
        if let Some(i) = toggle {
            u.tree[i].open = !u.tree[i].open;
            u.rows = rows(&u.tree);
        }
    });
    if u.dialog {
        let mut open = true;
        egui::Window::new("Settings").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.label("Handwriting block idle threshold");
            let mut v = 1.5f32;
            ui.add(egui::Slider::new(&mut v, 0.5..=10.0).suffix(" s"));
            ui.checkbox(&mut true.clone(), "Recognise handwriting for search");
            if ui.button("Close").clicked() {
                u.dialog = false;
            }
        });
        if !open {
            u.dialog = false;
        }
    }
}

fn main() -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let n_notes: usize = env_or("UI_NOTES", 10_000usize);
    let mut tree = make_tree(n_notes);
    if env_or("UI_EXPAND", 0u32) == 1 {
        for f in &mut tree {
            f.open = true;
        }
    }
    let r = rows(&tree);
    // cosmic-text box with rich text.
    let t0 = mono_ns();
    let mut fs = FontSystem::new();
    let font_load_ms = (mono_ns() - t0) as f64 / 1e6;
    let mut buf = Buffer::new(&mut fs, Metrics::new(18.0, 26.0));
    buf.set_size(Some(520.0), None);
    let words: usize = env_or("TEXT_WORDS", 60usize);
    let base = Attrs::new().family(Family::SansSerif);
    let mut spans: Vec<(String, Attrs)> = vec![("Vorlesungsnotizen — ".into(), base.clone().weight(Weight::BOLD)), ("Übung 3: ".into(), base.clone().color(Color::rgb(180, 40, 40)))];
    let lorem = "Gegeben sei die Funktion f und ein Beispiel for mixed English text with umlauts äöüß ";
    let mut body = String::new();
    while body.split_whitespace().count() < words {
        body.push_str(lorem);
    }
    spans.push((body, base.clone()));
    let t1 = mono_ns();
    buf.set_rich_text(spans.iter().map(|(s, a)| (s.as_str(), a.clone())), &base, Shaping::Advanced, None);
    buf.shape_until_scroll(&mut fs, false);
    let shape_ms = (mono_ns() - t1) as f64 / 1e6;
    let editor = Editor::new(buf);

    let mut app = App {
        log: Log::from_env(),
        compositor: None,
        wm: None,
        seat: None,
        tablet_mgr: None,
        presentation: None,
        fsm: None,
        viewporter: None,
        viewport: None,
        configured: false,
        closed: false,
        resized: true,
        logical: (1200, 800),
        scale120: 120,
        tools: vec![],
        strokes: vec![],
        uploaded: (0, 0),
        ink_dirty: true,
        pointer: (0.0, 0.0),
        xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb_state: None,
        ui: Ui {
            ctx: egui::Context::default(),
            events: vec![],
            mods: Default::default(),
            tree,
            rows: r,
            selected: None,
            dialog: false,
            tool: 0,
            dirty: true,
            prims: vec![],
            tex: Default::default(),
            autoscroll: env_or("UI_AUTOSCROLL", 0.0f32),
            scroll: 0.0,
            rerun_always: std::env::var("UI_RERUN").as_deref() == Ok("always"),
        },
        text: Text { fs, swash: SwashCache::new(), editor, focused: env_or("TEXT_FOCUS", 0u32) == 1, origin: (PANEL_W + 80.0, TOP_H + 60.0), bold_next: false },
        text_dirty: true,
        key_t: None,
    };
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    let compositor = app.compositor.clone().ok_or_else(|| anyhow::anyhow!("no compositor"))?;
    let wm = app.wm.clone().ok_or_else(|| anyhow::anyhow!("no xdg_wm_base"))?;
    let surface = compositor.create_surface(&qh, ());
    if let (Some(f), Some(vp)) = (&app.fsm, &app.viewporter) {
        f.get_fractional_scale(&surface, &qh, ());
        app.viewport = Some(vp.get_viewport(&surface, &qh, ()));
    }
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let top = xs.get_toplevel(&qh, ());
    top.set_title("vibenote spike — UI layer".into());
    top.set_app_id("vibespike-ui".into());
    surface.commit();
    if let Some(s) = &app.seat {
        s.get_pointer(&qh, ());
        s.get_keyboard(&qh, ());
        if let Some(m) = &app.tablet_mgr {
            m.get_tablet_seat(s, &qh, ());
        }
    }
    while !app.configured {
        queue.blocking_dispatch(&mut app)?;
    }

    let dptr = NonNull::new(conn.backend().display_ptr() as *mut std::ffi::c_void).ok_or_else(|| anyhow::anyhow!("null"))?;
    let sptr = NonNull::new(surface.id().as_ptr() as *mut std::ffi::c_void).ok_or_else(|| anyhow::anyhow!("null"))?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    // SAFETY: display/surface outlive the renderer.
    let wsurf = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(dptr))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(sptr)),
        })?
    };
    if std::env::var("INK_PRESENT").is_err() {
        // SAFETY: single-threaded at this point.
        unsafe { std::env::set_var("INK_PRESENT", "mailbox") };
    }
    let (pw, ph) = app.phys();
    let mut r = common::gpu::Renderer::new(&instance, wsurf, pw, ph)?;
    let fmt = r.config.format;
    let mut egui_r = egui_wgpu::Renderer::new(&r.device, fmt, egui_wgpu::RendererOptions::default());
    let gcache = Cache::new(&r.device);
    let mut gview = Viewport::new(&r.device, &gcache);
    let mut atlas = TextAtlas::new(&r.device, &r.queue, &gcache, fmt);
    let mut trend = TextRenderer::new(&mut atlas, &r.device, wgpu::MultisampleState::default(), None);
    app.log.meta(serde_json::json!({"variant":"ui-spike","notes":n_notes,"rows":app.ui.rows.len(),"rerun_always":app.ui.rerun_always,
        "present": format!("{:?}", r.config.present_mode), "font_load_ms": font_load_ms, "text_words": words, "text_shape_ms": shape_ms}));

    let fd = conn.backend().poll_fd().as_raw_fd();
    let start = std::time::Instant::now();
    let mut next_frame = 0u64;
    while !app.closed {
        let need = app.ink_dirty || app.ui.dirty || app.text_dirty || app.ui.rerun_always || app.ui.autoscroll > 0.0;
        if need {
            // drain queued input first
            conn.flush().ok();
            if let Some(g) = queue.prepare_read() {
                let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                // SAFETY: one valid pollfd.
                if unsafe { libc::poll(&mut p, 1, 0) } > 0 {
                    let _ = g.read();
                }
            }
            queue.dispatch_pending(&mut app)?;
            if app.resized {
                let (w, h) = app.phys();
                r.resize(w, h);
                if let Some(vp) = &app.viewport {
                    vp.set_destination(app.logical.0, app.logical.1);
                }
                app.resized = false;
                app.ui.dirty = true;
                app.text_dirty = true;
            }
            let scale = app.scale();
            let f = next_frame;
            next_frame += 1;
            // 1. egui (only if needed)
            let t_ui = mono_ns();
            let mut ran_ui = false;
            if app.ui.dirty || app.ui.rerun_always || app.ui.autoscroll > 0.0 {
                let ri = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(app.logical.0 as f32, app.logical.1 as f32))),
                    time: Some(start.elapsed().as_secs_f64()),
                    events: std::mem::take(&mut app.ui.events),
                    ..Default::default()
                };
                app.ui.ctx.set_pixels_per_point(scale);
                let ctx = app.ui.ctx.clone();
                let out = ctx.run_ui(ri, |ui| build_ui(&mut app.ui, ui, n_notes));
                app.ui.prims = ctx.tessellate(out.shapes, out.pixels_per_point);
                app.ui.tex.append(out.textures_delta);
                app.ui.dirty = out.viewport_output.values().any(|v| v.repaint_delay.is_zero());
                ran_ui = true;
            }
            let ui_cpu = mono_ns() - t_ui;
            // 2. text layout for the canvas text box
            let t_txt = mono_ns();
            if app.text_dirty {
                app.text.editor.shape_as_needed(&mut app.text.fs, false);
            }
            gview.update(&r.queue, Resolution { width: r.config.width, height: r.config.height });
            let (ox, oy) = app.text.origin;
            let text_ok = app.text.editor.with_buffer(|b| {
                trend.prepare(
                    &r.device,
                    &r.queue,
                    &mut app.text.fs,
                    &mut atlas,
                    &gview,
                    [TextArea {
                        buffer: b,
                        left: ox * scale,
                        top: oy * scale,
                        scale,
                        bounds: TextBounds { left: 0, top: 0, right: r.config.width as i32, bottom: r.config.height as i32 },
                        default_color: Color::rgb(20, 22, 30),
                        custom_glyphs: &[],
                    }],
                    &mut app.text.swash,
                )
            });
            app.text_dirty = false;
            let txt_cpu = mono_ns() - t_txt;
            // 3. ink upload
            let mut v = vec![];
            let (mut si, mut pi) = app.uploaded;
            while si < app.strokes.len() {
                ink::stroke_mesh(&mut v, &app.strokes[si], pi, scale);
                if si + 1 < app.strokes.len() {
                    si += 1;
                    pi = 0;
                } else {
                    pi = app.strokes[si].pts.len();
                    break;
                }
            }
            app.uploaded = (si, pi);
            r.append(&v);
            app.ink_dirty = false;
            let last = app.log.last_ev();
            let pres = app.presentation.clone();
            let (surf, qh2) = (surface.clone(), qh.clone());
            let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [r.config.width, r.config.height], pixels_per_point: scale };
            let prims = &app.ui.prims;
            let tex = std::mem::take(&mut app.ui.tex);
            let t_enc = mono_ns();
            let ok = r.render_ex(
                &[],
                |dev, q, enc, view| {
                    for (id, ds) in &tex.set {
                        for d in ds {
                            egui_r.update_texture(dev, q, *id, d);
                        }
                    }
                    if ran_ui {
                        let extra = egui_r.update_buffers(dev, q, enc, prims, &sd);
                        q.submit(extra);
                    }
                    let mut rp = enc
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("overlay"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        })
                        .forget_lifetime();
                    if text_ok.is_ok() {
                        let _ = trend.render(&atlas, &gview, &mut rp);
                    }
                    egui_r.render(&mut rp, prims, &sd);
                    drop(rp);
                    for id in &tex.free {
                        egui_r.free_texture(id);
                    }
                },
                || {
                    if let Some(p) = pres {
                        p.feedback(&surf, &qh2, f);
                    }
                },
            );
            let enc_cpu = mono_ns() - t_enc;
            if ok {
                let now = mono_ns();
                app.log.draw(f, now, last);
                app.log.raw(format!(r#"{{"k":"ui","f":{f},"ran_ui":{ran_ui},"ui_us":{},"text_us":{},"enc_us":{},"prims":{},"key_to_submit_us":{}}}"#,
                    ui_cpu / 1000, txt_cpu / 1000, enc_cpu / 1000, app.ui.prims.len(),
                    app.key_t.take().map(|k| ((now - k) / 1000).to_string()).unwrap_or("null".into())));
            }
            atlas.trim();
            conn.flush().ok();
            continue;
        }
        conn.flush().ok();
        if let Some(g) = queue.prepare_read() {
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut p, 1, -1) } > 0 {
                let _ = g.read();
            }
        }
        queue.dispatch_pending(&mut app)?;
    }
    Ok(())
}

// ---------------- keyboard → text box / egui ----------------

fn key_event(a: &mut App, key: u32, pressed: bool) {
    let Some(st) = a.xkb_state.as_mut() else { return };
    let code = xkb::Keycode::new(key + 8);
    let sym = st.key_get_one_sym(code);
    let utf8 = st.key_get_utf8(code);
    st.update_key(code, if pressed { xkb::KeyDirection::Down } else { xkb::KeyDirection::Up });
    if !pressed {
        return;
    }
    let ctrl = a.ui.mods.ctrl;
    if a.text.focused {
        use glyphon::Action;
        use cosmic_text::Motion;
        let fs = &mut a.text.fs;
        let ed = &mut a.text.editor;
        let mut ed = ed.borrow_with(fs);
        match sym {
            xkb::Keysym::BackSpace => ed.action(Action::Backspace),
            xkb::Keysym::Delete => ed.action(Action::Delete),
            xkb::Keysym::Return => ed.action(Action::Enter),
            xkb::Keysym::Left => ed.action(Action::Motion(Motion::Left)),
            xkb::Keysym::Right => ed.action(Action::Motion(Motion::Right)),
            xkb::Keysym::Up => ed.action(Action::Motion(Motion::Up)),
            xkb::Keysym::Down => ed.action(Action::Motion(Motion::Down)),
            xkb::Keysym::Escape => a.text.focused = false,
            xkb::Keysym::b if ctrl => a.text.bold_next = !a.text.bold_next,
            _ if !ctrl && !utf8.is_empty() && !utf8.chars().any(|c| c.is_control()) => {
                let attrs = Attrs::new().family(Family::SansSerif);
                let attrs = if a.text.bold_next { attrs.weight(Weight::BOLD) } else { attrs };
                ed.insert_string(&utf8, Some(AttrsList::new(&attrs)));
            }
            _ => {}
        }
        a.text_dirty = true;
        a.key_t = Some(mono_ns());
    } else {
        if !utf8.is_empty() && !ctrl && !utf8.chars().any(|c| c.is_control()) {
            a.ui.events.push(egui::Event::Text(utf8));
        }
        a.ui.dirty = true;
    }
}

// ---------------- Wayland dispatch ----------------

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(a: &mut Self, reg: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "wl_compositor" => a.compositor = Some(reg.bind(name, version.min(6), qh, ())),
                "xdg_wm_base" => a.wm = Some(reg.bind(name, version.min(5), qh, ())),
                "wl_seat" => a.seat = Some(reg.bind(name, version.min(7), qh, ())),
                "zwp_tablet_manager_v2" => a.tablet_mgr = Some(reg.bind(name, 1, qh, ())),
                "wp_presentation" => a.presentation = Some(reg.bind(name, 1, qh, ())),
                "wp_fractional_scale_manager_v1" => a.fsm = Some(reg.bind(name, 1, qh, ())),
                "wp_viewporter" => a.viewporter = Some(reg.bind(name, 1, qh, ())),
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
            a.ui.dirty = true;
        }
    }
}
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(a: &mut Self, _: &xdg_toplevel::XdgToplevel, e: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 && (width, height) != a.logical => {
                a.logical = (width, height);
                a.resized = true;
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
        }
    }
}
impl Dispatch<pfb::WpPresentationFeedback, u64> for App {
    fn event(a: &mut Self, _: &pfb::WpPresentationFeedback, e: pfb::Event, f: &u64, _: &Connection, _: &QueueHandle<Self>) {
        match e {
            pfb::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, .. } => {
                let ns = (((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000 + tv_nsec as u64;
                a.log.pres(*f, ns, refresh as u64, 0);
            }
            pfb::Event::Discarded => a.log.disc(*f),
            _ => {}
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(a: &mut Self, _: &wl_pointer::WlPointer, e: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                a.pointer = (surface_x as f32, surface_y as f32);
                a.ui.events.push(egui::Event::PointerMoved(egui::pos2(a.pointer.0, a.pointer.1)));
                a.ui.dirty = true;
            }
            wl_pointer::Event::Leave { .. } => {
                a.ui.events.push(egui::Event::PointerGone);
                a.ui.dirty = true;
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                let (x, y) = a.pointer;
                if pressed && a.in_canvas(x, y) {
                    a.text.focused = a.in_text(x, y);
                    a.text_dirty = true;
                }
                let b = match button {
                    0x110 => egui::PointerButton::Primary,
                    0x111 => egui::PointerButton::Secondary,
                    _ => egui::PointerButton::Middle,
                };
                a.ui.events.push(egui::Event::PointerButton { pos: egui::pos2(x, y), button: b, pressed, modifiers: a.ui.mods });
                a.ui.dirty = true;
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                let d = if matches!(axis, WEnum::Value(wl_pointer::Axis::VerticalScroll)) { egui::vec2(0.0, -value as f32) } else { egui::vec2(-value as f32, 0.0) };
                a.ui.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: d, modifiers: a.ui.mods, phase: egui::TouchPhase::Move });
                a.ui.dirty = true;
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(a: &mut Self, _: &wl_keyboard::WlKeyboard, e: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                // SAFETY: fd/size from the compositor's keymap event.
                let km = unsafe { xkb::Keymap::new_from_fd(&a.xkb_ctx, fd.as_fd().as_raw_fd().try_into().ok().map(|_: i32| fd).unwrap_or_else(|| unreachable_fd()), size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) };
                if let Ok(Some(km)) = km {
                    a.xkb_state = Some(xkb::State::new(&km));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(st) = a.xkb_state.as_mut() {
                    st.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                    a.ui.mods = egui::Modifiers {
                        ctrl: st.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE),
                        shift: st.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE),
                        alt: st.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE),
                        ..Default::default()
                    };
                }
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                key_event(a, key, matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}
fn unreachable_fd() -> std::os::fd::OwnedFd {
    // Only used in an impossible branch above; spike code.
    std::fs::File::open("/dev/null").map(Into::into).unwrap_or_else(|_| std::process::exit(1))
}

impl Dispatch<tseat::ZwpTabletSeatV2, ()> for App {
    fn event(a: &mut Self, _: &tseat::ZwpTabletSeatV2, e: tseat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let tseat::Event::ToolAdded { id } = e {
            a.tools.push((id, ToolSt::default()));
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
        let Some(i) = a.tools.iter().position(|(x, _)| x == t) else { return };
        let scale = a.scale();
        let st = &mut a.tools[i].1;
        match e {
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
            tool::Event::Frame { time } => {
                let ev = Ev { x: st.x, y: st.y, p: st.p, ..Default::default() };
                let (fd, fu, fm, down) = (st.f_down, st.f_up, st.f_motion, st.down);
                st.f_down = false;
                st.f_up = false;
                st.f_motion = false;
                let (x, y) = (ev.x as f32, ev.y as f32);
                let over_canvas = a.in_canvas(x, y);
                let pt = Pt { x: x * scale, y: y * scale, p: ev.p as f32, ns };
                if fd && over_canvas {
                    a.log.ev(ns, "down", "pen", ev, Some(time), false);
                    a.strokes.push(Stroke { pts: vec![pt], eraser: false });
                    a.ink_dirty = true;
                } else if fm && down && a.strokes.last().is_some() && over_canvas {
                    a.log.ev(ns, "motion", "pen", ev, Some(time), false);
                    if let Some(s) = a.strokes.last_mut() {
                        s.pts.push(pt);
                    }
                    a.ink_dirty = true;
                } else if !over_canvas {
                    // pen over chrome acts as a pointer for egui
                    a.ui.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
                    if fd || fu {
                        a.ui.events.push(egui::Event::PointerButton { pos: egui::pos2(x, y), button: egui::PointerButton::Primary, pressed: fd, modifiers: a.ui.mods });
                    }
                    a.ui.dirty = true;
                }
                if fu {
                    a.log.ev(ns, "up", "pen", ev, Some(time), false);
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<pad::ZwpTabletPadV2, ()> for App {
    fn event(_: &mut Self, _: &pad::ZwpTabletPadV2, _: pad::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(App, pad::ZwpTabletPadV2, [pad::EVT_GROUP_OPCODE => (padg::ZwpTabletPadGroupV2, ())]);
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_compositor::WlCompositor, wl_surface::WlSurface, wl_seat::WlSeat, tm::ZwpTabletManagerV2, tab::ZwpTabletV2, padg::ZwpTabletPadGroupV2,
    fsm::WpFractionalScaleManagerV1, wp_viewporter::WpViewporter, wp_viewport::WpViewport, wp_presentation::WpPresentation, wl_callback::WlCallback);
