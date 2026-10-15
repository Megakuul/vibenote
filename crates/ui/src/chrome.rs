//! App chrome (ADR 0002): an egui toolbar, folder tree, settings, search panel and find bar,
//! drawn as an overlay pass after the canvas. egui runs only when UI input arrived or it asked
//! for a repaint; ink-only frames re-draw its last tessellation, so the pen path never waits on
//! egui. The chrome reads the engine and returns actions; it never changes the engine itself
//! (AGENTS.md §4: UI is thin).

use crate::theme;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use vibenote_engine::Engine;
use vibenote_model::{FolderId, NoteId};
use vibenote_render::wgpu;

/// Toolbar height and sidebar width (logical px).
pub const TOP_H: f32 = 44.0;
pub const SIDE_W: f32 = 260.0;
const ROW_H: f32 = 26.0;
/// Indentation per tree level (logical px).
const INDENT: f32 = 16.0;
/// Search results shown at most.
const MAX_HITS: usize = 200;

/// What the chrome asks the app to do after a run.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run an engine command.
    Cmd(&'static str, Value),
    /// Open a note and centre a canvas region in the view.
    Show { note: u64, region: Option<[f64; 4]> },
    /// Search everything (results come back with [`Chrome::set_results`]).
    Search(String),
    /// Find in the current note.
    Find(String),
    /// Paste what the clipboard holds (files, images, text, or our own objects).
    Paste,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Folder(u64),
    Note(u64),
}

#[derive(Clone, Debug)]
struct Row {
    item: Item,
    depth: usize,
    name: String,
    color: Option<u32>,
    open: bool,
}

/// One search result line.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub note: Option<u64>,
    pub folder: Option<u64>,
    pub source: String,
    pub snippet: String,
    pub region: Option<[f64; 4]>,
}

#[derive(Default)]
struct State {
    sidebar: bool,
    settings: bool,
    search: bool,
    search_text: String,
    searched: String,
    hits: Vec<(String, Vec<Hit>)>,
    indexing: bool,
    find: bool,
    find_text: String,
    found: String,
    regions: Vec<[f64; 4]>,
    rename: Option<(Item, String)>,
    expanded: BTreeSet<u64>,
    /// Tree rows, rebuilt when the workspace or the expanded set changes.
    rows: Vec<Row>,
    rows_key: Option<(u64, u64)>,
    /// Where each visible tree row was laid out last run (logical px; tests and hit checks).
    row_rects: Vec<(String, egui::Rect)>,
    focus_search: bool,
    focus_find: bool,
}

/// Canvas-to-screen mapping of the view the chrome was laid out for (find highlights).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewMap {
    pub origin_x: f64,
    pub origin_y: f64,
    /// Canvas units → logical px.
    pub scale: f64,
}

pub struct Chrome {
    ctx: egui::Context,
    events: Vec<egui::Event>,
    mods: egui::Modifiers,
    prims: Vec<egui::ClippedPrimitive>,
    textures: egui::TexturesDelta,
    dirty: bool,
    /// Rects the chrome covers (logical px), from the last run.
    areas: Vec<egui::Rect>,
    wants_keyboard: bool,
    st: State,
    start: std::time::Instant,
    view: Option<ViewMap>,
    gpu: Option<(egui_wgpu::Renderer, wgpu::TextureFormat)>,
    ppp: f32,
    /// egui asked to run again at this time (cursor blink, animations).
    repaint_at: Option<std::time::Instant>,
    /// The cursor egui wants over the chrome (text fields, buttons).
    cursor: egui::CursorIcon,
}

impl Drop for Chrome {
    /// Texture updates never painted (no GPU frame came) are dropped on purpose; epaint
    /// treats dropping them unapplied as a bug otherwise.
    fn drop(&mut self) {
        self.textures.clear();
    }
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

impl Chrome {
    pub fn new() -> Chrome {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        Chrome {
            ctx,
            events: Vec::new(),
            mods: egui::Modifiers::NONE,
            prims: Vec::new(),
            textures: egui::TexturesDelta::default(),
            dirty: true,
            areas: Vec::new(),
            wants_keyboard: false,
            st: State {
                sidebar: true,
                ..State::default()
            },
            start: std::time::Instant::now(),
            view: None,
            gpu: None,
            ppp: 1.0,
            repaint_at: None,
            cursor: egui::CursorIcon::Default,
        }
    }

    /// True if (x, y) (logical px) is on the chrome: pointer and pen input there go to egui.
    pub fn covers(&self, x: f64, y: f64) -> bool {
        #[allow(clippy::cast_possible_truncation)] // logical px
        let p = egui::pos2(x as f32, y as f32);
        self.areas.iter().any(|r| r.contains(p))
    }

    /// The cursor egui asked for at the last run.
    pub fn cursor(&self) -> egui::CursorIcon {
        self.cursor
    }

    /// A text field has the keyboard (shortcuts go to it, not to the keymap).
    pub fn wants_keyboard(&self) -> bool {
        self.wants_keyboard
    }

    pub fn event(&mut self, e: egui::Event) {
        self.events.push(e);
        self.dirty = true;
    }

    pub fn set_modifiers(&mut self, m: egui::Modifiers) {
        self.mods = m;
    }

    pub fn modifiers(&self) -> egui::Modifiers {
        self.mods
    }

    /// Opens the search panel (Ctrl+Shift+F) or the find bar (Ctrl+F) with the field focused.
    pub fn open_search(&mut self) {
        self.st.search = true;
        self.st.focus_search = true;
        self.dirty = true;
    }

    pub fn open_find(&mut self) {
        self.st.find = true;
        self.st.focus_find = true;
        self.dirty = true;
    }

    /// Closes the find bar and the search panel (Escape in them).
    pub fn close_panels(&mut self) -> bool {
        let was = self.st.find || self.st.search;
        self.st.find = false;
        self.st.search = false;
        self.st.regions.clear();
        self.dirty = true;
        was
    }

    /// Something changed that the chrome shows (workspace, tool, view with find highlights).
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// How long the event loop may sleep before egui wants to run again.
    pub fn wake_in(&self) -> Option<std::time::Duration> {
        self.repaint_at
            .map(|t| t.saturating_duration_since(std::time::Instant::now()))
    }

    /// Marks the chrome dirty if its timed repaint is due; returns whether it did.
    pub fn due(&mut self) -> bool {
        if self.repaint_at.is_some_and(|t| t <= std::time::Instant::now()) {
            self.repaint_at = None;
            self.dirty = true;
            return true;
        }
        false
    }

    /// True if egui must run before the next frame.
    pub fn needs_run(&self, view: ViewMap) -> bool {
        self.dirty || (!self.st.regions.is_empty() && self.view != Some(view))
    }

    pub fn set_results(&mut self, r: &Value) {
        let mut groups = Vec::new();
        let hits = |v: &Value| -> Vec<Hit> {
            v.as_array()
                .into_iter()
                .flatten()
                .map(|h| Hit {
                    note: h["note"].as_u64(),
                    folder: h["folder"].as_u64(),
                    source: h["source"].as_str().unwrap_or("").to_string(),
                    snippet: h["snippet"].as_str().unwrap_or("").to_string(),
                    region: region_of(&h["region"]),
                })
                .collect()
        };
        let folders: Vec<Hit> = hits(&r["results"]).into_iter().filter(|h| h.folder.is_some()).collect();
        if !folders.is_empty() {
            groups.push(("Folders".to_string(), folders));
        }
        for g in r["groups"].as_array().into_iter().flatten() {
            groups.push((g["name"].as_str().unwrap_or("").to_string(), hits(&g["hits"])));
        }
        let mut n = 0usize;
        for g in &mut groups {
            g.1.truncate(MAX_HITS.saturating_sub(n));
            n = n.saturating_add(g.1.len());
        }
        groups.retain(|g| !g.1.is_empty());
        self.st.hits = groups;
        self.st.indexing = r["indexing"].as_bool().unwrap_or(false);
        self.dirty = true;
    }

    pub fn set_regions(&mut self, r: &Value) {
        self.st.regions = r["regions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|h| region_of(&h["region"]))
            .collect();
        self.dirty = true;
    }

    /// Runs egui (call only when [`Chrome::needs_run`]); returns what the user asked for.
    pub fn run(&mut self, eng: &Engine, logical: (f32, f32), ppp: f32, view: ViewMap) -> Vec<Action> {
        self.ppp = ppp;
        self.view = Some(view);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(logical.0, logical.1),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        self.ctx.set_pixels_per_point(ppp);
        let mut actions = Vec::new();
        let mut areas = Vec::new();
        let ctx = self.ctx.clone();
        rebuild_rows(&mut self.st, eng);
        let out = ctx.run_ui(raw, |root| {
            build(&mut self.st, eng, view, root, &mut actions, &mut areas)
        });
        self.areas = areas;
        self.cursor = out.platform_output.cursor_icon;
        self.wants_keyboard = ctx.egui_wants_keyboard_input();
        self.prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        self.textures.append(out.textures_delta);
        let delay = out.viewport_output.values().map(|v| v.repaint_delay).min();
        self.dirty = delay.is_some_and(|d| d.is_zero());
        // A later repaint (cursor blink): bounded, so a huge delay means "none".
        self.repaint_at = delay
            .filter(|d| !d.is_zero() && *d <= std::time::Duration::from_secs(10))
            .and_then(|d| std::time::Instant::now().checked_add(d));
        if std::env::var_os("VIBENOTE_CHROME_DEBUG").is_some() && self.dirty {
            eprintln!("chrome repaint: {:?}", ctx.repaint_causes());
        }
        actions
    }

    /// Draws the last tessellation over `view` (the frame the canvas was drawn into).
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size_px: [u32; 2],
    ) {
        if self.gpu.as_ref().is_none_or(|(_, f)| *f != format) {
            let r = egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
            self.gpu = Some((r, format));
            // A new renderer has no textures: egui sends the font atlas again.
            self.ctx.memory_mut(|m| *m = egui::Memory::default());
            // The memory holds the theme and styles too: set them again (else egui's dark
            // defaults show).
            theme::apply(&self.ctx);
            self.ctx.request_repaint();
            self.dirty = true;
        }
        let Some((r, _)) = self.gpu.as_mut() else { return };
        let tex = std::mem::take(&mut self.textures);
        for (id, deltas) in &tex.set {
            for d in deltas {
                r.update_texture(device, queue, *id, d);
            }
        }
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size_px,
            pixels_per_point: self.ppp,
        };
        let extra = r.update_buffers(device, queue, enc, &self.prims, &sd);
        queue.submit(extra);
        {
            let mut rp = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("chrome"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            r.render(&mut rp, &self.prims, &sd);
        }
        for id in &tex.free {
            r.free_texture(id);
        }
        let mut tex = tex;
        tex.clear(); // applied above
    }
}

fn region_of(v: &Value) -> Option<[f64; 4]> {
    let a = v.as_array()?;
    let mut out = [0.0; 4];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64()?;
    }
    (a.len() == 4).then_some(out)
}

/// The visible tree rows (folders, then notes; expanded folders only). Rebuilt only when the
/// workspace's names/structure (`notes_rev`) or the expanded set change, never per frame.
fn rebuild_rows(st: &mut State, eng: &Engine) {
    let s = &eng.session;
    let key = (
        s.notes_revision(),
        st.expanded
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, f| (h ^ f).wrapping_mul(0x100_0000_01b3)),
    );
    if st.rows_key == Some(key) {
        return;
    }
    st.rows_key = Some(key);
    let mut rows = Vec::new();
    let mut children: std::collections::BTreeMap<Option<FolderId>, Vec<(i64, Item)>> = Default::default();
    for f in s.ws.folders.values() {
        if !s.ws.trashed_folders.contains(&f.id) {
            children
                .entry(f.parent)
                .or_default()
                .push((f.sort, Item::Folder(f.id.0)));
        }
    }
    for n in s.ws.notes.values() {
        if !s.ws.trashed_notes.contains(&n.id) {
            children.entry(n.folder).or_default().push((n.sort, Item::Note(n.id.0)));
        }
    }
    for v in children.values_mut() {
        // Folders first, then by manual order.
        v.sort_by_key(|(sort, it)| (matches!(it, Item::Note(_)), *sort));
    }
    // Iterative depth-first walk (folder depth is user data: no recursion).
    let mut stack: Vec<(Item, usize)> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|(_, it)| (*it, 0))
        .collect();
    while let Some((it, depth)) = stack.pop() {
        match it {
            Item::Folder(id) => {
                let Some(f) = s.ws.folders.get(&FolderId(id)) else {
                    continue;
                };
                let open = st.expanded.contains(&id);
                rows.push(Row {
                    item: it,
                    depth,
                    name: f.name.clone(),
                    color: f.color.map(|c| c.0),
                    open,
                });
                if open {
                    for (_, c) in children.get(&Some(FolderId(id))).into_iter().flatten().rev() {
                        stack.push((*c, depth.saturating_add(1)));
                    }
                }
            }
            Item::Note(id) => {
                let Some(n) = s.ws.notes.get(&NoteId(id)) else { continue };
                rows.push(Row {
                    item: it,
                    depth,
                    name: n.name.clone(),
                    color: n.color.map(|c| c.0),
                    open: false,
                });
            }
        }
    }
    st.rows = rows;
}

/// Tool buttons: icon, tooltip (with the key), command, and whether it is the selected tool.
/// The eraser button switches between stroke and area eraser when pressed again
/// (`tool.select.eraser`).
fn tool_buttons(t: vibenote_model::settings::ToolKind) -> [(&'static str, &'static str, &'static str, bool); 6] {
    use theme::icon;
    use vibenote_model::settings::ToolKind as T;
    [
        (
            icon::PEN,
            "Pen (P) — hold still at the end of a stroke to snap it into a shape",
            "tool.select.pen",
            t == T::Pen,
        ),
        (
            icon::HIGHLIGHTER,
            "Highlighter (H)",
            "tool.select.highlighter",
            t == T::Highlighter,
        ),
        (
            icon::ERASER,
            if t == T::EraserArea {
                "Area eraser (E) — press again for the stroke eraser"
            } else {
                "Stroke eraser (E) — press again for the area eraser"
            },
            "tool.select.eraser",
            matches!(t, T::EraserStroke | T::EraserArea),
        ),
        (
            icon::LASSO,
            "Lasso: select, then drag to move (L)",
            "tool.select.lasso",
            t == T::Lasso,
        ),
        (icon::SHAPES, "Shapes", "tool.select.shape", t == T::Shape),
        (icon::TEXT, "Text: tap to write", "tool.select.text", t == T::Text),
    ]
}

/// What the shape tool draws: icon, tooltip, `settings.set` value.
const SHAPES: [(&str, &str, &str, vibenote_model::ShapeKind); 6] = [
    (theme::icon::LINE, "Line", "line", vibenote_model::ShapeKind::Line),
    (theme::icon::ARROW, "Arrow", "arrow", vibenote_model::ShapeKind::Arrow),
    (
        theme::icon::RECTANGLE,
        "Rectangle",
        "rectangle",
        vibenote_model::ShapeKind::Rectangle,
    ),
    (
        theme::icon::ELLIPSE,
        "Ellipse",
        "ellipse",
        vibenote_model::ShapeKind::Ellipse,
    ),
    (
        theme::icon::TRIANGLE,
        "Triangle",
        "triangle",
        vibenote_model::ShapeKind::Triangle,
    ),
    (
        theme::icon::POLYGON,
        "Polygon: tap the corners, tap the first one to close",
        "polygon",
        vibenote_model::ShapeKind::Polygon,
    ),
];

/// Text formats while typing: icon, tooltip, `text.style` format.
const TEXT_STYLES: [(&str, &str, &str); 10] = [
    (theme::icon::BOLD, "Bold (Ctrl+B)", "bold"),
    (theme::icon::ITALIC, "Italic (Ctrl+I)", "italic"),
    (theme::icon::UNDERLINE, "Underline (Ctrl+U)", "underline"),
    (theme::icon::STRIKE, "Strike through", "strike"),
    (theme::icon::H1, "Heading 1", "heading1"),
    (theme::icon::H2, "Heading 2", "heading2"),
    (theme::icon::H3, "Heading 3", "heading3"),
    (theme::icon::BULLETS, "Bullet list", "bullet_list"),
    (theme::icon::NUMBERS, "Numbered list", "numbered_list"),
    (theme::icon::CHECKS, "Checkbox", "checkbox"),
];

/// A square icon button for the toolbar; `on` draws it pressed.
fn icon_button(ui: &mut egui::Ui, icon: &str, on: bool, tip: &str) -> egui::Response {
    let text = egui::RichText::new(icon)
        .size(theme::ICON)
        .color(if on { theme::ACCENT } else { theme::TEXT });
    let b = egui::Button::selectable(on, text).min_size(egui::vec2(32.0, 32.0));
    ui.add(b).on_hover_text(tip)
}

/// A thin vertical divider between toolbar groups.
fn divider(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::PANEL_EDGE);
    ui.add_space(4.0);
}

fn build(
    st: &mut State,
    eng: &Engine,
    view: ViewMap,
    root: &mut egui::Ui,
    out: &mut Vec<Action>,
    areas: &mut Vec<egui::Rect>,
) {
    let ctx = root.ctx().clone();
    let s = &eng.session;
    let top = egui::Panel::top("toolbar")
        .exact_size(TOP_H)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(8, 0))
                .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE)),
        )
        .show(root, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, theme::icon::SIDEBAR, st.sidebar, "Notes").clicked() {
                    st.sidebar = !st.sidebar;
                }
                divider(ui);
                for (icon, tip, cmd, on) in tool_buttons(s.settings.tool) {
                    if icon_button(ui, icon, on, tip).clicked() {
                        out.push(Action::Cmd(cmd, json!({})));
                    }
                }
                // What the selected tool can do: shapes to draw, formats while typing.
                if s.settings.tool == vibenote_model::settings::ToolKind::Shape {
                    divider(ui);
                    for (icon, tip, value, kind) in SHAPES {
                        if icon_button(ui, icon, s.settings.shape == kind, tip).clicked() {
                            out.push(Action::Cmd("settings.set", json!({ "shape": value })));
                        }
                    }
                }
                divider(ui);
                if icon_button(
                    ui,
                    theme::icon::PASTE,
                    false,
                    "Paste (Ctrl+V): files, images or text copied anywhere — or drop files onto the page",
                )
                .clicked()
                {
                    out.push(Action::Paste);
                }
                if s.text_edit.is_some() {
                    divider(ui);
                    for (icon, tip, format) in TEXT_STYLES {
                        if icon_button(ui, icon, false, tip).clicked() {
                            out.push(Action::Cmd("text.style", json!({ "format": format })));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon_button(ui, theme::icon::SETTINGS, st.settings, "Settings").clicked() {
                        st.settings = !st.settings;
                    }
                    if icon_button(ui, theme::icon::SEARCH, st.search, "Search everything (Ctrl+Shift+F)").clicked() {
                        st.search = !st.search;
                        st.focus_search = st.search;
                    }
                    let recording = s.audio.is_recording();
                    let (icon, tip) = if recording {
                        (theme::icon::STOP, "Stop recording")
                    } else {
                        (theme::icon::MIC, "Record audio (synced to ink)")
                    };
                    if icon_button(ui, icon, recording, tip).clicked() {
                        out.push(if recording {
                            Action::Cmd("audio.record.stop", json!({}))
                        } else {
                            Action::Cmd("audio.record.start", json!({"source": "device"}))
                        });
                    }
                    divider(ui);
                    if icon_button(ui, theme::icon::FIT, false, "Fit to content (Ctrl+0)").clicked() {
                        out.push(Action::Cmd("view.zoom_fit", json!({})));
                    }
                    if icon_button(ui, theme::icon::ZOOM_IN, false, "Zoom in (Ctrl+=)").clicked() {
                        out.push(Action::Cmd("view.zoom_in", json!({})));
                    }
                    #[allow(clippy::cast_possible_truncation)] // a percentage label
                    let pct = (s.view.zoom * 100.0).round() as i64;
                    let zoom = egui::Button::new(egui::RichText::new(format!("{pct} %")).color(theme::MUTED))
                        .min_size(egui::vec2(52.0, 32.0));
                    if ui.add(zoom).on_hover_text("Actual size (Ctrl+1)").clicked() {
                        out.push(Action::Cmd("view.zoom_reset", json!({})));
                    }
                    if icon_button(ui, theme::icon::ZOOM_OUT, false, "Zoom out (Ctrl+-)").clicked() {
                        out.push(Action::Cmd("view.zoom_out", json!({})));
                    }
                    divider(ui);
                    if icon_button(ui, theme::icon::REDO, false, "Redo (Ctrl+Shift+Z)").clicked() {
                        out.push(Action::Cmd("edit.redo", json!({})));
                    }
                    if icon_button(ui, theme::icon::UNDO, false, "Undo (Ctrl+Z)").clicked() {
                        out.push(Action::Cmd("edit.undo", json!({})));
                    }
                });
            });
        });
    areas.push(top.response.rect);
    if st.sidebar {
        let side = egui::Panel::left("tree")
            .exact_size(SIDE_W)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(8, 8))
                    .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE)),
            )
            .show(root, |ui| {
                tree(st, eng, ui, out);
            });
        areas.push(side.response.rect);
    }
    if st.search {
        let panel = egui::Panel::right("search")
            .exact_size(320.0)
            .resizable(false)
            .show(root, |ui| {
                search_panel(st, ui, out);
            });
        areas.push(panel.response.rect);
    }
    if st.find {
        let w = egui::Window::new("Find in note")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, TOP_H + 8.0))
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.find_text)
                            .hint_text("Find in note")
                            .desired_width(200.0),
                    );
                    if std::mem::take(&mut st.focus_find) {
                        r.request_focus();
                    }
                    ui.label(format!("{}", st.regions.len()));
                    if ui.button("Close").clicked() {
                        st.find = false;
                        st.regions.clear();
                    }
                });
            });
        if let Some(w) = w {
            areas.push(w.response.rect);
        }
        if st.find_text != st.found {
            st.found = st.find_text.clone();
            out.push(Action::Find(st.find_text.clone()));
        }
        // Highlights: the regions found, on the canvas.
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("find")));
        for r in &st.regions {
            #[allow(clippy::cast_possible_truncation)] // logical px
            let rect = egui::Rect::from_min_max(
                egui::pos2(
                    ((r[0] - view.origin_x) * view.scale) as f32,
                    ((r[1] - view.origin_y) * view.scale) as f32,
                ),
                egui::pos2(
                    ((r[2] - view.origin_x) * view.scale) as f32,
                    ((r[3] - view.origin_y) * view.scale) as f32,
                ),
            );
            painter.rect_filled(rect.expand(3.0), 3.0, theme::HIGHLIGHT);
        }
    }
    // The selection's actions, above its box (below it near the toolbar): tidy handwriting
    // (or show it as written again), copy, cut, delete.
    if s.live.drag.is_none()
        && s.text_edit.is_none()
        && let Some(r) = s.selection_rect()
    {
        let (ink, tidied) = s.selection_ink();
        #[allow(clippy::cast_possible_truncation)] // logical px
        let (x, top, bottom) = (
            ((r.min.x - view.origin_x) * view.scale) as f32,
            ((r.min.y - view.origin_y) * view.scale) as f32,
            ((r.max.y - view.origin_y) * view.scale) as f32,
        );
        let y = if top - 92.0 > TOP_H { top - 92.0 } else { bottom + 16.0 };
        let a = egui::Area::new(egui::Id::new("selection-actions"))
            .fixed_pos(egui::pos2(x.max(4.0), y))
            .order(egui::Order::Middle)
            .fade_in(false)
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::CHIP)
                    .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(4, 3))
                    .shadow(egui::Shadow {
                        offset: [0, 1],
                        blur: 6,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(30),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if ink {
                                let tidy = egui::RichText::new(format!("{}  Tidy", theme::icon::SPARKLES))
                                    .color(theme::ACCENT);
                                if ui
                                    .add(egui::Button::new(tidy).min_size(egui::vec2(0.0, 30.0)))
                                    .on_hover_text(
                                        "Straighten the handwriting (baseline and slant; the ink itself is kept) and turn rough shapes into clean ones",
                                    )
                                    .clicked()
                                {
                                    out.push(Action::Cmd("selection.tidy", json!({})));
                                }
                                if tidied
                                    && ui
                                        .add(
                                            egui::Button::new(format!("{}  As written", theme::icon::UNDO))
                                                .min_size(egui::vec2(0.0, 30.0)),
                                        )
                                        .on_hover_text("Show this handwriting as written again")
                                        .clicked()
                                {
                                    out.push(Action::Cmd("selection.tidy", json!({"revert": true})));
                                }
                                divider(ui);
                            }
                            if icon_button(ui, theme::icon::COPY, false, "Copy (Ctrl+C)").clicked() {
                                out.push(Action::Cmd("clipboard.copy", json!({})));
                            }
                            if icon_button(ui, theme::icon::CUT, false, "Cut (Ctrl+X)").clicked() {
                                out.push(Action::Cmd("clipboard.cut", json!({})));
                            }
                            if icon_button(ui, theme::icon::TRASH, false, "Delete (Delete)").clicked() {
                                out.push(Action::Cmd("selection.delete", json!({})));
                            }
                        });
                    });
            });
        areas.push(a.response.rect);
    }
    // Prettify suggestions (mode "suggest", R-HWR-006): a chip at the top right of each block.
    if let Some(note) = s.current_note {
        for (block, r) in s.prettify_suggestions(note) {
            #[allow(clippy::cast_possible_truncation)] // logical px
            let at = egui::pos2(
                ((r.max.x - view.origin_x) * view.scale) as f32 + 6.0,
                ((r.min.y - view.origin_y) * view.scale) as f32 - 30.0,
            );
            let a = egui::Area::new(egui::Id::new(("suggest", block)))
                .fixed_pos(at)
                .order(egui::Order::Middle)
                .fade_in(false)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(theme::CHIP)
                        .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE))
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(4, 2))
                        .shadow(egui::Shadow {
                            offset: [0, 1],
                            blur: 4,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(30),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let tidy = egui::RichText::new(format!("{}  Tidy", theme::icon::SPARKLES))
                                    .color(theme::ACCENT);
                                if ui
                                    .add(egui::Button::new(tidy).corner_radius(12))
                                    .on_hover_text("Straighten this handwriting (undo or revert any time)")
                                    .clicked()
                                {
                                    out.push(Action::Cmd("hwr.suggestion", json!({"block": block, "apply": true})));
                                }
                                let close = egui::RichText::new(theme::icon::CLOSE).color(theme::MUTED);
                                if ui
                                    .add(egui::Button::new(close).corner_radius(12))
                                    .on_hover_text("Dismiss")
                                    .clicked()
                                {
                                    out.push(Action::Cmd("hwr.suggestion", json!({ "block": block })));
                                }
                            });
                        });
                });
            areas.push(a.response.rect);
        }
    }
    if st.settings {
        let mut open = true;
        let w = egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(&ctx, |ui| settings(eng, ui, out));
        if let Some(w) = w {
            areas.push(w.response.rect);
        }
        if !open {
            st.settings = false;
        }
    }
    if let Some((item, mut name)) = st.rename.take() {
        let mut keep = true;
        let w = egui::Window::new("Rename")
            .collapsible(false)
            .resizable(false)
            .show(&ctx, |ui| {
                let mut o = egui::TextEdit::singleline(&mut name).desired_width(240.0).show(ui);
                let r = o.response;
                // Enter in the field confirms (it may already have dropped focus this frame).
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if !enter && !r.has_focus() {
                    // Just opened: focus with the whole name selected, so typing replaces it.
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(name.chars().count()),
                    );
                    o.state.cursor.set_char_range(Some(all));
                    o.state.store(ui.ctx(), r.id);
                    r.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("Rename").clicked() || enter {
                        let (cmd, id) = match item {
                            Item::Folder(id) => ("workspace.folder.rename", id),
                            Item::Note(id) => ("workspace.note.rename", id),
                        };
                        out.push(Action::Cmd(cmd, json!({"id": id, "name": name})));
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                });
            });
        if let Some(w) = w {
            areas.push(w.response.rect);
        }
        if keep {
            st.rename = Some((item, name));
        }
    }
}

/// One tree row: indentation guides, a chevron for folders, the item's icon in its colour and
/// the name; hover and the open note are highlighted.
/// One widget senses both clicks and drags: egui then tells a click (open, expand) from a drag
/// (move) by the distance moved; a separate drag widget on top would take every click.
fn tree_row(ui: &mut egui::Ui, id: egui::Id, row: &Row, selected: bool) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::hover());
    let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
    paint_row(ui.painter(), rect, row, selected, resp.hovered());
    resp
}

fn paint_row(p: &egui::Painter, rect: egui::Rect, row: &Row, selected: bool, hovered: bool) {
    if selected {
        p.rect_filled(rect, 6.0, theme::SELECTED);
    } else if hovered {
        p.rect_filled(rect, 6.0, theme::HOVER);
    }
    #[allow(clippy::cast_precision_loss)] // a few levels of indentation
    let depth = row.depth.min(16) as f32;
    for d in 0..row.depth.min(16) {
        #[allow(clippy::cast_precision_loss)]
        let x = rect.left() + 13.0 + INDENT * d as f32;
        p.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, theme::GUIDE),
        );
    }
    let mut x = rect.left() + 4.0 + INDENT * depth;
    let cy = rect.center().y;
    let icon_font = egui::FontId::proportional(theme::TREE_ICON);
    let tint = row.color.map(theme::rgba);
    let (chevron, icon) = match row.item {
        Item::Folder(_) if row.open => (Some(theme::icon::CHEVRON_DOWN), theme::icon::FOLDER_OPEN),
        Item::Folder(_) => (Some(theme::icon::CHEVRON_RIGHT), theme::icon::FOLDER),
        Item::Note(_) => (None, theme::icon::NOTE),
    };
    if let Some(c) = chevron {
        p.text(
            egui::pos2(x + 6.0, cy),
            egui::Align2::CENTER_CENTER,
            c,
            egui::FontId::proportional(12.0),
            theme::MUTED,
        );
    }
    x += 16.0;
    p.text(
        egui::pos2(x + 8.0, cy),
        egui::Align2::CENTER_CENTER,
        icon,
        icon_font,
        tint.unwrap_or(theme::MUTED),
    );
    x += 22.0;
    let name = p.layout_no_wrap(
        row.name.clone(),
        egui::FontId::proportional(13.5),
        if selected { theme::ACCENT } else { theme::TEXT },
    );
    let clip = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(rect.right() - 4.0, rect.bottom()));
    p.with_clip_rect(clip)
        .galley(egui::pos2(x, cy - name.size().y / 2.0), name, theme::TEXT);
}

fn tree(st: &mut State, eng: &Engine, ui: &mut egui::Ui, out: &mut Vec<Action>) {
    let s = &eng.session;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("NOTES").size(11.5).strong().color(theme::MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let small =
                |icon: &str| egui::Button::new(egui::RichText::new(icon).size(16.0)).min_size(egui::vec2(26.0, 26.0));
            if ui
                .add(small(theme::icon::NEW_FOLDER))
                .on_hover_text("New folder")
                .clicked()
            {
                out.push(Action::Cmd("workspace.folder.create", json!({"name": "New folder"})));
            }
            if ui.add(small(theme::icon::NEW_NOTE)).on_hover_text("New note").clicked() {
                out.push(Action::Cmd("workspace.note.create", json!({"name": "Untitled"})));
            }
        });
    });
    ui.add_space(4.0);
    let mut toggle = None;
    let current = s.current_note.map(|n| n.0);
    let State {
        rows,
        rename,
        row_rects,
        ..
    } = st;
    row_rects.clear();
    let rows = &*rows;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in rows.get(range).unwrap_or(&[]) {
                let id = egui::Id::new(("row", row.item));
                let selected = matches!(row.item, Item::Note(n) if Some(n) == current);
                let r = tree_row(ui, id, row, selected);
                row_rects.push((row.name.clone(), r.rect));
                if r.double_clicked() {
                    // Double-click renames (the first click already toggled or opened).
                    *rename = Some((row.item, row.name.clone()));
                } else if r.clicked() {
                    match row.item {
                        Item::Folder(f) => toggle = Some(f),
                        Item::Note(n) => out.push(Action::Show { note: n, region: None }),
                    }
                }
                // Drag a row onto a folder to move it (R-ORG-002): the row follows the pointer.
                if r.dragged() {
                    egui::DragAndDrop::set_payload(ui.ctx(), row.item);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    if let Some(p) = ui.ctx().pointer_interact_pos() {
                        let layer = egui::LayerId::new(egui::Order::Tooltip, id);
                        let ghost = egui::Rect::from_center_size(p, r.rect.size());
                        let painter = ui.ctx().layer_painter(layer);
                        painter.rect_filled(ghost, 6.0, theme::HOVER);
                        paint_row(&painter, ghost, row, selected, false);
                    }
                }
                if let Item::Folder(target) = row.item
                    && r.dnd_hover_payload::<Item>().is_some_and(|p| *p != row.item)
                {
                    ui.painter().rect_stroke(
                        r.rect,
                        6.0,
                        egui::Stroke::new(1.5, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                    if let Some(payload) = r.dnd_release_payload::<Item>() {
                        match *payload {
                            Item::Note(n) => {
                                out.push(Action::Cmd("workspace.note.move", json!({"id": n, "folder": target})))
                            }
                            Item::Folder(f) if f != target => {
                                out.push(Action::Cmd("workspace.folder.move", json!({"id": f, "parent": target})));
                            }
                            Item::Folder(_) => {}
                        }
                    }
                }
                r.context_menu(|ui| context_menu(row, rename, ui, out));
            }
        });
    if let Some(f) = toggle
        && !st.expanded.remove(&f)
    {
        st.expanded.insert(f);
    }
}

fn context_menu(row: &Row, rename: &mut Option<(Item, String)>, ui: &mut egui::Ui, out: &mut Vec<Action>) {
    let item = row.item;
    if ui.button("Rename…").clicked() {
        *rename = Some((item, row.name.clone()));
        ui.close();
    }
    ui.menu_button("Colour", |ui| {
        for c in theme::PALETTE.iter().map(|c| Some(*c)).chain([None]) {
            let label = if c.is_some() { "■" } else { "None" };
            let mut text = egui::RichText::new(label);
            if let Some(c) = c {
                text = text.color(theme::rgba(c));
            }
            if ui.button(text).clicked() {
                let (cmd, id) = match item {
                    Item::Folder(id) => ("workspace.folder.set_color", id),
                    Item::Note(id) => ("workspace.note.set_color", id),
                };
                out.push(Action::Cmd(cmd, json!({"id": id, "color": c})));
                ui.close();
            }
        }
    });
    if let Item::Folder(f) = item {
        if ui.button("New note here").clicked() {
            out.push(Action::Cmd(
                "workspace.note.create",
                json!({"name": "Untitled", "folder": f}),
            ));
            ui.close();
        }
        if ui.button("New folder here").clicked() {
            out.push(Action::Cmd(
                "workspace.folder.create",
                json!({"name": "New folder", "parent": f}),
            ));
            ui.close();
        }
    }
    if ui.button(format!("{}  Move to trash", theme::icon::TRASH)).clicked() {
        let (cmd, id) = match item {
            Item::Folder(id) => ("workspace.folder.delete", id),
            Item::Note(id) => ("workspace.note.delete", id),
        };
        out.push(Action::Cmd(cmd, json!({ "id": id })));
        ui.close();
    }
}

fn search_panel(st: &mut State, ui: &mut egui::Ui, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.strong("Search");
        if ui.small_button("Close").clicked() {
            st.search = false;
        }
    });
    let r = ui.add(
        egui::TextEdit::singleline(&mut st.search_text)
            .hint_text("Notes, text, PDFs, handwriting")
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(&mut st.focus_search) {
        r.request_focus();
    }
    if st.search_text != st.searched {
        st.searched = st.search_text.clone();
        if st.search_text.trim().is_empty() {
            st.hits.clear();
        } else {
            out.push(Action::Search(st.search_text.clone()));
        }
    }
    if st.indexing {
        ui.weak("Still indexing — more results may follow.");
    }
    ui.separator();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (name, hits) in &st.hits {
            ui.strong(name);
            for h in hits {
                let label = format!("{}  {}", h.source, h.snippet);
                if ui.selectable_label(false, label).clicked()
                    && let Some(n) = h.note
                {
                    {
                        out.push(Action::Show {
                            note: n,
                            region: h.region,
                        });
                    }
                }
            }
            ui.add_space(6.0);
        }
    });
}

fn settings(eng: &Engine, ui: &mut egui::Ui, out: &mut Vec<Action>) {
    use vibenote_model::settings::{BarrelAction, PrettifyMode};
    let st = &eng.session.settings;
    ui.label("Handwriting block: idle time before it is finished");
    let mut idle = f64::from(st.block_idle_ms) / 1000.0;
    if ui.add(egui::Slider::new(&mut idle, 0.5..=10.0).suffix(" s")).changed() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 500..=10000 ms
        let ms = (idle * 1000.0).round() as u32;
        out.push(Action::Cmd("settings.set", json!({ "block_idle_ms": ms })));
    }
    ui.separator();
    ui.label("Straighten handwriting (prettify)");
    ui.horizontal(|ui| {
        for (label, mode, value) in [
            ("Off", PrettifyMode::Off, "off"),
            ("Suggest", PrettifyMode::Suggest, "suggest"),
            ("Automatic", PrettifyMode::Automatic, "automatic"),
        ] {
            if ui.selectable_label(st.prettify_mode == mode, label).clicked() {
                out.push(Action::Cmd("settings.set", json!({ "prettify_mode": value })));
            }
        }
    });
    ui.separator();
    ui.label("Shapes from ink");
    let mut lift = st.snap_on_lift;
    if ui
        .checkbox(&mut lift, "Snap rough shapes when the pen lifts")
        .on_hover_text("Off: hold the pen still for half a second at the end of a stroke, or lasso it and press Tidy")
        .changed()
    {
        out.push(Action::Cmd("settings.set", json!({ "snap_on_lift": lift })));
    }
    ui.separator();
    for (k, label, cur) in [
        ("barrel1", "Pen button 1", st.barrel1),
        ("barrel2", "Pen button 2", st.barrel2),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            for (name, action, value) in [
                ("Lasso", BarrelAction::Lasso, "lasso"),
                ("Eraser", BarrelAction::Eraser, "eraser"),
                ("Pan", BarrelAction::Pan, "pan"),
                ("None", BarrelAction::None, "none"),
            ] {
                if ui.selectable_label(cur == action, name).clicked() {
                    out.push(Action::Cmd("settings.set", json!({ k: value })));
                }
            }
        });
    }
    ui.separator();
    ui.label("Paper of this note");
    ui.horizontal(|ui| {
        for (name, kind) in [
            ("Blank", "blank"),
            ("Lined", "lined"),
            ("Grid", "grid"),
            ("Dots", "dots"),
        ] {
            if ui.button(name).clicked() {
                out.push(Action::Cmd("canvas.set_background", json!({ "kind": kind })));
            }
        }
    });
}

impl Chrome {
    /// A folder or note was just created from the chrome: show it (expand its parent) and open
    /// the rename field so it gets a real name right away.
    pub fn created(&mut self, cmd: &str, params: &Value, result: &Value) {
        let Some(id) = result["id"].as_u64() else { return };
        let (item, parent, name) = match cmd {
            "workspace.folder.create" => (Item::Folder(id), params["parent"].as_u64(), "New folder"),
            "workspace.note.create" => (Item::Note(id), params["folder"].as_u64(), "Untitled"),
            _ => return,
        };
        if let Some(p) = parent {
            self.st.expanded.insert(p);
        }
        let name = params["name"].as_str().unwrap_or(name).to_string();
        self.st.rename = Some((item, name));
        self.dirty = true;
    }

    /// The visible tree rows (name, rect in logical px) from the last run.
    pub fn tree_rows(&self) -> Vec<(String, egui::Rect)> {
        self.st.row_rects.clone()
    }

    /// Search results held (tests).
    pub fn result_count(&self) -> usize {
        self.st.hits.iter().map(|g| g.1.len()).sum()
    }
}

/// Renders the current note with the chrome on top, headless (UI tests and screenshots for
/// proposed UI goldens). The chrome runs once at scale 1 first; its actions are returned.
pub fn screenshot(
    engine: &Engine,
    chrome: &mut Chrome,
    size: (u32, u32),
) -> Result<(vibenote_render::Image, Vec<Action>), String> {
    let s = &engine.session;
    let note = s.current_note.ok_or("no note is open")?;
    let canvas = s.ws.canvas(note).map_err(|e| e.to_string())?;
    let v = s.view;
    let vm = ViewMap {
        origin_x: v.origin_x,
        origin_y: v.origin_y,
        scale: v.zoom,
    };
    #[allow(clippy::cast_precision_loss)] // window sizes
    let actions = chrome.run(engine, (size.0 as f32, size.1 as f32), 1.0, vm);
    let mut h = vibenote_render::Headless::new().map_err(|e| e.to_string())?;
    h.set_background(s.background_of(note));
    let (texts, vectors) = s.extras_in_view((v.origin_x, v.origin_y), v.zoom, (f64::from(size.0), f64::from(size.1)));
    h.set_extras(texts, vectors);
    let scene = vibenote_render::Scene {
        canvas,
        changes: None,
        live: &[],
        viewport: vibenote_render::Viewport {
            origin_x: v.origin_x,
            origin_y: v.origin_y,
            zoom: v.zoom,
            width: size.0,
            height: size.1,
        },
    };
    let (img, _) = h
        .render_scene_with(&scene, &mut |d, q, enc, view, f| {
            chrome.paint(d, q, enc, view, f, [size.0, size.1])
        })
        .map_err(|e| e.to_string())?;
    Ok((img, actions))
}
