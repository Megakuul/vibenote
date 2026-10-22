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
    /// Ask the finder for results (they come back with [`Chrome::set_finder_results`]).
    FinderQuery { kind: &'static str, query: String },
    /// Open a finder result (`finder.open`).
    FinderOpen(Value),
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
    /// The backup fields being edited in the settings (filled from `backup.status`).
    backup: Option<BackupForm>,
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
    /// The finder overlay (R-VIM-005), while open.
    finder: Option<Finder>,
    /// A notice shown until the instant (errors of actions).
    notice: Option<(String, std::time::Instant)>,
    /// Colour swatches laid out last run (logical px; tests).
    swatches: Vec<(u32, egui::Rect)>,
}

/// The telescope-like finder: a prompt, ranked results and a preview; keyboard only.
struct Finder {
    /// "text" (Space f w) or "notes" (Space f f, Ctrl+P).
    kind: &'static str,
    query: String,
    /// The query the results were asked for.
    asked: String,
    results: Vec<Value>,
    sel: usize,
    /// The selection moved by key: scroll it into view.
    moved: bool,
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
    /// Shows the settings window (as its toolbar button does).
    pub fn open_settings(&mut self) {
        self.st.settings = true;
        self.dirty = true;
    }

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
    /// The colour swatches on the toolbar and where they are (logical px).
    pub fn swatch_rects(&self) -> &[(u32, egui::Rect)] {
        &self.st.swatches
    }

    /// The notice shown, if any.
    pub fn notice(&self) -> Option<&str> {
        self.st.notice.as_ref().map(|(m, _)| m.as_str())
    }

    /// Shows `msg` to the user for a few seconds (an action failed, R-AUDIO-004).
    pub fn notify(&mut self, msg: String) {
        let now = std::time::Instant::now();
        let until = now.checked_add(std::time::Duration::from_secs(6)).unwrap_or(now);
        self.st.notice = Some((msg, until));
        self.dirty = true;
    }

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

    /// egui asked to run again right away (a new area's sizing pass, an animation step).
    pub fn wants_frame(&self) -> bool {
        self.dirty
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
        // Popups (context menus, submenus, pickers) are chrome too: without them the pointer
        // leaving a panel for its menu went to the canvas and egui closed the menu.
        // Tooltips are left out (they must not block the canvas under them).
        ctx.memory(|m| {
            for l in m.areas().visible_layer_ids() {
                if !matches!(l.order, egui::Order::Background | egui::Order::Tooltip)
                    && let Some(r) = m.area_rect(l.id)
                {
                    areas.push(r);
                }
            }
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

/// Tool buttons: icon, tooltip, key (the engine keymap's binding, shown on the button),
/// command, and whether it is the selected tool. The eraser button switches between stroke and
/// area eraser when pressed again (`tool.select.eraser`).
fn tool_buttons(
    t: vibenote_model::settings::ToolKind,
) -> [(&'static str, &'static str, &'static str, &'static str, bool); 7] {
    use theme::icon;
    use vibenote_model::settings::ToolKind as T;
    [
        (
            icon::PEN,
            "Pen (W or 1) — hold still at the end of a stroke to snap it into a shape",
            "W",
            "tool.select.pen",
            t == T::Pen,
        ),
        (
            icon::HIGHLIGHTER,
            "Highlighter (A or 2)",
            "A",
            "tool.select.highlighter",
            t == T::Highlighter,
        ),
        (
            icon::COVER,
            "Cover (D or 3): hide what is underneath; tap a cover to reveal it, again to hide (flashcards)",
            "D",
            "tool.select.cover",
            t == T::Cover,
        ),
        (
            icon::ERASER,
            if t == T::EraserArea {
                "Area eraser (E or 4) — press again for the stroke eraser"
            } else {
                "Stroke eraser (E or 4) — press again for the area eraser"
            },
            "E",
            "tool.select.eraser",
            matches!(t, T::EraserStroke | T::EraserArea),
        ),
        (
            icon::LASSO,
            "Lasso (Q or 5): select, then drag to move",
            "Q",
            "tool.select.lasso",
            t == T::Lasso,
        ),
        (icon::SHAPES, "Shapes (S or 6)", "S", "tool.select.shape", t == T::Shape),
        (
            icon::TEXT,
            "Text (T or 7): click into a box to type there; drag selects, double-click a word, triple-click a line; drag the frame to move",
            "T",
            "tool.select.text",
            t == T::Text,
        ),
    ]
}

/// Pen and text colours (R-PEN-013, R-TEXT-006), 0xRRGGBBAA: ink, blue, red, green, orange,
/// purple, grey, teal.
const PEN_COLORS: [u32; 8] = [
    0x1e17_14ff,
    0x1f5f_bfff,
    0xc628_28ff,
    0x2e7d_32ff,
    0xe079_1aff,
    0x6a3f_b5ff,
    0x6b6f_78ff,
    0x0083_8fff,
];
/// Highlighter colours (drawn translucent): yellow, green, pink, blue, orange, lilac.
const HIGHLIGHTER_COLORS: [u32; 6] = [
    0xf5c8_00ff,
    0x7ed9_57ff,
    0xff7e_b6ff,
    0x6ec6_ffff,
    0xffa6_4dff,
    0xc39b_ffff,
];

fn color32(c: u32) -> egui::Color32 {
    let [r, g, b, _] = c.to_be_bytes();
    egui::Color32::from_rgb(r, g, b)
}

/// A colour swatch on the toolbar; `on` draws a ring around it.
fn swatch(ui: &mut egui::Ui, c: u32, on: bool, tip: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(20.0, 32.0), egui::Sense::click());
    let p = ui.painter();
    let centre = r.center();
    if on {
        p.circle_stroke(centre, 9.0, egui::Stroke::new(2.0, theme::ACCENT));
    } else if resp.hovered() {
        p.circle_stroke(centre, 9.0, egui::Stroke::new(1.0, theme::MUTED));
    }
    p.circle_filled(centre, 6.5, color32(c));
    resp.on_hover_text(tip)
}

/// A palette of swatches plus a custom colour picker; returns the colour chosen this frame.
fn palette(
    ui: &mut egui::Ui,
    colors: &[u32],
    current: Option<u32>,
    what: &str,
    rects: &mut Vec<(u32, egui::Rect)>,
) -> Option<u32> {
    let mut chosen = None;
    for &c in colors {
        let r = swatch(ui, c, current.is_some_and(|k| k | 0xff == c | 0xff), what);
        rects.push((c, r.rect));
        if r.clicked() {
            chosen = Some(c);
        }
    }
    let mut c = color32(current.unwrap_or(0x8080_80ff));
    let r =
        egui::widgets::color_picker::color_edit_button_srgba(ui, &mut c, egui::widgets::color_picker::Alpha::Opaque)
            .on_hover_text(format!("Custom {what}"));
    if r.changed() {
        let [rr, gg, bb, _] = c.to_array();
        chosen = Some(u32::from_be_bytes([rr, gg, bb, 0xff]));
    }
    chosen
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
    st.swatches.clear();
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
                for (icon, tip, key, cmd, on) in tool_buttons(s.settings.tool) {
                    let r = icon_button(ui, icon, on, tip);
                    // The shortcut in the button's corner, so the keys are discoverable.
                    ui.painter().text(
                        egui::pos2(r.rect.right() - 3.0, r.rect.bottom() - 1.0),
                        egui::Align2::RIGHT_BOTTOM,
                        key,
                        egui::FontId::proportional(8.5),
                        theme::MUTED,
                    );
                    if r.clicked() {
                        out.push(Action::Cmd(cmd, json!({})));
                    }
                }
                // What the selected tool can do: colours to ink with, shapes to draw, formats
                // while typing.
                // (While typing the text palette shows instead.)
                if s.settings.tool == vibenote_model::settings::ToolKind::Pen && s.text_edit.is_none() {
                    divider(ui);
                    if let Some(c) = palette(ui, &PEN_COLORS, Some(s.settings.pen_color.0), "pen colour", &mut st.swatches) {
                        out.push(Action::Cmd("settings.set", json!({ "pen_color": c })));
                    }
                }
                if s.settings.tool == vibenote_model::settings::ToolKind::Highlighter && s.text_edit.is_none() {
                    divider(ui);
                    let cur = Some(s.settings.highlighter_color.0);
                    if let Some(c) = palette(ui, &HIGHLIGHTER_COLORS, cur, "highlighter colour", &mut st.swatches) {
                        out.push(Action::Cmd("settings.set", json!({ "highlighter_color": c })));
                    }
                }
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
                if icon_button(
                    ui,
                    theme::icon::RULER,
                    s.ruler.visible,
                    "Ruler (R): lines drawn along its edge come out straight; drag it to move, its knob or the wheel to rotate, Alt+Left/Right turn by 15°",
                )
                .clicked()
                {
                    out.push(Action::Cmd("ruler.toggle", ruler_here(s, view, ctx.content_rect())));
                }
                // Vim mode (R-VIM-001): a text toggle, highlighted while on.
                let vim = s.settings.vim;
                let label = egui::RichText::new("VIM")
                    .monospace()
                    .size(12.0)
                    .strong()
                    .color(if vim { theme::ACCENT } else { theme::MUTED });
                if ui
                    .add(egui::Button::selectable(vim, label).min_size(egui::vec2(40.0, 32.0)))
                    .on_hover_text(
                        "Vim mode: text boxes edit with vim keys (jk = Esc) in a monospace font; Space f w finds text, Space f f finds notes",
                    )
                    .clicked()
                {
                    out.push(Action::Cmd("settings.set", json!({ "vim": !vim })));
                }
                if s.text_edit.is_some() {
                    divider(ui);
                    for (icon, tip, format) in TEXT_STYLES {
                        if icon_button(ui, icon, false, tip).clicked() {
                            out.push(Action::Cmd("text.style", json!({ "format": format })));
                        }
                    }
                    // Text colour (R-TEXT-006): a palette button with the swatches in a popup.
                    let icon = egui::RichText::new(theme::icon::PALETTE).size(theme::ICON).color(theme::TEXT);
                    let menu = ui
                        .menu_button(icon, |ui| {
                            ui.horizontal(|ui| {
                                palette(
                                    ui,
                                    PEN_COLORS.get(1..).unwrap_or(&[]),
                                    None,
                                    "text colour (the selection, or the whole box)",
                                    &mut st.swatches,
                                )
                            })
                            .inner
                        });
                    st.swatches.push((0, menu.response.rect));
                    if let Some(Some(c)) = menu.inner {
                        out.push(Action::Cmd("text.style", json!({ "format": "color", "value": c })));
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
                        (theme::icon::MIC, "Record a voice note (synced to ink)")
                    };
                    if icon_button(ui, icon, recording, tip).clicked() {
                        out.push(if recording {
                            Action::Cmd("audio.record.stop", json!({}))
                        } else {
                            Action::Cmd("audio.record.start", json!({"source": "device"}))
                        });
                    }
                    // The elapsed time while recording (R-AUDIO-004).
                    if let Some(secs) = recording_secs(s) {
                        ui.label(
                            egui::RichText::new(format!("● {}:{:02}", secs / 60, secs % 60))
                                .color(egui::Color32::from_rgb(0xc6, 0x28, 0x28))
                                .monospace(),
                        );
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
    vim_status(s, st, &ctx);
    ruler_label(s, view, &ctx);
    notice(st, &ctx, areas);
    if st.finder.is_some() {
        finder(st, &ctx, out, areas);
    }
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
            .show(&ctx, |ui| settings(eng, &mut st.backup, ui, out));
        if let Some(w) = w {
            areas.push(w.response.rect);
        }
        if !open {
            st.settings = false;
            st.backup = None;
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

fn settings(eng: &Engine, backup_form: &mut Option<BackupForm>, ui: &mut egui::Ui, out: &mut Vec<Action>) {
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
    let mut vim = st.vim;
    if ui
        .checkbox(&mut vim, "Vim mode for text boxes")
        .on_hover_text(
            "Normal/Insert/Visual modes, jk leaves Insert, monospace text; Space f w finds text, Space f f finds notes",
        )
        .changed()
    {
        out.push(Action::Cmd("settings.set", json!({ "vim": vim })));
    }
    ui.separator();
    ui.label("Shapes from ink");
    // R-SHAPE-002: resting the pen shows the shape, lifting there makes it, moving on keeps
    // drawing; how long the rest is, or never.
    let mut hold_on = st.snap_hold_ms > 0;
    let mut hold_s = if hold_on {
        f64::from(st.snap_hold_ms) / 1000.0
    } else {
        0.5
    };
    ui.horizontal(|ui| {
        let a = ui
            .checkbox(&mut hold_on, "Snap when the pen rests")
            .on_hover_text("Rest the pen at the end of a stroke: the clean shape appears. Lift there to keep it; move on to keep drawing.");
        let b = ui.add_enabled(hold_on, egui::Slider::new(&mut hold_s, 0.2..=3.0).suffix(" s"));
        if a.changed() || b.changed() {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 200..=3000 ms
            let ms = if hold_on { (hold_s * 1000.0).round() as u32 } else { 0 };
            out.push(Action::Cmd("settings.set", json!({ "snap_hold_ms": ms })));
        }
    });
    ui.horizontal(|ui| {
        use vibenote_model::settings::ShapeStyle;
        ui.label("Shapes look");
        for (label, style, value) in [
            ("Clean", ShapeStyle::Clean, "clean"),
            ("Hand-drawn", ShapeStyle::HandDrawn, "hand_drawn"),
        ] {
            if ui.selectable_label(st.shape_style == style, label).clicked() {
                out.push(Action::Cmd("settings.set", json!({ "shape_style": value })));
            }
        }
    });
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
    ui.separator();
    // R-PLAT-004: a launcher entry with the icon (packages install one system-wide).
    if ui
        .button("Add to app launcher")
        .on_hover_text("Installs a desktop entry and the vibenote icon for your user (~/.local/share)")
        .clicked()
    {
        out.push(Action::Cmd("app.install_desktop_entry", json!({})));
    }
    ui.separator();
    egui::CollapsingHeader::new("Cloud backup")
        .default_open(eng.backup_status().is_some_and(|b| at(&b, "/enabled") == &json!(true)))
        .show(ui, |ui| backup_settings(eng, backup_form, ui, out));
}

/// The backup settings being edited (secrets stay empty unless the owner types new ones).
#[derive(Clone, Debug, Default, PartialEq)]
struct BackupForm {
    provider: String,
    interval: u32,
    endpoint: String,
    bucket: String,
    region: String,
    prefix: String,
    key_id: String,
    secret: String,
    profile: String,
    client_id: String,
    client_secret: String,
    folder: String,
}

impl BackupForm {
    fn from_status(b: &Value) -> BackupForm {
        let t = |v: &Value| v.as_str().unwrap_or("").to_string();
        BackupForm {
            provider: match at(b, "/provider").as_str() {
                Some("drive") => "drive".into(),
                _ => "s3".into(),
            },
            interval: at(b, "/interval_min")
                .as_u64()
                .and_then(|m| u32::try_from(m).ok())
                .unwrap_or(10),
            endpoint: t(at(b, "/s3/endpoint")),
            bucket: t(at(b, "/s3/bucket")),
            region: t(at(b, "/s3/region")),
            prefix: t(at(b, "/s3/prefix")),
            key_id: t(at(b, "/s3/access_key_id")),
            secret: String::new(),
            profile: t(at(b, "/s3/profile")),
            client_id: t(at(b, "/drive/client_id")),
            client_secret: String::new(),
            folder: t(at(b, "/drive/folder")),
        }
    }

    /// `backup.configure` parameters for the form (empty secrets: unchanged).
    fn params(&self) -> Value {
        let mut s3 = serde_json::Map::new();
        for (k, v) in [
            ("endpoint", &self.endpoint),
            ("bucket", &self.bucket),
            ("region", &self.region),
            ("prefix", &self.prefix),
            ("access_key_id", &self.key_id),
            ("profile", &self.profile),
        ] {
            s3.insert(k.into(), json!(v));
        }
        if !self.secret.is_empty() {
            s3.insert("secret_access_key".into(), json!(self.secret));
        }
        let mut drive = serde_json::Map::new();
        drive.insert("client_id".into(), json!(self.client_id));
        drive.insert("folder".into(), json!(self.folder));
        if !self.client_secret.is_empty() {
            drive.insert("client_secret".into(), json!(self.client_secret));
        }
        json!({"provider": self.provider, "interval_min": self.interval, "s3": s3, "drive": drive})
    }
}

/// The value at JSON pointer `p` (null if missing).
fn at<'a>(v: &'a Value, p: &str) -> &'a Value {
    v.pointer(p).unwrap_or(&Value::Null)
}

/// "5 min ago" for a Unix-ms time.
fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let min = now.saturating_sub(ms) / 60_000;
    match min {
        0 => "just now".into(),
        1..=59 => format!("{min} min ago"),
        60..=2879 => format!("{} h ago", min / 60),
        _ => format!("{} days ago", min / 1440),
    }
}

/// Settings → Cloud backup (R-BACKUP-001, R-BACKUP-003).
fn backup_settings(eng: &Engine, form: &mut Option<BackupForm>, ui: &mut egui::Ui, out: &mut Vec<Action>) {
    let Some(b) = eng.backup_status() else {
        ui.label("Not available for this workspace.");
        return;
    };
    let f = form.get_or_insert_with(|| BackupForm::from_status(&b));
    let mut enabled = at(&b, "/enabled") == &json!(true);
    if ui.checkbox(&mut enabled, "Back up this workspace").changed() {
        let mut p = f.params();
        if let Some(m) = p.as_object_mut() {
            m.insert("enabled".into(), json!(enabled));
        }
        out.push(Action::Cmd("backup.configure", p));
    }
    ui.horizontal(|ui| {
        ui.label("Service");
        for (label, value) in [("S3", "s3"), ("Google Drive", "drive")] {
            if ui.selectable_label(f.provider == value, label).clicked() {
                f.provider = value.into();
            }
        }
    });
    let field = |ui: &mut egui::Ui, label: &str, v: &mut String, hint: &str, secret: bool| {
        ui.label(label);
        ui.add(
            egui::TextEdit::singleline(v)
                .hint_text(hint)
                .password(secret)
                .desired_width(260.0),
        );
        ui.end_row();
    };
    egui::Grid::new("backup-fields")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            if f.provider == "s3" {
                field(ui, "Bucket", &mut f.bucket, "", false);
                field(ui, "Region", &mut f.region, "e.g. eu-central-1", false);
                field(
                    ui,
                    "Endpoint",
                    &mut f.endpoint,
                    "empty: AWS (or https://… for others)",
                    false,
                );
                field(ui, "Prefix", &mut f.prefix, "vibenote", false);
                field(ui, "Access key ID", &mut f.key_id, "or use an AWS profile", false);
                let hint = if at(&b, "/s3/has_secret") == &json!(true) {
                    "saved (type to replace)"
                } else {
                    ""
                };
                field(ui, "Secret key", &mut f.secret, hint, true);
                field(ui, "AWS profile", &mut f.profile, "from ~/.aws/credentials", false);
            } else {
                field(
                    ui,
                    "OAuth client ID",
                    &mut f.client_id,
                    "Google Cloud → Credentials → Desktop app",
                    false,
                );
                let hint = if at(&b, "/drive/has_client_secret") == &json!(true) {
                    "saved (type to replace)"
                } else {
                    ""
                };
                field(ui, "Client secret", &mut f.client_secret, hint, true);
                field(ui, "Folder", &mut f.folder, "vibenote-backup", false);
            }
            ui.label("Every");
            ui.add(egui::DragValue::new(&mut f.interval).range(1..=1440).suffix(" min"));
            ui.end_row();
        });
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            out.push(Action::Cmd("backup.configure", f.params()));
            f.secret.clear();
            f.client_secret.clear();
        }
        if f.provider == "drive" {
            let connected = at(&b, "/drive/connected") == &json!(true);
            if ui
                .button(if connected {
                    "Reconnect Google Drive"
                } else {
                    "Connect Google Drive"
                })
                .on_hover_text("Opens the Google sign-in in your browser")
                .clicked()
            {
                out.push(Action::Cmd("backup.configure", f.params()));
                out.push(Action::Cmd("backup.drive_connect", json!({})));
                f.client_secret.clear();
            }
            if connected && ui.button("Disconnect").clicked() {
                out.push(Action::Cmd("backup.drive_disconnect", json!({})));
            }
        }
        if ui
            .add_enabled(at(&b, "/running") != &json!(true), egui::Button::new("Back up now"))
            .clicked()
        {
            out.push(Action::Cmd("backup.now", json!({})));
        }
    });
    let dim = ui.visuals().weak_text_color();
    if at(&b, "/running") == &json!(true) {
        ui.label("Backing up…");
    } else if at(&b, "/connecting") == &json!(true) {
        ui.label("Waiting for the Google sign-in in your browser…");
    } else if let Some(t) = at(&b, "/last_success_ms").as_u64() {
        ui.label(format!("Last backup: {}", ago(t)));
    } else {
        ui.colored_label(dim, "No backup yet.");
    }
    if let Some(e) = at(&b, "/last_error").as_str() {
        ui.colored_label(ui.visuals().error_fg_color, e);
    } else if let (false, Some(p)) = (enabled, at(&b, "/problem").as_str()) {
        ui.colored_label(dim, p);
    }
}

/// Shows the ruler at the middle of the visible canvas (a toggle that hides it keeps it put).
fn ruler_here(s: &vibenote_engine::Session, view: ViewMap, screen: egui::Rect) -> Value {
    if s.ruler.visible {
        return json!({ "visible": false });
    }
    // The canvas point at the middle of the window, a little above it.
    let (cx, cy) = (f64::from(screen.center().x), f64::from(screen.center().y) - 40.0);
    let at = [view.origin_x + cx / view.scale, view.origin_y + cy / view.scale];
    json!({ "visible": true, "at": at })
}

/// Seconds since the recording in progress started.
fn recording_secs(s: &vibenote_engine::Session) -> Option<u64> {
    let start = s.audio.recording_start_us()?;
    Some(vibenote_engine::audio::now_us().saturating_sub(start) / 1_000_000)
}

/// The ruler's angle next to its pivot (R-RULER-001).
fn ruler_label(s: &vibenote_engine::Session, view: ViewMap, ctx: &egui::Context) {
    let r = s.ruler;
    if !r.visible {
        return;
    }
    #[allow(clippy::cast_possible_truncation)] // screen position
    let pos = egui::pos2(
        ((r.x - view.origin_x) * view.scale) as f32,
        ((r.y - view.origin_y) * view.scale) as f32 + 30.0,
    );
    // A tooltip-order painter: not a chrome area, so the pen still reaches the ruler there.
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("ruler_label")));
    let a = r.angle_deg.rem_euclid(360.0);
    let shown = if a > 180.0 { a - 360.0 } else { a };
    painter.text(
        pos,
        egui::Align2::CENTER_CENTER,
        format!("{shown:.0}°"),
        egui::FontId::proportional(13.0),
        egui::Color32::from_rgb(0x34, 0x5a, 0x8f),
    );
}

/// A notice for the user at the bottom (errors of actions, R-AUDIO-004), for a few seconds.
fn notice(st: &mut State, ctx: &egui::Context, areas: &mut Vec<egui::Rect>) {
    let Some((msg, until)) = st.notice.clone() else { return };
    let now = std::time::Instant::now();
    if now >= until {
        st.notice = None;
        return;
    }
    ctx.request_repaint_after(until.saturating_duration_since(now));
    let r = egui::Area::new(egui::Id::new("notice"))
        .fade_in(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(0x3a, 0x2a, 0x2a))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(msg).color(egui::Color32::WHITE));
                });
        });
    areas.push(r.response.rect);
}

/// The vim mode of the box being edited, bottom left over the canvas ("-- NORMAL --" and the
/// keys of an unfinished command).
/// It is not chrome: the canvas under it still takes the pen (a tooltip-order layer, which the
/// hit areas leave out).
fn vim_status(s: &vibenote_engine::Session, st: &State, ctx: &egui::Context) {
    let Some(v) = s.text_edit.as_ref().and_then(|e| serde_json::to_value(e).ok()) else {
        return;
    };
    let vim = v.get("vim");
    let Some(mode) = vim.and_then(|x| x.get("mode")).and_then(Value::as_str) else {
        return;
    };
    let label = match mode {
        "insert" => "-- INSERT --",
        "visual" => "-- VISUAL --",
        "visual_line" => "-- VISUAL LINE --",
        _ => "-- NORMAL --",
    };
    let pending = vim.and_then(|x| x.get("pending")).and_then(Value::as_str).unwrap_or("");
    let screen = ctx.content_rect();
    let left = if st.sidebar { SIDE_W + 12.0 } else { 12.0 };
    egui::Area::new(egui::Id::new("vim-status"))
        .order(egui::Order::Tooltip)
        .fade_in(false)
        .interactable(false)
        .fixed_pos(egui::pos2(screen.left() + left, screen.bottom() - 36.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let color = if mode == "insert" { theme::ACCENT } else { theme::TEXT };
                        ui.label(egui::RichText::new(label).monospace().strong().color(color));
                        if !pending.is_empty() {
                            ui.label(egui::RichText::new(pending).monospace().color(theme::MUTED));
                        }
                    });
                });
        });
}

/// The finder overlay (R-VIM-005): prompt, results and preview in the centre. Up/Down,
/// Ctrl+N/P and Ctrl+J/K move, Enter opens, Esc closes; the prompt keeps the keyboard.
fn finder(st: &mut State, ctx: &egui::Context, out: &mut Vec<Action>, areas: &mut Vec<egui::Rect>) {
    let Some(f) = st.finder.as_mut() else { return };
    let (mut down, mut up, mut enter, mut esc) = (false, false, false, false);
    ctx.input_mut(|i| {
        use egui::{Key, Modifiers as M};
        down = i.consume_key(M::NONE, Key::ArrowDown) | i.consume_key(M::CTRL, Key::N) | i.consume_key(M::CTRL, Key::J);
        up = i.consume_key(M::NONE, Key::ArrowUp) | i.consume_key(M::CTRL, Key::P) | i.consume_key(M::CTRL, Key::K);
        enter = i.consume_key(M::NONE, Key::Enter);
        esc = i.consume_key(M::NONE, Key::Escape);
    });
    let n = f.results.len();
    if down && n > 0 {
        f.sel = f.sel.saturating_add(1).min(n.saturating_sub(1));
        f.moved = true;
    }
    if up {
        f.sel = f.sel.saturating_sub(1);
        f.moved = true;
    }
    if esc {
        st.finder = None;
        return;
    }
    if enter {
        if let Some(r) = f.results.get(f.sel) {
            out.push(Action::FinderOpen(r.clone()));
        }
        st.finder = None;
        return;
    }
    let screen = ctx.content_rect();
    let size = egui::vec2((screen.width() * 0.85).min(900.0), (screen.height() * 0.7).min(480.0));
    let rect = egui::Rect::from_center_size(screen.center(), size);
    let title = if f.kind == "text" { "Find text" } else { "Find note" };
    let mut open: Option<usize> = None;
    let a = egui::Area::new(egui::Id::new("finder"))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::PANEL_EDGE))
                .corner_radius(10.0)
                .shadow(egui::Shadow {
                    offset: [0, 4],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(60),
                })
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(size.x - 24.0);
                    ui.set_height(size.y - 24.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(title).strong());
                        ui.label(
                            egui::RichText::new("↑↓ / Ctrl+N P J K move · Enter open · Esc close")
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    });
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut f.query)
                            .hint_text(if f.kind == "text" {
                                "Text or handwriting…"
                            } else {
                                "Note name…"
                            })
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    if f.query != f.asked {
                        f.asked = f.query.clone();
                        f.sel = 0;
                        out.push(Action::FinderQuery {
                            kind: f.kind,
                            query: f.query.clone(),
                        });
                    }
                    ui.add_space(6.0);
                    let list_w = (size.x - 24.0) * 0.5;
                    ui.horizontal_top(|ui| {
                        let col = egui::Layout::top_down(egui::Align::Min);
                        ui.allocate_ui_with_layout(egui::vec2(list_w, size.y - 90.0), col, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("finder-results")
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    if f.results.is_empty() {
                                        ui.label(egui::RichText::new("No matches").color(theme::MUTED));
                                    }
                                    for (i, r) in f.results.iter().enumerate() {
                                        let main = r["line"].as_str().or_else(|| r["name"].as_str()).unwrap_or("");
                                        let mut where_ = r["path"].as_str().unwrap_or("").to_string();
                                        if f.kind == "text" {
                                            let name = r["name"].as_str().unwrap_or("");
                                            where_ = if where_.is_empty() {
                                                name.to_string()
                                            } else {
                                                format!("{where_}/{name}")
                                            };
                                            if let Some(p) = r["page"].as_u64() {
                                                where_ = format!("{where_} · PDF p. {p}");
                                            }
                                        }
                                        let mut job = egui::text::LayoutJob::default();
                                        job.append(
                                            main,
                                            0.0,
                                            egui::TextFormat {
                                                font_id: egui::FontId::monospace(13.0),
                                                color: if i == f.sel { theme::ACCENT } else { theme::TEXT },
                                                ..Default::default()
                                            },
                                        );
                                        if !where_.is_empty() {
                                            job.append(
                                                &format!("   {where_}"),
                                                0.0,
                                                egui::TextFormat {
                                                    font_id: egui::FontId::proportional(11.5),
                                                    color: theme::MUTED,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        job.wrap.max_rows = 1;
                                        job.wrap.max_width = list_w - 12.0;
                                        // A left-aligned row (a button would centre its text).
                                        let (rect, resp) = ui
                                            .allocate_exact_size(egui::vec2(list_w - 12.0, 24.0), egui::Sense::click());
                                        if i == f.sel {
                                            ui.painter().rect_filled(rect, 5.0, theme::SELECTED);
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(rect, 5.0, theme::HOVER);
                                        }
                                        let galley = ui.fonts_mut(|fo| fo.layout_job(job));
                                        let y = rect.center().y - galley.size().y / 2.0;
                                        ui.painter().with_clip_rect(rect).galley(
                                            egui::pos2(rect.left() + 8.0, y),
                                            galley,
                                            theme::TEXT,
                                        );
                                        if i == f.sel && f.moved {
                                            resp.scroll_to_me(None);
                                        }
                                        if resp.clicked() {
                                            open = Some(i);
                                        }
                                    }
                                });
                        });
                        ui.separator();
                        // Preview of the selected result.
                        let rest = egui::vec2(ui.available_width(), size.y - 90.0);
                        ui.allocate_ui_with_layout(rest, col, |ui| {
                            let Some(r) = f.results.get(f.sel) else { return };
                            let name = r["name"].as_str().unwrap_or("");
                            let path = r["path"].as_str().unwrap_or("");
                            let head = if path.is_empty() {
                                name.to_string()
                            } else {
                                format!("{path}/{name}")
                            };
                            ui.label(egui::RichText::new(head).strong());
                            ui.add_space(4.0);
                            let line = r["line"].as_str();
                            for l in r["preview"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                                let hit = line == Some(l);
                                ui.label(
                                    egui::RichText::new(if l.is_empty() { " " } else { l })
                                        .monospace()
                                        .size(12.5)
                                        .color(if hit { theme::ACCENT } else { theme::TEXT }),
                                );
                            }
                        });
                    });
                });
        });
    f.moved = false;
    areas.push(a.response.rect);
    if let Some(i) = open {
        if let Some(r) = f.results.get(i) {
            out.push(Action::FinderOpen(r.clone()));
        }
        st.finder = None;
    }
}

impl Chrome {
    /// Opens the finder (`kind` "text" or "notes") with an empty prompt (R-VIM-005).
    pub fn open_finder(&mut self, kind: &str) {
        self.st.finder = Some(Finder {
            kind: if kind == "text" { "text" } else { "notes" },
            query: String::new(),
            asked: String::new(),
            results: Vec::new(),
            sel: 0,
            moved: false,
        });
        self.dirty = true;
    }

    /// Results of `finder.query` for `query` (older answers are ignored).
    pub fn set_finder_results(&mut self, query: &str, r: &Value) {
        if let Some(f) = self.st.finder.as_mut()
            && f.asked == query
        {
            f.results = r["results"].as_array().cloned().unwrap_or_default();
            f.sel = f.sel.min(f.results.len().saturating_sub(1));
            self.dirty = true;
        }
    }

    /// True while the finder is open (tests).
    pub fn finder_open(&self) -> bool {
        self.st.finder.is_some()
    }

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
