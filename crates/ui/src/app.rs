//! The Wayland app (ADR 0001): window, seats, own `zwp_tablet_v2` binding, fractional scale.
//!
//! Frame loop (ADR 0001 binding rules): pen frames go into the engine (`input.pen`) on the
//! event; after each batch of queued Wayland events the frame is drawn immediately into the wgpu
//! surface (ADR 0010) with a non-blocking present (Mailbox, else Immediate) — never waiting for
//! a frame callback. If no GPU surface can be created, a slow fallback renders offscreen and
//! copies into a `wl_shm` buffer, paced by frame callbacks.

use crate::chrome::{Action, Chrome, ViewMap};
use crate::clipboard::{self as clip, Kind, Pasted};
use crate::cursor::Cursor;
use crate::gpu_surface::GpuSurface;
use crate::latency::LatencyLog;
use crate::tablet::{ToolEvent, ToolTranslator};
use sctk::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::dispatch2::Dispatch2;
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::calloop::EventLoop;
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::{GlobalList, registry_queue_init};
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum};
use sctk::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1 as fsm, wp_fractional_scale_v1 as fs,
};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_pinch_v1 as pinch, zwp_pointer_gestures_v1 as gestures,
};
use sctk::reexports::protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback as pfb};
use sctk::reexports::protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2 as tm, zwp_tablet_pad_group_v2 as pgroup, zwp_tablet_pad_ring_v2 as pring,
    zwp_tablet_pad_strip_v2 as pstrip, zwp_tablet_pad_v2 as pad, zwp_tablet_seat_v2 as tseat,
    zwp_tablet_tool_v2 as tool, zwp_tablet_v2 as tab,
};
use sctk::reexports::protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers};
use sctk::seat::pointer::{PointerEvent, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::WaylandSurface;
use sctk::shell::xdg::XdgShell;
use sctk::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use sctk::shm::slot::{Buffer, SlotPool};
use sctk::shm::{Shm, ShmHandler};
use sctk::{delegate_registry, registry_handlers};
use smithay_client_toolkit as sctk;
use std::path::PathBuf;
use std::sync::Arc;
use vibenote_engine::Engine;
use vibenote_ink::input::{PenEvent, PenEventKind, PenTool};
use vibenote_render::{GpuFault, Headless, PAPER, Renderer, Viewport, request_device, wgpu};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1 as csd, wp_cursor_shape_manager_v1 as csm,
};

pub const APP_ID: &str = "vibenote";

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("wayland: {0}")]
    Wayland(String),
    #[error("engine: {0}")]
    Engine(#[from] vibenote_engine::EngineError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

fn wl(e: impl std::fmt::Display) -> UiError {
    UiError::Wayland(e.to_string())
}

/// Start-up options (`vibenote --workspace <path>`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Workspace database; default `$XDG_DATA_HOME/vibenote/workspace.sqlite`.
    pub workspace: Option<PathBuf>,
    /// Note to open; default the first note (one is created in an empty workspace).
    pub note: Option<u64>,
}

/// CLOCK_MONOTONIC in nanoseconds (pen timestamps, presentation times, later audio sync).
pub fn mono_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let s = u64::try_from(t.tv_sec).unwrap_or(0);
    let ns = u64::try_from(t.tv_nsec).unwrap_or(0);
    s.saturating_mul(1_000_000_000).saturating_add(ns)
}

/// CLOCK_MONOTONIC in microseconds.
pub fn mono_us() -> u64 {
    mono_ns() / 1000
}

fn default_workspace() -> Result<PathBuf, UiError> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| UiError::Io(std::io::Error::other("neither XDG_DATA_HOME nor HOME is set")))?;
    let dir = base.join("vibenote");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("workspace.sqlite"))
}

/// Opens the workspace and picks the note to show (creating one in an empty workspace). The
/// note itself opens on the first window configure, when the visible size is known.
pub fn open_engine(opts: &Options) -> Result<(Engine, u64), UiError> {
    let path = match &opts.workspace {
        Some(p) => p.clone(),
        None => default_workspace()?,
    };
    let mut engine = Engine::open(&path)?;
    // Cloud backup (ADR 0012): only this workspace's entry in the config decides; a broken
    // config never keeps the workspace from opening.
    if let Some((config, state)) = vibenote_engine::backup::default_paths()
        && let Err(e) = engine.use_backup_config(&config, &state)
    {
        eprintln!("vibenote: backup: {e}");
    }
    let first = opts.note.or_else(|| engine.session.ws.notes.keys().next().map(|n| n.0));
    let id = match first {
        Some(id) => id,
        None => {
            let r = engine.execute("workspace.note.create", serde_json::json!({"name": "Note 1"}))?;
            r.get("id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| UiError::Wayland("note.create returned no id".into()))?
        }
    };
    Ok((engine, id))
}

/// Runs the app until the window is closed.
pub fn run(opts: &Options) -> Result<(), UiError> {
    let (engine, note) = open_engine(opts)?;
    let conn = Connection::connect_to_env().map_err(wl)?;
    let (globals, queue) = registry_queue_init::<App>(&conn).map_err(wl)?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(wl)?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(wl)?;
    // Pasted and dropped data, read on worker threads (clipboard.rs).
    let (paste_tx, paste_rx) = clip::sctk_channel::channel::<Pasted>();
    event_loop
        .handle()
        .insert_source(paste_rx, |ev, _, app: &mut App| {
            if let clip::sctk_channel::Event::Msg(p) = ev {
                app.insert_pasted(p);
            }
        })
        .map_err(|e| wl(e.to_string()))?;
    let data_devices = DataDeviceManagerState::bind(&globals, &qh).ok();

    let compositor = CompositorState::bind(&globals, &qh).map_err(wl)?;
    let xdg = XdgShell::bind(&globals, &qh).map_err(wl)?;
    let shm = Shm::bind(&globals, &qh).map_err(wl)?;
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("vibenote");
    window.set_app_id(APP_ID);
    window.set_min_size(Some((320, 240)));

    let viewport = bind_opt::<wp_viewporter::WpViewporter>(&globals, &qh, 1)
        .map(|vp| vp.get_viewport(window.wl_surface(), &qh, Plain));
    let fractional = bind_opt::<fsm::WpFractionalScaleManagerV1>(&globals, &qh, 1)
        .map(|m| m.get_fractional_scale(window.wl_surface(), &qh, Plain));
    let tablet_manager = bind_opt::<tm::ZwpTabletManagerV2>(&globals, &qh, 1);
    // Touchpad pinch zoom (R-CANVAS-002); optional.
    let gestures = bind_opt::<gestures::ZwpPointerGesturesV1>(&globals, &qh, 1);
    // System cursors by name (per tool and handle); without it the compositor's default stays.
    let cursor_shapes = bind_opt::<csm::WpCursorShapeManagerV1>(&globals, &qh, 1);
    let latency = LatencyLog::from_env();
    // wp_presentation is the latency instrument (ADR 0001 rule 4); feedback only when logging.
    let presentation = latency
        .as_ref()
        .and_then(|_| bind_opt::<wp_presentation::WpPresentation>(&globals, &qh, 1));
    if tablet_manager.is_none() {
        eprintln!("vibenote: compositor has no zwp_tablet_manager_v2; pen input unavailable");
    }
    window.commit();

    let pool = SlotPool::new(640 * 480 * 4, &shm).map_err(wl)?;
    let gpu = match Gpu::new(&conn, &window) {
        Ok(g) => {
            eprintln!("vibenote: {} {:?} {:?}", g.renderer.adapter, g.format, g.present_mode);
            Some(g)
        }
        Err(e) => {
            eprintln!("vibenote: no GPU surface ({e}); using the slow shm fallback");
            None
        }
    };
    let renderer = if gpu.is_some() {
        None
    } else {
        match Headless::new() {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("vibenote: renderer unavailable ({e}); showing blank paper");
                None
            }
        }
    };
    let mut app = App {
        conn: conn.clone(),
        gpu_recoveries: 0,
        retry: false,
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        _compositor: compositor,
        _xdg: xdg,
        shm,
        pool,
        buffer: None,
        window,
        viewport,
        _fractional: fractional,
        tablet_manager,
        tablet_seats: Vec::new(),
        tools: Vec::new(),
        width: 1200,
        height: 800,
        scale120: 120,
        configured: false,
        frame_pending: false,
        dirty: true,
        exit: false,
        open_note: Some(note),
        engine,
        gpu,
        renderer,
        latency,
        presentation,
        clock_id: None,
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        mouse_held: None,
        pdf_pages_seen: 0,
        mouse_as: 0,
        space_held: false,
        gestures,
        data_devices,
        data_device: None,
        copy_source: None,
        input_serial: 0,
        dnd: Vec::new(),
        dnd_next: 0,
        paste_tx,
        qh: qh.clone(),
        chrome_seen: 0,
        cursor_shapes,
        pointer_cursor: CursorSlot::default(),
        pen_pos: None,
        pinch: None,
        pinch_scale: 1.0,
        pointer_pos: (0.0, 0.0),
        chrome: Chrome::new(),
        mouse_on_chrome: false,
        pen_on_chrome: false,
        pointer_in_chrome: false,
    };
    if let Some(l) = app.latency.as_mut() {
        l.meta(serde_json::json!({
            "gpu": app.gpu.as_ref().map(|g| format!("{} {:?} {:?}", g.renderer.adapter, g.format, g.present_mode)),
            "presentation": app.presentation.is_some(),
            // Set by launchers (startup bench): CLOCK_MONOTONIC just before exec.
            "launch_ns": std::env::var("VIBENOTE_LAUNCH_NS").ok().and_then(|v| v.parse::<u64>().ok()),
        }));
    }
    let seats: Vec<_> = app.seat_state.seats().collect();
    for seat in seats {
        app.add_tablet_seat(&seat, &qh);
        app.add_data_device(&seat, &qh);
    }

    while !app.exit {
        // Dispatch returns after all queued events are handled (input coalescing, ADR 0001
        // rule 3); then draw at once (rule 1). A frame that could not be drawn (surface
        // outdated, acquire timeout) is retried shortly even without new events, and so is
        // merging a note that is still loading in the background (R-PERF-004).
        let pdf_busy = app.engine.session.pdf.busy()
            || app.engine.session.images.busy()
            || app.engine.session.hwr.busy()
            || app.engine.session.open_block.is_some();
        let timeout = (app.retry || app.engine.loading() || pdf_busy).then_some(std::time::Duration::from_millis(2));
        // The chrome's timed repaints (cursor blink), and a live recording's chunks, which are
        // journalled for crash recovery as they arrive.
        let recording = app
            .engine
            .session
            .audio
            .is_recording()
            .then_some(std::time::Duration::from_millis(250));
        // A playing player shows its progress (R-AUDIO-004).
        let playing = app.engine.session.audio.is_playing();
        if playing {
            app.dirty = true;
        }
        let playing = playing.then_some(std::time::Duration::from_millis(100));
        // A stroke being drawn may be held still to snap into a shape (R-SHAPE-002).
        let holding = app
            .engine
            .session
            .live
            .stroke
            .is_some()
            .then_some(std::time::Duration::from_millis(20));
        // Cloud backup: finished work is picked up and due backups start (R-BACKUP-002).
        let backup = app
            .engine
            .backup_active()
            .then(|| std::time::Duration::from_millis(if app.engine.backup_busy() { 500 } else { 30_000 }));
        let timeout = [timeout, app.chrome.wake_in(), recording, holding, playing, backup]
            .into_iter()
            .flatten()
            .min();
        event_loop.dispatch(timeout, &mut app).map_err(wl)?;
        if app.chrome.due() {
            app.dirty = true;
        }
        // One page per iteration, none while the pen is near or on the surface: a merged page
        // triggers a full redraw (until M3's incremental chunks), which must not delay ink.
        let pen_near = app.engine.session.live.tool.is_some() || app.engine.session.live.stroke.is_some();
        match app.engine.tick_pen(mono_us()) {
            Ok(true) => app.dirty = true,
            Ok(false) => {}
            Err(e) => eprintln!("vibenote: shape snap: {e}"),
        }
        // Handwriting: a block closes once the pen rested longer than the idle threshold.
        if !pen_near {
            app.engine.tick(mono_ns() / 1000);
        }
        if !pen_near && app.engine.pump(1) > 0 {
            app.dirty = true;
        }
        // PDF pages rendered in the background since the last frame.
        let pages = app
            .engine
            .session
            .pdf
            .finished()
            .wrapping_add(app.engine.session.images.finished());
        if pages != app.pdf_pages_seen {
            app.pdf_pages_seen = pages;
            app.dirty = true;
        }
        for msg in app.engine.backup_tick(std::time::SystemTime::now()) {
            app.chrome.notify(msg);
            app.dirty = true;
        }
        app.place_caret();
        app.redraw(&qh);
        app.update_cursors();
    }
    if let Some(l) = app.latency.as_mut() {
        l.meta(serde_json::json!({"clock_id": app.clock_id}));
        l.flush();
    }
    app.finish_live_stroke();
    app.engine.sync()?;
    Ok(())
}

fn bind_opt<I>(globals: &GlobalList, qh: &QueueHandle<App>, max: u32) -> Option<I>
where
    I: Proxy + 'static,
    Plain: Dispatch2<I, App>,
{
    globals.bind::<I, App, Plain>(qh, 1..=max, Plain).ok()
}

pub struct App {
    conn: Connection,
    /// GPU device/surface recreations so far (bounded; then the shm fallback).
    gpu_recoveries: u32,
    /// The last frame could not be drawn; retry soon even without new events.
    retry: bool,
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    _compositor: CompositorState,
    _xdg: XdgShell,
    shm: Shm,
    pool: SlotPool,
    buffer: Option<Buffer>,
    window: Window,
    viewport: Option<wp_viewport::WpViewport>,
    _fractional: Option<fs::WpFractionalScaleV1>,
    tablet_manager: Option<tm::ZwpTabletManagerV2>,
    tablet_seats: Vec<(wl_seat::WlSeat, tseat::ZwpTabletSeatV2)>,
    tools: Vec<ToolSlot>,
    /// Logical size.
    width: u32,
    height: u32,
    /// Preferred scale × 120 (wp_fractional_scale_v1 units).
    scale120: u32,
    configured: bool,
    frame_pending: bool,
    dirty: bool,
    exit: bool,
    /// Note to open on the first configure (its first viewport = the window, R-PERF-004).
    open_note: Option<u64>,
    engine: Engine,
    gpu: Option<Gpu>,
    /// Fallback only (no GPU surface).
    renderer: Option<Headless>,
    latency: Option<LatencyLog>,
    presentation: Option<wp_presentation::WpPresentation>,
    /// wp_presentation clock (1 = CLOCK_MONOTONIC).
    clock_id: Option<u32>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    /// Mouse button held over the canvas (left draws, middle and right pan; R-PEN-012).
    mouse_held: Option<u32>,
    /// Background PDF pages finished when last drawn.
    pdf_pages_seen: u64,
    /// Button the held mouse button is sent as (left → middle while Space is held).
    mouse_as: u32,
    /// Space is held (Space+Drag pans).
    space_held: bool,
    gestures: Option<gestures::ZwpPointerGesturesV1>,
    /// Clipboard and drag-and-drop (clipboard.rs).
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// Our claim on the clipboard after an in-app copy.
    copy_source: Option<CopyPasteSource>,
    /// Serial of the last key press, button press or pen down (claiming the clipboard needs one).
    input_serial: u32,
    /// Dropped offers being read, finished when their data arrived.
    dnd: Vec<(u64, DragOffer)>,
    dnd_next: u64,
    paste_tx: clip::sctk_channel::Sender<Pasted>,
    qh: QueueHandle<App>,
    /// [`App::chrome_state`] when egui last ran.
    chrome_seen: u64,
    cursor_shapes: Option<csm::WpCursorShapeManagerV1>,
    /// The mouse's cursor (set on each enter, and when what the pointer would do changes).
    pointer_cursor: CursorSlot,
    /// Last position of the pen in proximity (logical px): its cursor over the chrome.
    pen_pos: Option<(f64, f64)>,
    pinch: Option<pinch::ZwpPointerGesturePinchV1>,
    /// Cumulative scale of the pinch in progress at the last update.
    pinch_scale: f64,
    /// Last pointer position on our surface (logical px): the pinch anchor.
    pointer_pos: (f64, f64),
    /// Toolbar, folder tree, settings, search (ADR 0002).
    chrome: Chrome,
    /// The mouse / the pen pressed on the chrome: their events go to it until released.
    mouse_on_chrome: bool,
    pen_on_chrome: bool,
    /// The pointer hovers the chrome (it gets `PointerGone` when the pointer leaves it).
    pointer_in_chrome: bool,
}

/// A cursor to keep up to date: the shape device, the serial of the last enter (cursors can
/// only be set with it) and what was set since.
#[derive(Default)]
struct CursorSlot {
    device: Option<csd::WpCursorShapeDeviceV1>,
    serial: Option<u32>,
    shown: Option<Cursor>,
}

impl CursorSlot {
    /// Shows `want` unless it is already shown; `hide` removes the cursor image.
    fn show(&mut self, want: Cursor, hide: impl FnOnce(u32)) {
        let Some(serial) = self.serial else { return };
        if self.shown == Some(want) {
            return;
        }
        match want {
            Cursor::Hidden => hide(serial),
            Cursor::Shape(shape) => match &self.device {
                Some(d) => d.set_shape(serial, shape),
                None => return, // no cursor-shape protocol: the compositor's default stays
            },
        }
        self.shown = Some(want);
    }

    fn enter(&mut self, serial: u32) {
        self.serial = Some(serial);
        self.shown = None;
    }
}

/// A pen (tablet tool): its event translator and its cursor.
struct ToolSlot {
    tool: tool::ZwpTabletToolV2,
    tr: ToolTranslator,
    cursor: CursorSlot,
}

/// Caret, selection and box outline of the text box being edited.
const TEXT_CARET_COLOR: u32 = 0x2a6f_dbff;
const TEXT_SELECTION_COLOR: u32 = 0x3b7b_e04a;
/// Vim's Normal-mode block cursor (translucent, the character stays readable).
const TEXT_BLOCK_COLOR: u32 = 0x2a6f_db70;
const TEXT_BOX_COLOR: u32 = 0x9aa3_b090;

/// A vim selection as whole rows (R-VIM-001): one rectangle per laid-out row; rows the selection
/// continues past reach the right edge `x1`, Visual Line rows span `x0..x1`, and every row is at
/// least one `cell` wide (an empty line too).
fn full_line_rows(ranges: &[[f64; 4]], x0: f64, x1: f64, linewise: bool, cell: f64) -> Vec<[f64; 4]> {
    let mut rows: Vec<[f64; 4]> = Vec::new();
    for r in ranges {
        match rows.iter_mut().find(|q| (q[1] - r[1]).abs() < 0.5) {
            Some(q) => {
                q[0] = q[0].min(r[0]);
                q[2] = q[2].max(r[2]);
                q[3] = q[3].max(r[3]);
            }
            None => rows.push(*r),
        }
    }
    rows.sort_by(|a, b| a[1].total_cmp(&b[1]));
    let n = rows.len();
    for (i, q) in rows.iter_mut().enumerate() {
        if linewise {
            q[0] = x0;
            q[2] = x1;
        } else if i.saturating_add(1) < n {
            q[2] = x1;
        }
        q[2] = q[2].max(q[0] + cell);
    }
    rows
}

/// Text layout queries of either renderer (GPU window or the shm fallback).
trait TextLayout {
    fn caret(&mut self, item: &vibenote_render::TextItem, offset: u32) -> (f64, f64, f64);
    fn hit(&mut self, item: &vibenote_render::TextItem, p: (f64, f64)) -> u32;
    fn ranges(&mut self, item: &vibenote_render::TextItem, a: u32, b: u32) -> Vec<[f64; 4]>;
}

impl TextLayout for Renderer {
    fn caret(&mut self, item: &vibenote_render::TextItem, offset: u32) -> (f64, f64, f64) {
        self.text_caret(item, offset)
    }
    fn hit(&mut self, item: &vibenote_render::TextItem, p: (f64, f64)) -> u32 {
        self.text_hit(item, p)
    }
    fn ranges(&mut self, item: &vibenote_render::TextItem, a: u32, b: u32) -> Vec<[f64; 4]> {
        self.text_ranges(item, a, b)
    }
}

impl TextLayout for Headless {
    fn caret(&mut self, item: &vibenote_render::TextItem, offset: u32) -> (f64, f64, f64) {
        self.text_caret(item, offset)
    }
    fn hit(&mut self, item: &vibenote_render::TextItem, p: (f64, f64)) -> u32 {
        self.text_hit(item, p)
    }
    fn ranges(&mut self, item: &vibenote_render::TextItem, a: u32, b: u32) -> Vec<[f64; 4]> {
        self.text_ranges(item, a, b)
    }
}

/// Where the caret and selection of the edited text box are (canvas units).
struct TextGeom {
    /// (x, top, height) of the caret, and of the end of the text.
    caret: (f64, f64, f64),
    last: (f64, f64, f64),
    ranges: Vec<[f64; 4]>,
    /// Vim's block cursor: the cursor character's box.
    block: Vec<[f64; 4]>,
}

/// Scales a logical length by `scale120 / 120`, rounding (wp_fractional_scale_v1).
pub fn scaled(logical: u32, scale120: u32) -> u32 {
    let v = u64::from(logical)
        .saturating_mul(u64::from(scale120))
        .saturating_add(60)
        / 120;
    u32::try_from(v).unwrap_or(u32::MAX).max(1)
}

impl App {
    fn add_tablet_seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<App>) {
        let Some(mgr) = &self.tablet_manager else { return };
        if self.tablet_seats.iter().any(|(s, _)| s == seat) {
            return;
        }
        let ts = mgr.get_tablet_seat(seat, qh, Plain);
        self.tablet_seats.push((seat.clone(), ts));
    }

    fn add_data_device(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<App>) {
        if self.data_device.is_none()
            && let Some(m) = &self.data_devices
        {
            self.data_device = Some(m.get_data_device(qh, seat));
        }
    }

    /// After an in-app copy: claim the clipboard, so a paste here takes our objects while
    /// another app's later copy takes precedence.
    fn claim_clipboard(&mut self) {
        let (Some(m), Some(d)) = (&self.data_devices, &self.data_device) else {
            return;
        };
        let src = m.create_copy_paste_source(&self.qh, [clip::MARKER]);
        src.set_selection(d, self.input_serial);
        self.copy_source = Some(src);
    }

    /// Ctrl+V: what the clipboard holds goes onto the canvas (or into the text box being
    /// typed into); without a clipboard, the in-app one.
    fn paste(&mut self) {
        let offer = self.data_device.as_ref().and_then(|d| d.data().selection_offer());
        let Some(offer) = offer else {
            return self.paste_own();
        };
        let typing = self.engine.session.text_edit.is_some();
        let kind = offer.with_mime_types(|m| clip::choose(m, typing));
        match kind {
            None => {}
            Some(Kind::Own) => self.paste_own(),
            Some(kind) => match offer.receive(kind.mime().to_string()) {
                Ok(pipe) => {
                    use std::os::fd::AsFd;
                    match pipe.as_fd().try_clone_to_owned() {
                        Ok(fd) => clip::read_async(fd, kind, None, None, self.paste_tx.clone()),
                        Err(e) => eprintln!("vibenote: paste: {e}"),
                    }
                }
                Err(e) => eprintln!("vibenote: paste: {e}"),
            },
        }
    }

    fn paste_own(&mut self) {
        match self.engine.execute("clipboard.paste", serde_json::json!({})) {
            Ok(_) | Err(vibenote_engine::EngineError::NotImplemented(_)) => {}
            Err(e) => eprintln!("vibenote: clipboard.paste: {e}"),
        }
        self.dirty = true;
    }

    /// Where pasted things go (canvas units): the drop point, else the mouse if it is over
    /// the canvas, else the middle of the view.
    fn paste_point(&self, at: Option<(f64, f64)>) -> (f64, f64) {
        let surface = at.unwrap_or_else(|| {
            let (x, y) = self.pointer_pos;
            if !self.pointer_in_chrome && x > 0.0 && y > 0.0 && !self.chrome.covers(x, y) {
                (x, y)
            } else {
                (f64::from(self.width) / 2.0, f64::from(self.height) / 2.0)
            }
        });
        self.engine.session.view.to_canvas(surface.0, surface.1)
    }

    /// Data read from a paste or drop: files become PDFs, images, audio or text boxes, image
    /// bytes an image, text goes into the box being typed into or a new text box. What was
    /// inserted ends up selected (drag it into place).
    fn insert_pasted(&mut self, p: Pasted) {
        if let Some(token) = p.dnd
            && let Some(i) = self.dnd.iter().position(|(t, _)| *t == token)
        {
            let (_, offer) = self.dnd.remove(i);
            offer.finish();
            offer.destroy();
        }
        let data = match p.data {
            Ok(d) => d,
            Err(e) => return eprintln!("vibenote: paste: {e}"),
        };
        let (x, y) = self.paste_point(p.at);
        let zoom = self.engine.session.view.zoom.max(1e-6);
        let mut inserted: Vec<u64> = Vec::new();
        let run = |app: &mut App, cmd: &str, params: serde_json::Value| -> Option<serde_json::Value> {
            match app.engine.execute(cmd, params) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("vibenote: {cmd}: {e}");
                    None
                }
            }
        };
        let text_box = |content: String| serde_json::json!({"at": [x, y], "width": 600.0 / zoom, "size": 16.0 / zoom, "content": content});
        match p.kind {
            Kind::Own => {}
            Kind::Text(_) => {
                let text = String::from_utf8_lossy(&data).into_owned();
                if self.engine.session.text_edit.is_some() {
                    run(self, "text.input", serde_json::json!({ "text": text }));
                } else if !text.trim().is_empty()
                    && let Some(r) = run(self, "text.create", text_box(text))
                {
                    inserted.extend(r.get("id").and_then(serde_json::Value::as_u64));
                }
            }
            Kind::Image(mime) => {
                let path = std::env::temp_dir().join(format!(
                    "vibenote-paste-{}-{}.{}",
                    std::process::id(),
                    mono_ns(),
                    clip::image_ext(&mime)
                ));
                match std::fs::write(&path, &data) {
                    Ok(()) => {
                        let r = run(self, "image.import", serde_json::json!({"path": path, "at": [x, y]}));
                        inserted.extend(r.and_then(|r| r.get("id").and_then(serde_json::Value::as_u64)));
                        let _ = std::fs::remove_file(&path);
                    }
                    Err(e) => eprintln!("vibenote: paste: {e}"),
                }
            }
            Kind::Files(_) => {
                let list = String::from_utf8_lossy(&data).into_owned();
                // Files side by side from the paste point, each below the previous one.
                let mut y_at = y;
                for path in clip::uri_paths(&list) {
                    let at = serde_json::json!([x, y_at]);
                    let r = match clip::file_kind(&path) {
                        Some(clip::FileKind::Pdf) => {
                            run(self, "pdf.import", serde_json::json!({"path": path, "at": at}))
                        }
                        Some(clip::FileKind::Image) => {
                            run(self, "image.import", serde_json::json!({"path": path, "at": at}))
                        }
                        Some(clip::FileKind::Audio) => {
                            run(self, "audio.import", serde_json::json!({"path": path, "at": at}))
                        }
                        Some(clip::FileKind::Text) => match std::fs::metadata(&path) {
                            Ok(m) if m.len() <= 1 << 20 => match std::fs::read_to_string(&path) {
                                Ok(t) => run(self, "text.create", {
                                    let mut v = text_box(t);
                                    if let Some(o) = v.as_object_mut() {
                                        o.insert("at".into(), at);
                                    }
                                    v
                                }),
                                Err(e) => {
                                    eprintln!("vibenote: {}: {e}", path.display());
                                    None
                                }
                            },
                            _ => {
                                eprintln!("vibenote: {}: text file too large", path.display());
                                None
                            }
                        },
                        None => {
                            eprintln!("vibenote: {}: not a PDF, image, audio or text file", path.display());
                            None
                        }
                    };
                    let Some(id) = r.and_then(|r| r.get("id").and_then(serde_json::Value::as_u64)) else {
                        continue;
                    };
                    inserted.push(id);
                    let bottom = self
                        .engine
                        .session
                        .current_note
                        .and_then(|n| self.engine.session.ws.canvas(n).ok())
                        .and_then(|c| c.objects.get(&vibenote_model::ObjectId(id)))
                        .and_then(|o| o.kind.bounds())
                        .map(|b| b.max.y);
                    y_at = bottom.map_or(y_at + 40.0 / zoom, |b| b + 24.0 / zoom);
                }
            }
        }
        if !inserted.is_empty() {
            run(self, "selection.set", serde_json::json!({ "ids": inserted }));
        }
        self.chrome.invalidate();
        self.dirty = true;
    }

    /// Brings the mouse's and the pens' cursors in line with what they would do now: the
    /// chrome's cursor over the chrome, the engine's hint over the canvas (R-PEN-011).
    fn update_cursors(&mut self) {
        let hint = self.engine.session.cursor_hint();
        let over_chrome = self.pointer_in_chrome || self.mouse_on_chrome;
        let want = if over_chrome {
            crate::cursor::for_egui(self.chrome.cursor())
        } else {
            crate::cursor::for_hint(hint, self.space_held)
        };
        let pointer = self.pointer.clone();
        self.pointer_cursor.show(want, |serial| {
            if let Some(p) = &pointer {
                p.set_cursor(serial, None, 0, 0);
            }
        });
        let pen_on_chrome = self.pen_on_chrome || self.pen_pos.is_some_and(|(x, y)| self.chrome.covers(x, y));
        let want = if pen_on_chrome {
            crate::cursor::for_egui(self.chrome.cursor())
        } else {
            crate::cursor::for_hint(hint, false)
        };
        for t in &mut self.tools {
            let tool = t.tool.clone();
            t.cursor.show(want, |serial| tool.set_cursor(serial, None, 0, 0));
        }
    }

    /// Pen frames → engine, on the event (ADR 0001 rule 1).
    fn pen(&mut self, events: Vec<PenEvent>) {
        if events.is_empty() {
            return;
        }
        for e in &events {
            self.pen_pos = match e.kind {
                PenEventKind::ProximityOut => None,
                _ => Some((e.x, e.y)),
            };
        }
        // A pen touching the chrome works it like a mouse (until lifted); never draws there.
        let starts_on_chrome = self.engine.session.live.stroke.is_none()
            && events
                .iter()
                .any(|e| e.kind == PenEventKind::Down && self.chrome.covers(e.x, e.y));
        if self.pen_on_chrome || starts_on_chrome {
            self.pen_to_chrome(&events);
            self.dirty = true;
            return;
        }
        if let Some(l) = self.latency.as_mut() {
            let ns = mono_ns();
            for e in &events {
                l.input(e, ns);
            }
        }
        match self
            .engine
            .execute("input.pen", serde_json::json!({ "events": events }))
        {
            Ok(r) => {
                if let Some(errs) = r.get("errors").and_then(serde_json::Value::as_array) {
                    for e in errs {
                        eprintln!("vibenote: input.pen: {e}");
                        if let Some(msg) = e.as_str().filter(|m| m.contains("audio")) {
                            self.chrome.notify(friendly_error("audio", msg));
                        }
                    }
                }
            }
            Err(e) => eprintln!("vibenote: input.pen: {e}"),
        }
        self.dirty = true;
    }

    fn pen_to_chrome(&mut self, events: &[PenEvent]) {
        let m = self.chrome.modifiers();
        for e in events {
            #[allow(clippy::cast_possible_truncation)] // logical px
            let pos = egui::pos2(e.x as f32, e.y as f32);
            match e.kind {
                PenEventKind::Down => {
                    self.pen_on_chrome = true;
                    self.chrome.event(egui::Event::PointerMoved(pos));
                    self.chrome.event(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    });
                }
                PenEventKind::Motion if self.pen_on_chrome => self.chrome.event(egui::Event::PointerMoved(pos)),
                PenEventKind::Up if self.pen_on_chrome => {
                    self.pen_on_chrome = false;
                    self.chrome.event(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    });
                }
                PenEventKind::ProximityOut => {
                    self.pen_on_chrome = false;
                    self.chrome.event(egui::Event::PointerGone);
                }
                _ => {}
            }
        }
    }

    /// Runs what the chrome asked for (engine commands; search results go back to it).
    fn apply_actions(&mut self, actions: Vec<Action>) {
        if actions.is_empty() {
            return;
        }
        self.apply_each(actions);
        self.open_requested_panel();
    }

    fn apply_each(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Cmd(name, params) => {
                    let params = self.ui_params(name, params);
                    match self.engine.execute(name, params.clone()) {
                        Ok(_) if name == "clipboard.copy" || name == "clipboard.cut" => self.claim_clipboard(),
                        Ok(r) if name == "workspace.folder.create" || name == "workspace.note.create" => {
                            self.chrome.created(name, &params, &r);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("vibenote: {name}: {e}");
                            self.chrome.notify(friendly_error(name, &e.to_string()));
                        }
                    }
                }
                Action::Show { note, region } => {
                    let viewport = [f64::from(self.width), f64::from(self.height)];
                    if let Err(e) = self
                        .engine
                        .execute("note.open", serde_json::json!({"id": note, "viewport": viewport}))
                    {
                        eprintln!("vibenote: note.open: {e}");
                        continue;
                    }
                    if let Some([x0, y0, x1, y1]) = region {
                        // Centre the region: content moves by (origin − target origin) × zoom.
                        let v = self.engine.session.view;
                        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                        let tx = cx - viewport[0] / 2.0 / v.zoom;
                        let ty = cy - viewport[1] / 2.0 / v.zoom;
                        let pan =
                            serde_json::json!({"dx": (v.origin_x - tx) * v.zoom, "dy": (v.origin_y - ty) * v.zoom});
                        if let Err(e) = self.engine.execute("view.pan", pan) {
                            eprintln!("vibenote: view.pan: {e}");
                        }
                    }
                }
                Action::FinderQuery { kind, query } => self.finder_query(kind, &query),
                Action::FinderOpen(r) => {
                    let viewport = [f64::from(self.width), f64::from(self.height)];
                    let f = |k: &str| r.get(k).cloned().unwrap_or(serde_json::Value::Null);
                    let params = serde_json::json!({
                        "note": f("note"), "object": f("object"), "offset": f("offset"),
                        "region": f("region"), "viewport": viewport,
                    });
                    if let Err(e) = self.engine.execute("finder.open", params) {
                        eprintln!("vibenote: finder.open: {e}");
                    }
                    self.dirty = true;
                }
                Action::Search(text) => {
                    match self.engine.execute(
                        "search.query",
                        serde_json::json!({"text": text, "limit": 200, "wait": false}),
                    ) {
                        Ok(r) => self.chrome.set_results(&r),
                        Err(e) => eprintln!("vibenote: search: {e}"),
                    }
                }
                Action::Paste => self.paste(),
                Action::Find(text) => {
                    if text.trim().is_empty() {
                        self.chrome.set_regions(&serde_json::json!({}));
                        continue;
                    }
                    match self
                        .engine
                        .execute("search.in_note", serde_json::json!({"text": text, "wait": false}))
                    {
                        Ok(r) => self.chrome.set_regions(&r),
                        Err(e) => eprintln!("vibenote: find: {e}"),
                    }
                }
            }
        }
        if self.engine.session.ui.quit {
            self.exit = true;
        }
        self.chrome.invalidate();
        self.dirty = true;
    }

    /// A key for a chrome text field (named keys as `Key` events, typed text as `Text`).
    fn chrome_key(&mut self, ev: &KeyEvent, pressed: bool, repeat: bool) {
        let modifiers = self.chrome.modifiers();
        if let Some(key) = egui_key(ev.keysym) {
            self.chrome.event(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat,
                modifiers,
            });
        }
        if pressed
            && !self.modifiers.ctrl
            && !self.modifiers.alt
            && let Some(t) = ev.utf8.as_deref().filter(|t| !t.chars().any(char::is_control))
        {
            self.chrome.event(egui::Event::Text(t.to_string()));
        }
    }

    /// A key for the text box being edited; false if it is not for the text box (shortcuts
    /// like Ctrl+Z go to the keymap).
    fn text_key(&mut self, ev: &KeyEvent) -> bool {
        let m = self.modifiers;
        let shift = m.shift;
        let t_ms = mono_us() / 1000;
        let vim = self.engine.session.settings.vim;
        let params = if vim && m.ctrl && !m.alt && matches!(ev.keysym, Keysym::r | Keysym::R) {
            // Vim's redo (R-VIM-002).
            (
                "text.input",
                serde_json::json!({"text": "r", "ctrl": true, "t_ms": t_ms}),
            )
        } else if m.ctrl && !m.alt {
            let style = |f: &str| ("text.style", serde_json::json!({ "format": f }));
            match ev.keysym {
                Keysym::a | Keysym::A => ("text.input", serde_json::json!({"key": "select_all"})),
                Keysym::b | Keysym::B => style("bold"),
                Keysym::i | Keysym::I => style("italic"),
                Keysym::u | Keysym::U => style("underline"),
                _ => return false,
            }
        } else {
            let key = match ev.keysym {
                Keysym::BackSpace => Some("backspace"),
                Keysym::Delete => Some("delete"),
                Keysym::Left => Some("left"),
                Keysym::Right => Some("right"),
                Keysym::Up => Some("up"),
                Keysym::Down => Some("down"),
                Keysym::Home => Some("home"),
                Keysym::End => Some("end"),
                Keysym::Return | Keysym::KP_Enter => Some("enter"),
                Keysym::Escape => Some("escape"),
                // Indent / dedent (R-TEXT-007, R-VIM-002).
                Keysym::Tab | Keysym::ISO_Left_Tab => Some("tab"),
                _ => None,
            };
            match (key, ev.utf8.as_deref()) {
                (Some(k), _) => (
                    "text.input",
                    serde_json::json!({"key": k, "shift": shift, "t_ms": t_ms}),
                ),
                (None, Some(t)) if !t.is_empty() && !t.chars().any(char::is_control) => {
                    ("text.input", serde_json::json!({ "text": t, "t_ms": t_ms }))
                }
                (None, _) => return !m.alt && ev.keysym.key_char().is_some(),
            }
        };
        if let Err(e) = self.engine.execute(params.0, params.1) {
            eprintln!("vibenote: {}: {e}", params.0);
        }
        // Leader keys typed in the box may ask for the finder (R-VIM-004).
        self.open_requested_panel();
        self.chrome.invalidate();
        true
    }

    /// Panels the engine asked for (the finder from leader keys or Ctrl+P; R-VIM-005).
    fn open_requested_panel(&mut self) {
        let kind = match self.engine.session.ui.panel {
            Some("finder_text") => "text",
            Some("finder_notes" | "quickopen") => "notes",
            _ => return,
        };
        self.engine.session.ui.panel = None;
        self.chrome.open_finder(kind);
        // The first results (empty query) right away.
        self.finder_query(kind, "");
    }

    fn finder_query(&mut self, kind: &str, query: &str) {
        match self.engine.execute(
            "finder.query",
            serde_json::json!({"kind": kind, "query": query, "limit": 50}),
        ) {
            Ok(r) => self.chrome.set_finder_results(query, &r),
            Err(e) => eprintln!("vibenote: finder.query: {e}"),
        }
    }

    /// A tap in a text box: the caret goes where the layout puts that point.
    fn place_caret(&mut self) {
        let Some(ed) = self.engine.session.text_edit.clone() else {
            return;
        };
        let Some(at) = ed.place else { return };
        let Some(item) = self.engine.session.edited_text_item() else {
            return;
        };
        let mut hit = |p: (f64, f64)| match (self.gpu.as_mut(), self.renderer.as_mut()) {
            (Some(g), _) => TextLayout::hit(&mut g.renderer, &item, p),
            (None, Some(r)) => TextLayout::hit(r, &item, p),
            (None, None) => u32::MAX, // no layout: the end
        };
        let offset = hit(at);
        // A drag selects from where it started (R-TEXT-005).
        let anchor = ed.place_from.map(&mut hit);
        let params = serde_json::json!({
            "offset": offset,
            "word": ed.word,
            "line": ed.line,
            "extend": ed.extend,
            "anchor": anchor,
        });
        if let Err(e) = self.engine.execute("text.caret", params) {
            eprintln!("vibenote: text.caret: {e}");
        }
        self.dirty = true;
    }

    /// The caret, the selection and the outline of the text box being edited, as overlay
    /// strokes (canvas units).
    fn text_overlay(&mut self) -> Vec<vibenote_model::Stroke> {
        let s = &self.engine.session;
        let (Some(ed), Some(item)) = (s.text_edit.clone(), s.edited_text_item()) else {
            return Vec::new();
        };
        let zoom = if s.view.zoom.is_finite() && s.view.zoom > 0.0 {
            s.view.zoom
        } else {
            1.0
        };
        // Vim's Visual modes select up to and including the cursor's character (R-VIM-002).
        let (a, b) = s.vim_selection().unwrap_or_else(|| ed.range());
        let end = u32::try_from(item.content.chars().count()).unwrap_or(u32::MAX);
        // Normal mode: a block over the cursor's character (none on an empty line or past the end).
        let block = s
            .vim_block_cursor()
            .filter(|&c| c < end && item.content.chars().nth(usize::try_from(c).unwrap_or(usize::MAX)) != Some('\n'));
        let measure = |r: &mut dyn TextLayout| TextGeom {
            caret: r.caret(&item, ed.caret),
            last: r.caret(&item, end),
            ranges: if a < b { r.ranges(&item, a, b) } else { Vec::new() },
            block: block
                .map(|c| r.ranges(&item, c, c.saturating_add(1)))
                .unwrap_or_default(),
        };
        let g = match (self.gpu.as_mut(), self.renderer.as_mut()) {
            (Some(gpu), _) => measure(&mut gpu.renderer),
            (None, Some(h)) => measure(h),
            (None, None) => {
                let h = f64::from(item.size) * 1.3;
                TextGeom {
                    caret: (item.origin.x, item.origin.y, h),
                    last: (item.origin.x, item.origin.y, h),
                    ranges: Vec::new(),
                    block: Vec::new(),
                }
            }
        };
        let line = |pts: &[(f64, f64)], color: u32, w: f64| vibenote_model::Stroke {
            tool: vibenote_model::Tool::Pen,
            color: vibenote_model::Color(color),
            #[allow(clippy::cast_possible_truncation)] // a few pixels in canvas units
            width: w as f32,
            samples: pts
                .iter()
                .map(|&(x, y)| vibenote_model::Sample {
                    x,
                    y,
                    pressure: 1.0,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    t_us: 0,
                })
                .collect(),
        };
        let mut out = Vec::new();
        // The box: as wide as its text (at least a few characters), down to the last line
        // (R-TEXT-004); with vim on the whole wrap width with padding, like an editor (R-VIM-001).
        let (x0, y0, x1, y1) = if item.mono {
            let f = vibenote_engine::text_frame(&item);
            let pad = vibenote_engine::vim_pad(item.size);
            (f.min.x, f.min.y, f.max.x, f.max.y.max(g.last.1 + g.last.2 + pad))
        } else {
            let pad = 4.0 / zoom;
            let (w, _) = vibenote_render::text_measure(&item);
            let w = w.max(f64::from(item.size) * 4.0).min(item.width);
            (
                item.origin.x - pad,
                item.origin.y - pad,
                item.origin.x + w + pad,
                g.last.1 + g.last.2 + pad,
            )
        };
        out.push(line(
            &[(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
            TEXT_BOX_COLOR,
            1.0 / zoom,
        ));
        // Rectangles as rows of round-capped strokes that end exactly at the rectangle's edges
        // (one thick stroke's caps spilled half a line height onto the neighbouring letters).
        let fill = |out: &mut Vec<vibenote_model::Stroke>, r: [f64; 4], color: u32| {
            let (w, h) = ((r[2] - r[0]).max(0.0), (r[3] - r[1]).max(0.0));
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let t = (h / 8.0).min(w / 2.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few rows
            let rows = ((h / t).ceil() as usize).clamp(1, 64);
            #[allow(clippy::cast_precision_loss)]
            let step = if rows > 1 {
                (h - t) / rows.saturating_sub(1) as f64
            } else {
                0.0
            };
            for k in 0..rows {
                #[allow(clippy::cast_precision_loss)]
                let y = r[1] + t / 2.0 + step * k as f64;
                out.push(line(&[(r[0] + t / 2.0, y), (r[2] - t / 2.0, y)], color, t));
            }
        };
        let ranges = if item.mono {
            let linewise = s.vim_linewise();
            let cell = 0.6 * f64::from(item.size);
            full_line_rows(&g.ranges, item.origin.x, item.origin.x + item.width, linewise, cell)
        } else {
            g.ranges.clone()
        };
        for r in &ranges {
            fill(&mut out, *r, TEXT_SELECTION_COLOR);
        }
        let (x, top, h) = g.caret;
        if s.vim_block_cursor().is_some() {
            // Normal mode: a block (half an em wide on an empty line).
            let r = g
                .block
                .first()
                .copied()
                .unwrap_or([x, top, x + 0.55 * f64::from(item.size), top + h]);
            fill(&mut out, r, TEXT_BLOCK_COLOR);
        } else {
            out.push(line(&[(x, top), (x, top + h)], TEXT_CARET_COLOR, 1.5 / zoom));
        }
        out
    }

    /// Context a command needs that only the UI knows (keys and buttons carry no parameters).
    fn ui_params(&self, cmd: &str, params: serde_json::Value) -> serde_json::Value {
        let centre = [f64::from(self.width) / 2.0, f64::from(self.height) / 2.0];
        match cmd {
            "view.zoom_in" | "view.zoom_out" | "view.zoom_reset" => serde_json::json!({ "anchor": centre }),
            "view.zoom_fit" => serde_json::json!({"viewport": [f64::from(self.width), f64::from(self.height)]}),
            _ => params,
        }
    }

    /// Closing mid-stroke must not lose ink: commit the live stroke as if the pen was lifted.
    fn finish_live_stroke(&mut self) {
        let Some(ls) = self.engine.session.live.stroke.as_ref() else {
            return;
        };
        let t = ls.t0_us.saturating_add(ls.samples.last().map_or(0, |x| x.t_us));
        let tool = ls.tool;
        let up = serde_json::json!({"t_us": t, "kind": "up", "tool": tool, "x": 0.0, "y": 0.0});
        if let Err(e) = self.engine.execute("input.pen", serde_json::json!({ "events": [up] })) {
            eprintln!("vibenote: finishing the live stroke: {e}");
        }
    }

    fn request_draw(&mut self, _qh: &QueueHandle<App>) {
        self.dirty = true;
    }

    /// Draws if something changed: GPU path at once, shm fallback when its frame callback came.
    fn redraw(&mut self, qh: &QueueHandle<App>) {
        if !self.dirty || !self.configured {
            return;
        }
        if self.gpu.is_some() {
            self.draw_gpu(qh);
        } else if !self.frame_pending {
            self.draw(qh);
        }
    }

    fn pixel_size(&self) -> (u32, u32) {
        if self.viewport.is_some() {
            (scaled(self.width, self.scale120), scaled(self.height, self.scale120))
        } else {
            (
                self.width.saturating_mul(self.scale120 / 120),
                self.height.saturating_mul(self.scale120 / 120),
            )
        }
    }

    fn set_destination(&self) {
        if let Some(vp) = &self.viewport {
            vp.set_destination(
                i32::try_from(self.width).unwrap_or(i32::MAX),
                i32::try_from(self.height).unwrap_or(i32::MAX),
            );
        }
    }

    /// What the chrome shows of the engine besides the workspace (selection bar, text format
    /// group, suggestion chips, their positions): when it changes, egui runs again.
    fn chrome_state(&self) -> u64 {
        let s = &self.engine.session;
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        for id in &s.selection.ids {
            mix(*id);
        }
        if let Some(r) = s.selection_rect() {
            for v in [r.min.x, r.min.y, r.max.x, r.max.y] {
                mix(v.to_bits());
            }
        }
        mix(u64::from(s.text_edit.is_some()));
        // Vim's mode and unfinished keys show in the status pill.
        if let Some(v) = s.text_edit.as_ref().and_then(|e| e.vim.as_ref()) {
            mix(v.mode as u64);
            for b in v.pending.bytes() {
                mix(u64::from(b));
            }
        }
        mix(u64::from(s.settings.vim));
        mix(u64::from(s.live.drag.is_some()));
        mix(u64::try_from(s.suggestions.len()).unwrap_or(0));
        mix(s.settings.tool as u64);
        mix(u64::from(s.settings.pen_color.0));
        mix(u64::from(s.settings.highlighter_color.0));
        // The recording time label (R-AUDIO-004) and the ruler's angle label (R-RULER-001).
        if let Some(t) = s.audio.recording_start_us() {
            mix(vibenote_engine::audio::now_us().saturating_sub(t) / 1_000_000);
        }
        // The backup's status line in the settings (R-BACKUP-003).
        if let Some(b) = self.engine.backup_status() {
            for x in b.to_string().bytes() {
                mix(u64::from(x));
            }
        }
        mix(u64::from(s.ruler.visible));
        if s.ruler.visible {
            for v in [s.ruler.x, s.ruler.y, s.ruler.angle_deg] {
                mix(v.to_bits());
            }
        }
        if !s.selection.ids.is_empty() || !s.suggestions.is_empty() || s.ruler.visible {
            for v in [s.view.origin_x, s.view.origin_y, s.view.zoom] {
                mix(v.to_bits());
            }
        }
        h
    }

    fn draw_gpu(&mut self, qh: &QueueHandle<App>) {
        let (pw, ph) = self.pixel_size();
        let state = self.chrome_state();
        if state != self.chrome_seen {
            self.chrome_seen = state;
            self.chrome.invalidate();
        }
        // The chrome first: what it asks for changes what the canvas shows in this frame.
        let v = self.engine.session.view;
        let vm = ViewMap {
            origin_x: v.origin_x,
            origin_y: v.origin_y,
            scale: v.zoom,
        };
        if self.chrome.needs_run(vm) {
            #[allow(clippy::cast_precision_loss)] // window sizes
            let logical = (self.width as f32, self.height as f32);
            #[allow(clippy::cast_precision_loss)] // scale × 120
            let ppp = self.scale120 as f32 / 120.0;
            let actions = self.chrome.run(&self.engine, logical, ppp, vm);
            if !actions.is_empty() {
                self.apply_actions(actions);
                // The chrome shows engine state the actions changed (a mode, a selection):
                // run it again next frame.
                self.chrome.invalidate();
            }
        }
        // The text box overlay after the chrome's actions (a finder jump starts editing).
        let text = self.text_overlay();
        let Some(vp) = self.view(pw, ph) else {
            eprintln!("vibenote: window {pw}x{ph} px is beyond the supported size; not drawn");
            self.dirty = false;
            return;
        };
        let s = &self.engine.session;
        let Some(note) = s.current_note else { return };
        let Ok(canvas) = s.ws.canvas(note) else { return };
        let changes = s.change_log(note);
        let live: Vec<_> = s.live_preview().into_iter().chain(s.overlay()).chain(text).collect();
        let live_refs: Vec<_> = live.iter().collect();
        self.set_destination();
        let feedback = match (&mut self.latency, &self.presentation) {
            (Some(l), Some(p)) => Some((l, p, self.window.wl_surface().clone())),
            _ => None,
        };
        let pen_near = s.live.tool.is_some() || s.live.stroke.is_some();
        let background = s.background_of(note);
        let images = page_quads(s, &vp);
        let (texts, vectors) = s.extras_in_view(
            (vp.origin_x, vp.origin_y),
            vp.zoom,
            (f64::from(vp.width), f64::from(vp.height)),
        );
        let display = s.display_transforms(note);
        let chrome = &mut self.chrome;
        let Some(gpu) = self.gpu.as_mut() else { return };
        gpu.renderer.set_background(background);
        gpu.renderer.set_display(display);
        gpu.renderer.set_revealed(s.revealed.iter().copied().collect());
        gpu.renderer.set_images(images);
        gpu.renderer.set_extras(texts, vectors);
        // Bulk loading of a large note waits while the pen is near (ink first).
        gpu.renderer.set_budget(if pen_near {
            vibenote_render::FrameBudget::PEN
        } else {
            vibenote_render::FrameBudget::APP
        });
        // Feedback attaches to the next commit, which is the present below.
        let before_present = |qh: &QueueHandle<App>| {
            if let Some((l, p, surface)) = feedback {
                let f = l.draw(mono_ns());
                p.feedback(&surface, qh, Feedback(f));
            }
        };
        let scene = vibenote_render::Scene {
            canvas,
            changes,
            live: &live_refs,
            viewport: vp,
        };
        let mut overlay =
            |d: &wgpu::Device, q: &wgpu::Queue, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, f| {
                chrome.paint(d, q, enc, view, f, [pw, ph]);
            };
        let drawn = gpu.draw(&scene, || before_present(qh), &mut overlay);
        let fault = gpu.renderer.take_fault();
        self.retry = false;
        match drawn {
            // Actions of this frame's chrome may have changed what it shows: one more frame.
            Ok(Drawn::Presented) => {
                self.dirty = self.chrome_state() != self.chrome_seen || self.chrome.wants_frame();
            }
            Ok(Drawn::Partial | Drawn::Retry) => self.retry = true,
            Ok(Drawn::Skip) => {} // occluded: draw on the next event
            Ok(Drawn::SurfaceLost) => return self.recover_gpu("surface lost"),
            Err(e) => eprintln!("vibenote: draw: {e}"),
        }
        match fault {
            Some(GpuFault::Lost(m)) => self.recover_gpu(&m),
            Some(GpuFault::Error(m)) => eprintln!("vibenote: GPU error: {m}"),
            None => {}
        }
    }

    /// Recreates the GPU surface and device after a loss (driver reset, suspend). After a few
    /// failed attempts the slow shm fallback takes over; the app never exits because of it.
    fn recover_gpu(&mut self, why: &str) {
        eprintln!("vibenote: GPU {why}; recreating");
        self.gpu = None; // drops the old wgpu surface before a new one is attached
        self.dirty = true;
        self.retry = true;
        while self.gpu_recoveries < MAX_GPU_RECOVERIES {
            self.gpu_recoveries = self.gpu_recoveries.saturating_add(1);
            match Gpu::new(&self.conn, &self.window) {
                Ok(g) => {
                    self.gpu = Some(g);
                    return;
                }
                Err(e) => eprintln!("vibenote: GPU recreation failed: {e}"),
            }
        }
        self.retry = false;
        self.frame_pending = false;
        if self.renderer.is_none() {
            self.renderer = Headless::new()
                .inspect_err(|e| eprintln!("vibenote: renderer unavailable ({e}); showing blank paper"))
                .ok();
        }
    }

    /// The engine view → render viewport for a `pw`×`ph` pixel target.
    fn view(&self, pw: u32, ph: u32) -> Option<Viewport> {
        let v = self.engine.session.view;
        let scale = f64::from(self.scale120) / 120.0;
        let vp = Viewport {
            origin_x: v.origin_x,
            origin_y: v.origin_y,
            zoom: v.zoom * scale,
            width: pw,
            height: ph,
        };
        vp.check().ok().map(|()| vp)
    }

    fn draw(&mut self, qh: &QueueHandle<App>) {
        let (pw, ph) = self.pixel_size();
        // Same cap as the GPU path (Viewport::check, 8192²): a hostile configure must not
        // allocate a multi-GB shm buffer.
        if self.view(pw, ph).is_none() {
            eprintln!("vibenote: window {pw}x{ph} px is beyond the supported size; not drawn");
            self.dirty = false;
            return;
        }
        let (Ok(w), Ok(h)) = (i32::try_from(pw), i32::try_from(ph)) else {
            return;
        };
        let Some(stride) = w.checked_mul(4) else { return };
        let buffer_ok = self
            .buffer
            .as_ref()
            .is_some_and(|b| b.height() == h && b.stride() == stride);
        if !buffer_ok {
            match self.pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888) {
                Ok((b, _)) => self.buffer = Some(b),
                Err(e) => {
                    eprintln!("vibenote: shm buffer: {e}");
                    return;
                }
            }
        }
        let rendered = self.render(pw, ph);
        let complete = rendered.as_ref().is_none_or(|(_, c)| *c);
        let image = rendered.map(|(img, _)| img);
        self.set_destination();
        let Some(buffer) = self.buffer.as_mut() else { return };
        let canvas = match self.pool.canvas(buffer) {
            Some(c) => c,
            None => match self.pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888) {
                // The compositor still holds the previous buffer: use a second one.
                Ok((b, c)) => {
                    *buffer = b;
                    c
                }
                Err(e) => {
                    eprintln!("vibenote: shm buffer: {e}");
                    return;
                }
            },
        };
        match &image {
            Some(img) => copy_rgba_to_argb(&img.rgba, canvas),
            None => fill(canvas, PAPER),
        }
        let surface = self.window.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        surface.frame(qh, FrameCallbackData(surface.clone()));
        if let Err(e) = buffer.attach_to(surface) {
            eprintln!("vibenote: attach: {e}");
            return;
        }
        self.window.commit();
        self.frame_pending = true;
        // A large note arrives over several frames (bounded work per frame): keep drawing.
        self.dirty = !complete;
    }

    /// One frame and whether it shows the whole canvas.
    fn render(&mut self, pw: u32, ph: u32) -> Option<(vibenote_render::Image, bool)> {
        let vp = self.view(pw, ph)?;
        let text = self.text_overlay();
        let r = self.renderer.as_mut()?;
        let s = &self.engine.session;
        let note = s.current_note?;
        let canvas = s.ws.canvas(note).ok()?;
        let live: Vec<_> = s.live_preview().into_iter().chain(s.overlay()).chain(text).collect();
        let live_refs: Vec<_> = live.iter().collect();
        r.set_background(s.background_of(note));
        r.set_display(s.display_transforms(note));
        r.set_images(page_quads(s, &vp));
        let (texts, vectors) = s.extras_in_view(
            (vp.origin_x, vp.origin_y),
            vp.zoom,
            (f64::from(vp.width), f64::from(vp.height)),
        );
        r.set_extras(texts, vectors);
        let scene = vibenote_render::Scene {
            canvas,
            changes: s.change_log(note),
            live: &live_refs,
            viewport: vp,
        };
        match r.render_scene(&scene) {
            Ok((img, st)) if img.width == pw && img.height == ph => Some((img, st.complete)),
            Ok(_) => None,
            Err(e) => {
                eprintln!("vibenote: render: {e}");
                None
            }
        }
    }
}

/// GPU recreations before falling back to shm for the rest of the session.
const MAX_GPU_RECOVERIES: u32 = 3;

/// Result of one GPU frame attempt.
enum Drawn {
    Presented,
    /// Presented, but the canvas is not fully on the GPU yet: draw again soon (bounded work per
    /// frame, M3.1).
    Partial,
    /// Reconfigured (outdated) or acquire timed out: try again shortly.
    Retry,
    /// Window occluded: nothing to draw now.
    Skip,
    /// The surface must be recreated.
    SurfaceLost,
}

/// The window's GPU state: renderer + surface + its configuration.
struct Gpu {
    renderer: Renderer,
    surface: GpuSurface,
    format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha: wgpu::CompositeAlphaMode,
    size: (u32, u32),
}

impl Gpu {
    fn new(conn: &Connection, window: &Window) -> Result<Gpu, UiError> {
        let instance = vibenote_render::instance();
        let surface = GpuSurface::new(&instance, conn, window).map_err(wl)?;
        let (adapter, device, queue) = request_device(&instance, Some(surface.surface())).map_err(wl)?;
        let caps = surface.surface().get_capabilities(&adapter);
        // Plain 8-bit formats, like the headless renderer (goldens are non-sRGB RGBA8).
        let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| UiError::Wayland("surface has no formats".into()))?;
        // Never block the input thread on presentation (ADR 0001 rule 2).
        let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|m| caps.present_modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        let alpha = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let mut renderer = Renderer::new(&adapter, device, queue);
        renderer.set_budget(vibenote_render::FrameBudget::APP);
        renderer.warm_up(format);
        Ok(Gpu {
            renderer,
            surface,
            format,
            present_mode,
            alpha,
            size: (0, 0),
        })
    }

    fn configure(&mut self, w: u32, h: u32) {
        let cfg = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.format,
            width: w,
            height: h,
            present_mode: self.present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: self.alpha,
            view_formats: Vec::new(),
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        self.surface.surface().configure(self.renderer.device(), &cfg);
        self.size = (w, h);
    }

    /// Draws and presents one frame.
    fn draw(
        &mut self,
        scene: &vibenote_render::Scene<'_>,
        before_present: impl FnOnce(),
        overlay: &mut dyn FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
        ),
    ) -> Result<Drawn, vibenote_render::RenderError> {
        let vp = &scene.viewport;
        if self.size != (vp.width, vp.height) {
            self.configure(vp.width, vp.height);
        }
        let frame = match self.surface.surface().get_current_texture() {
            // Suboptimal still presents correctly; reconfiguring on it every frame would
            // recreate the swapchain on the pen path (M1 latency was measured this way).
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Validation => {
                self.configure(vp.width, vp.height);
                return Ok(Drawn::Retry);
            }
            wgpu::CurrentSurfaceTexture::Timeout => return Ok(Drawn::Retry),
            wgpu::CurrentSurfaceTexture::Occluded => return Ok(Drawn::Skip),
            wgpu::CurrentSurfaceTexture::Lost => return Ok(Drawn::SurfaceLost),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.renderer.device().create_command_encoder(&Default::default());
        let stats = self.renderer.encode_scene(&mut enc, &view, self.format, scene)?;
        overlay(
            self.renderer.device(),
            self.renderer.queue(),
            &mut enc,
            &view,
            self.format,
        );
        self.renderer.queue().submit([enc.finish()]);
        before_present();
        self.renderer.queue().present(frame);
        Ok(if stats.complete {
            Drawn::Presented
        } else {
            Drawn::Partial
        })
    }
}

/// RGBA (renderer) → little-endian ARGB8888 (wl_shm) = bytes B, G, R, A.
pub fn copy_rgba_to_argb(src: &[u8], dst: &mut [u8]) {
    for ([r, g, b, a], d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0) {
        *d = [*b, *g, *r, *a];
    }
}

fn fill(dst: &mut [u8], rgba: [u8; 4]) {
    let [r, g, b, a] = rgba;
    for d in dst.as_chunks_mut::<4>().0 {
        *d = [b, g, r, a];
    }
}

// ---- sctk handlers -------------------------------------------------------------------------

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, f: i32) {
        // Integer scale is only used without wp_fractional_scale_v1 + viewporter.
        if self.viewport.is_none() {
            let f = u32::try_from(f).unwrap_or(1).max(1);
            self.scale120 = f.saturating_mul(120);
            self.window.wl_surface().set_buffer_scale(i32::try_from(f).unwrap_or(1));
            self.request_draw(qh);
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty && self.gpu.is_none() {
            self.draw(qh);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &Window, c: WindowConfigure, _: u32) {
        self.width = c.new_size.0.map_or(self.width, |v| v.get());
        self.height = c.new_size.1.map_or(self.height, |v| v.get());
        self.configured = true;
        self.frame_pending = false;
        self.chrome.invalidate();
        if let Some(id) = self.open_note.take() {
            let viewport = [f64::from(self.width), f64::from(self.height)];
            if let Err(e) = self
                .engine
                .execute("note.open", serde_json::json!({"id": id, "viewport": viewport}))
            {
                eprintln!("vibenote: opening note {id}: {e}");
                self.exit = true;
            }
        }
        self.request_draw(qh);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.add_tablet_seat(&seat, qh);
        self.add_data_device(&seat, qh);
    }
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            if let (Some(g), Some(p)) = (&self.gestures, &self.pointer) {
                self.pinch = Some(g.get_pinch_gesture(p, qh, Plain));
            }
            if let (Some(m), Some(p)) = (&self.cursor_shapes, &self.pointer) {
                self.pointer_cursor = CursorSlot {
                    device: Some(m.get_pointer(p, qh, Plain)),
                    ..CursorSlot::default()
                };
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if cap == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            if let Some(g) = self.pinch.take() {
                g.destroy();
            }
            if let Some(d) = std::mem::take(&mut self.pointer_cursor).device {
                d.destroy();
            }
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.tablet_seats.retain(|(s, ts)| {
            let keep = *s != seat;
            if !keep {
                ts.destroy();
            }
            keep
        });
    }
}

impl KeyboardHandler for App {
    #[allow(clippy::too_many_arguments)]
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        ev: KeyEvent,
    ) {
        self.input_serial = serial;
        // A text field of the chrome has the keyboard.
        if self.chrome.wants_keyboard() {
            self.chrome_key(&ev, true, false);
            self.request_draw(qh);
            return;
        }
        // Paste: files, images or text from other apps, or our own objects (clipboard.rs).
        if self.modifiers.ctrl
            && !self.modifiers.alt
            && !self.chrome.wants_keyboard()
            && (ev.keysym == Keysym::v || ev.keysym == Keysym::V)
        {
            self.paste();
            self.request_draw(qh);
            return;
        }
        // A text box being edited gets typing and editing keys (R-TEXT-002).
        if self.engine.session.text_edit.is_some() && self.text_key(&ev) {
            self.open_requested_panel();
            self.request_draw(qh);
            return;
        }
        // Search and find (R-SEARCH-002) and closing them.
        if self.modifiers.ctrl && (ev.keysym == Keysym::f || ev.keysym == Keysym::F) {
            if self.modifiers.shift {
                self.chrome.open_search();
            } else {
                self.chrome.open_find();
            }
            self.request_draw(qh);
            return;
        }
        if ev.keysym == Keysym::Escape && self.chrome.close_panels() {
            self.request_draw(qh);
            return;
        }
        // Vim mode's leader keys on the canvas (R-VIM-004): Space f w / Space f f. Space stays
        // held for Space+Drag too.
        if self.engine.session.settings.vim
            && !self.modifiers.ctrl
            && !self.modifiers.alt
            && let Some(k) = if ev.keysym == Keysym::space {
                Some("space".to_string())
            } else {
                ev.utf8.clone().filter(|t| t.chars().count() == 1)
            }
        {
            let r = self
                .engine
                .execute("vim.key", serde_json::json!({"key": k, "t_ms": mono_us() / 1000}));
            if ev.keysym == Keysym::space {
                self.space_held = true;
            }
            if r.is_ok_and(|v| v.get("consumed") == Some(&serde_json::Value::Bool(true))) {
                self.open_requested_panel();
                self.request_draw(qh);
                return;
            }
        }
        // Shortcuts come from the engine's keymap (R-CMD-002); Space is held for Space+Drag.
        if ev.keysym == Keysym::space && !self.modifiers.ctrl {
            self.space_held = true;
            return;
        }
        let Some(key) = key_name(ev.keysym, &self.modifiers) else {
            return;
        };
        let Some(cmd) = self.engine.key_binding(&key) else {
            return;
        };
        let centre = [f64::from(self.width) / 2.0, f64::from(self.height) / 2.0];
        // Context the UI knows and the command needs (keys carry no parameters).
        let params = match cmd.as_str() {
            "view.zoom_in" | "view.zoom_out" | "view.zoom_reset" => serde_json::json!({ "anchor": centre }),
            "view.zoom_fit" => serde_json::json!({"viewport": [f64::from(self.width), f64::from(self.height)]}),
            // The ruler appears in the middle of the window (R-RULER-001).
            "ruler.toggle" if !self.engine.session.ruler.visible => {
                let (x, y) = self.engine.session.view.to_canvas(centre[0], centre[1] - 40.0);
                serde_json::json!({"visible": true, "at": [x, y]})
            }
            _ => serde_json::json!({}),
        };
        let done = self.engine.execute(&cmd, params);
        self.open_requested_panel();
        match done {
            Ok(_) if cmd == "clipboard.copy" || cmd == "clipboard.cut" => self.claim_clipboard(),
            Ok(_) => {}
            // Nothing selected (Delete/Escape) or a feature not built yet: not worth a message.
            Err(vibenote_engine::EngineError::NotImplemented(_)) => {}
            Err(e) => eprintln!("vibenote: {key} → {cmd}: {e}"),
        }
        self.chrome.invalidate();
        if self.engine.session.ui.quit {
            self.exit = true;
        }
        self.request_draw(qh);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        ev: KeyEvent,
    ) {
        if self.chrome.wants_keyboard() {
            self.chrome_key(&ev, true, true);
            self.request_draw(qh);
        } else if self.engine.session.text_edit.is_some() && self.text_key(&ev) {
            self.request_draw(qh);
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        ev: KeyEvent,
    ) {
        if ev.keysym == Keysym::space {
            self.space_held = false;
        }
        if self.chrome.wants_keyboard() {
            self.chrome_key(&ev, false, false);
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = m;
        self.chrome.set_modifiers(egui::Modifiers {
            alt: m.alt,
            ctrl: m.ctrl,
            shift: m.shift,
            mac_cmd: false,
            command: m.ctrl,
        });
    }
}

/// The egui key for a keysym (text fields: editing, navigation, shortcuts).
fn egui_key(k: Keysym) -> Option<egui::Key> {
    use egui::Key as E;
    Some(match k {
        Keysym::Return | Keysym::KP_Enter => E::Enter,
        Keysym::Escape => E::Escape,
        Keysym::BackSpace => E::Backspace,
        Keysym::Delete => E::Delete,
        Keysym::Tab => E::Tab,
        Keysym::Left => E::ArrowLeft,
        Keysym::Right => E::ArrowRight,
        Keysym::Up => E::ArrowUp,
        Keysym::Down => E::ArrowDown,
        Keysym::Home => E::Home,
        Keysym::End => E::End,
        Keysym::Page_Up => E::PageUp,
        Keysym::Page_Down => E::PageDown,
        _ => {
            // Letter keysyms are their ASCII codes.
            let c = char::from_u32(k.raw()).filter(char::is_ascii_alphabetic)?;
            E::from_name(&c.to_ascii_uppercase().to_string())?
        }
    })
}

/// Linux input code of the middle mouse button.
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;
/// Linux input code of the right mouse button (drag pans, like swiping on a touchpad).
const BTN_RIGHT: u32 = 0x111;
/// Zoom per wheel step (Ctrl+wheel).
const WHEEL_ZOOM: f64 = 1.1;

/// PDF pages in view as renderer images (cached bitmaps; missing ones are queued, never
/// waited for).
fn page_quads(s: &vibenote_engine::Session, vp: &Viewport) -> Vec<vibenote_render::ImageQuad> {
    s.images_in_view(
        (vp.origin_x, vp.origin_y),
        vp.zoom,
        (f64::from(vp.width), f64::from(vp.height)),
        false,
    )
    .into_iter()
    .map(|p| vibenote_render::ImageQuad {
        key: p.key,
        rect: p.rect,
        width: p.width,
        height: p.height,
        pixels: p.pixels,
    })
    .collect()
}

/// A key in the keymap's canonical form ("Ctrl+Shift+Z"), or `None` for keys it never binds.
fn key_name(k: Keysym, m: &Modifiers) -> Option<String> {
    let raw = k.raw();
    let name = match k {
        Keysym::Delete => "Delete".to_string(),
        Keysym::BackSpace => "Backspace".to_string(),
        Keysym::Escape => "Escape".to_string(),
        Keysym::Return | Keysym::KP_Enter => "Enter".to_string(),
        Keysym::Tab => "Tab".to_string(),
        Keysym::plus | Keysym::KP_Add => "Plus".to_string(),
        Keysym::minus | Keysym::KP_Subtract => "Minus".to_string(),
        Keysym::equal => "Equal".to_string(),
        Keysym::Up => "Up".to_string(),
        Keysym::Down => "Down".to_string(),
        Keysym::Left => "Left".to_string(),
        Keysym::Right => "Right".to_string(),
        Keysym::Home => "Home".to_string(),
        Keysym::End => "End".to_string(),
        Keysym::Prior => "PageUp".to_string(),
        Keysym::Next => "PageDown".to_string(),
        _ if (Keysym::F1.raw()..=Keysym::F12.raw()).contains(&raw) => {
            format!("F{}", raw.saturating_sub(Keysym::F1.raw()).saturating_add(1))
        }
        _ if (Keysym::KP_0.raw()..=Keysym::KP_9.raw()).contains(&raw) => {
            char::from_digit(raw.saturating_sub(Keysym::KP_0.raw()), 10)?.to_string()
        }
        _ => {
            let c = k.key_char().filter(char::is_ascii_alphanumeric)?;
            c.to_ascii_uppercase().to_string()
        }
    };
    let mut parts = Vec::new();
    for (on, n) in [(m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.logo, "Super")] {
        if on {
            parts.push(n.to_string());
        }
    }
    parts.push(name);
    Some(parts.join("+"))
}

/// An error message for the user: what failed and why, in a sentence.
fn friendly_error(what: &str, e: &str) -> String {
    let action = match what {
        "audio.record.start" => "Could not start recording",
        "audio.record.stop" => "Could not stop recording",
        "audio" | "audio.play" => "Could not play audio",
        w if w.starts_with("backup.") => "Backup",
        _ => "That did not work",
    };
    format!("{action}: {e}")
}

/// One `input.pointer` event. Stamped with CLOCK_MONOTONIC on receipt like pen events (the
/// compositor's millisecond times are on an unspecified clock, so holding a mouse stroke still
/// never reached the snap time, R-SHAPE-002).
fn pointer_cmd(
    kind: &str,
    _time_ms: u32,
    (x, y): (f64, f64),
    button: Option<u32>,
) -> (&'static str, serde_json::Value) {
    pointer_cmd_mods(kind, (x, y), button, false)
}

/// [`pointer_cmd`] with Shift (a Shift+click extends a text selection).
fn pointer_cmd_mods(
    kind: &str,
    (x, y): (f64, f64),
    button: Option<u32>,
    shift: bool,
) -> (&'static str, serde_json::Value) {
    let t_us = mono_us();
    let ev = serde_json::json!({"t_us": t_us, "kind": kind, "x": x, "y": y, "button": button, "shift": shift});
    ("input.pointer", serde_json::json!({ "events": [ev] }))
}

impl App {
    /// The mouse left the canvas: the engine stops drawing its footprint.
    fn pointer_hover_gone(&mut self) {
        if self.mouse_held.is_some() {
            return;
        }
        let (name, params) = pointer_cmd("leave", 0, self.pointer_pos, None);
        if let Err(e) = self.engine.execute(name, params) {
            eprintln!("vibenote: {name}: {e}");
        }
        self.dirty = true;
    }

    fn pointer_to_chrome(&mut self, ev: &PointerEvent) {
        use sctk::seat::pointer::PointerEventKind as K;
        let m = self.chrome.modifiers();
        #[allow(clippy::cast_possible_truncation)] // logical px
        let pos = egui::pos2(ev.position.0 as f32, ev.position.1 as f32);
        self.pointer_in_chrome = true;
        let button = |b: u32| match b {
            BTN_LEFT => Some(egui::PointerButton::Primary),
            BTN_MIDDLE => Some(egui::PointerButton::Middle),
            BTN_RIGHT => Some(egui::PointerButton::Secondary),
            _ => None,
        };
        match &ev.kind {
            K::Enter { .. } | K::Motion { .. } => self.chrome.event(egui::Event::PointerMoved(pos)),
            K::Leave { .. } => {
                self.pointer_in_chrome = false;
                self.chrome.event(egui::Event::PointerGone);
            }
            K::Press { button: b, .. } => {
                if let Some(button) = button(*b) {
                    self.mouse_on_chrome = true;
                    self.chrome.event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: true,
                        modifiers: m,
                    });
                }
            }
            K::Release { button: b, .. } => {
                self.mouse_on_chrome = false;
                if let Some(button) = button(*b) {
                    self.chrome.event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed: false,
                        modifiers: m,
                    });
                }
            }
            K::Axis {
                horizontal, vertical, ..
            } => {
                #[allow(clippy::cast_possible_truncation)] // scroll deltas
                let delta = egui::vec2(-horizontal.absolute as f32, -vertical.absolute as f32);
                self.chrome.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta,
                    modifiers: m,
                    phase: egui::TouchPhase::Move,
                });
            }
        }
    }
}

impl PointerHandler for App {
    /// Navigation with the mouse (R-CANVAS-002): wheel pans, Ctrl+wheel zooms at the cursor,
    /// middle- or right-drag pans. The left button draws with the selected tool (`input.pointer`, R-PEN-012).
    /// Motion is forwarded only while a button is held (the system cursor shows hover).
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use sctk::seat::pointer::PointerEventKind as K;
        for ev in events {
            if &ev.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = ev.position;
            self.pointer_pos = (x, y);
            if let K::Enter { serial } = ev.kind {
                self.pointer_cursor.enter(serial);
            }
            if let K::Press { serial, .. } = ev.kind {
                self.input_serial = serial;
            }
            if let K::Leave { .. } = ev.kind {
                self.pointer_cursor.serial = None;
                self.pointer_hover_gone();
            }
            // The chrome gets the pointer over it, and while a press that began on it is held.
            let on_chrome = self.mouse_on_chrome || (self.mouse_held.is_none() && self.chrome.covers(x, y));
            if on_chrome {
                self.pointer_hover_gone();
                self.pointer_to_chrome(ev);
                self.request_draw(qh);
                continue;
            }
            if std::mem::take(&mut self.pointer_in_chrome) {
                self.chrome.event(egui::Event::PointerGone);
                self.request_draw(qh);
            }
            let cmd = match &ev.kind {
                K::Press { time, button, .. }
                    if self.mouse_held.is_none() && matches!(*button, BTN_LEFT | BTN_MIDDLE | BTN_RIGHT) =>
                {
                    self.mouse_held = Some(*button);
                    // Space+Drag pans (R-CMD-002): the left button acts as the middle one.
                    let b = if self.space_held { BTN_MIDDLE } else { *button };
                    self.mouse_as = b;
                    Some(pointer_cmd_mods("down", (x, y), Some(b), self.modifiers.shift))
                }
                K::Release { time, button, .. } if self.mouse_held == Some(*button) => {
                    self.mouse_held = None;
                    Some(pointer_cmd("up", *time, (x, y), Some(self.mouse_as)))
                }
                // Without a button: hover, for the footprint and the cursor (R-PEN-011).
                K::Motion { time } => Some(pointer_cmd("motion", *time, (x, y), None)),
                K::Enter { .. } => Some(pointer_cmd("motion", 0, (x, y), None)),
                K::Axis {
                    horizontal, vertical, ..
                } if self.modifiers.ctrl => {
                    let steps = if vertical.discrete != 0 {
                        f64::from(vertical.discrete)
                    } else {
                        vertical.absolute / 10.0
                    };
                    (steps != 0.0)
                        .then(|| {
                            (
                                "view.zoom",
                                serde_json::json!({"factor": WHEEL_ZOOM.powf(-steps), "anchor": [x, y]}),
                            )
                        })
                        .or_else(|| {
                            (horizontal.absolute != 0.0)
                                .then(|| ("view.pan", serde_json::json!({"dx": -horizontal.absolute, "dy": 0.0})))
                        })
                }
                // The wheel over the ruler turns it (R-RULER-001): 1° a notch, 15° with Shift.
                K::Axis { vertical, .. }
                    if vertical.absolute != 0.0
                        && self
                            .engine
                            .session
                            .ruler
                            .part_at(self.engine.session.view.to_canvas(x, y), self.engine.session.view.zoom)
                            .is_some() =>
                {
                    let notch = if vertical.discrete != 0 {
                        f64::from(vertical.discrete)
                    } else {
                        vertical.absolute / 10.0
                    };
                    let by = if self.modifiers.shift { 15.0 } else { 1.0 } * notch.signum();
                    let a = self.engine.session.ruler.angle_deg + by;
                    Some((
                        "ruler.toggle",
                        serde_json::json!({"visible": true, "angle_deg": a, "snap": self.modifiers.shift}),
                    ))
                }
                K::Axis {
                    horizontal, vertical, ..
                } => {
                    let (dx, dy) = (-horizontal.absolute, -vertical.absolute);
                    (dx != 0.0 || dy != 0.0).then(|| ("view.pan", serde_json::json!({"dx": dx, "dy": dy})))
                }
                _ => None,
            };
            if let Some((name, params)) = cmd {
                match self.engine.execute(name, params) {
                    Ok(r) => {
                        for e in r
                            .get("errors")
                            .and_then(serde_json::Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            eprintln!("vibenote: {name}: {e}");
                            if let Some(msg) = e.as_str().filter(|m| m.contains("audio")) {
                                self.chrome.notify(friendly_error("audio", msg));
                            }
                        }
                        self.request_draw(qh);
                    }
                    Err(e) => eprintln!("vibenote: {name}: {e}"),
                }
            }
        }
    }
}

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        d: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
        // Accept files and images dropped from other apps (a copy).
        let Some(offer) = self
            .data_device
            .as_ref()
            .filter(|x| x.inner() == d)
            .and_then(|x| x.data().drag_offer())
        else {
            return;
        };
        let kind = offer.with_mime_types(|m| clip::choose(m, false));
        match kind {
            Some(k @ (Kind::Files(_) | Kind::Image(_) | Kind::Text(_))) => {
                offer.accept_mime_type(offer.serial, Some(k.mime().to_string()));
                offer.set_actions(DndAction::Copy, DndAction::Copy);
            }
            _ => offer.accept_mime_type(offer.serial, None),
        }
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, d: &WlDataDevice) {
        let Some(offer) = self
            .data_device
            .as_ref()
            .filter(|x| x.inner() == d)
            .and_then(|x| x.data().drag_offer())
        else {
            return;
        };
        let Some(kind) = offer
            .with_mime_types(|m| clip::choose(m, false))
            .filter(|k| *k != Kind::Own)
        else {
            return;
        };
        let at = Some((offer.x, offer.y));
        match offer.receive(kind.mime().to_string()) {
            Ok(pipe) => {
                use std::os::fd::AsFd;
                match pipe.as_fd().try_clone_to_owned() {
                    Ok(fd) => {
                        let token = self.dnd_next;
                        self.dnd_next = self.dnd_next.wrapping_add(1);
                        clip::read_async(fd, kind, at, Some(token), self.paste_tx.clone());
                        self.dnd.push((token, offer));
                    }
                    Err(e) => eprintln!("vibenote: drop: {e}"),
                }
            }
            Err(e) => eprintln!("vibenote: drop: {e}"),
        }
    }
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, _: DndAction) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    /// Our clipboard holds no bytes (the objects stay in the engine); the marker is enough.
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, fd: WritePipe) {
        drop(fd);
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.copy_source = None;
        }
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

sctk::delegate_dispatch2!(App);

// ---- protocols we bind ourselves --------------------------------------------------------

/// User data for objects vibenote binds itself (tablet-v2, fractional scale, viewporter).
pub struct Plain;

macro_rules! no_events {
    ($($i:ty),* $(,)?) => {$(
        impl Dispatch2<$i, App> for Plain {
            fn event(&self, _: &mut App, _: &$i, _: <$i as Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
        }
    )*};
}

no_events!(
    tm::ZwpTabletManagerV2,
    tab::ZwpTabletV2,
    pring::ZwpTabletPadRingV2,
    pstrip::ZwpTabletPadStripV2,
    wp_viewporter::WpViewporter,
    wp_viewport::WpViewport,
    fsm::WpFractionalScaleManagerV1,
    gestures::ZwpPointerGesturesV1,
    csm::WpCursorShapeManagerV1,
    csd::WpCursorShapeDeviceV1,
);

/// Touchpad pinch (R-CANVAS-002): zoom by the change in scale around the pointer, pan by the
/// fingers' movement.
impl Dispatch2<pinch::ZwpPointerGesturePinchV1, App> for Plain {
    fn event(
        &self,
        app: &mut App,
        _: &pinch::ZwpPointerGesturePinchV1,
        e: pinch::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        match e {
            pinch::Event::Begin { surface, .. } if &surface == app.window.wl_surface() => app.pinch_scale = 1.0,
            pinch::Event::Update { dx, dy, scale, .. } => {
                let factor = scale / app.pinch_scale;
                if factor.is_finite() && factor > 0.0 && scale.is_finite() && scale > 0.0 {
                    app.pinch_scale = scale;
                    let (x, y) = app.pointer_pos;
                    let cmds = [
                        ("view.pan", serde_json::json!({"dx": dx, "dy": dy})),
                        ("view.zoom", serde_json::json!({"factor": factor, "anchor": [x, y]})),
                    ];
                    for (name, params) in cmds {
                        if let Err(err) = app.engine.execute(name, params) {
                            eprintln!("vibenote: pinch {name}: {err}");
                        }
                    }
                    app.request_draw(qh);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch2<fs::WpFractionalScaleV1, App> for Plain {
    fn event(&self, app: &mut App, _: &fs::WpFractionalScaleV1, e: fs::Event, _: &Connection, qh: &QueueHandle<App>) {
        if let fs::Event::PreferredScale { scale } = e
            && scale > 0
            && scale != app.scale120
        {
            app.scale120 = scale;
            app.request_draw(qh);
        }
    }
}

impl Dispatch2<wp_presentation::WpPresentation, App> for Plain {
    fn event(
        &self,
        app: &mut App,
        _: &wp_presentation::WpPresentation,
        e: wp_presentation::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = e {
            app.clock_id = Some(clk_id);
        }
    }
}

/// User data of a presentation feedback: the frame id from the latency log.
pub struct Feedback(u64);

impl Dispatch2<pfb::WpPresentationFeedback, App> for Feedback {
    fn event(
        &self,
        app: &mut App,
        _: &pfb::WpPresentationFeedback,
        e: pfb::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        let Some(l) = app.latency.as_mut() else { return };
        match e {
            pfb::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                ..
            } => {
                let sec = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                l.presented(
                    self.0,
                    sec.saturating_mul(1_000_000_000).saturating_add(u64::from(tv_nsec)),
                    refresh,
                );
            }
            pfb::Event::Discarded => l.discarded(self.0),
            _ => {}
        }
    }
}

impl Dispatch2<tseat::ZwpTabletSeatV2, App> for Plain {
    fn event(&self, app: &mut App, _: &tseat::ZwpTabletSeatV2, e: tseat::Event, _: &Connection, qh: &QueueHandle<App>) {
        if let tseat::Event::ToolAdded { id } = e {
            let device = app.cursor_shapes.as_ref().map(|m| m.get_tablet_tool_v2(&id, qh, Plain));
            app.tools.push(ToolSlot {
                tool: id,
                tr: ToolTranslator::default(),
                cursor: CursorSlot {
                    device,
                    ..CursorSlot::default()
                },
            });
        }
    }
    fn event_created_child(
        opcode: u16,
        qh: &QueueHandle<App>,
    ) -> Arc<dyn sctk::reexports::client::backend::ObjectData> {
        match opcode {
            tseat::EVT_TOOL_ADDED_OPCODE => qh.make_data::<tool::ZwpTabletToolV2, Plain>(Plain),
            tseat::EVT_PAD_ADDED_OPCODE => qh.make_data::<pad::ZwpTabletPadV2, Plain>(Plain),
            _ => qh.make_data::<tab::ZwpTabletV2, Plain>(Plain),
        }
    }
}

impl Dispatch2<pad::ZwpTabletPadV2, App> for Plain {
    fn event(&self, _: &mut App, p: &pad::ZwpTabletPadV2, e: pad::Event, _: &Connection, _: &QueueHandle<App>) {
        if let pad::Event::Removed = e {
            p.destroy();
        }
    }
    fn event_created_child(_: u16, qh: &QueueHandle<App>) -> Arc<dyn sctk::reexports::client::backend::ObjectData> {
        qh.make_data::<pgroup::ZwpTabletPadGroupV2, Plain>(Plain)
    }
}

impl Dispatch2<pgroup::ZwpTabletPadGroupV2, App> for Plain {
    fn event(
        &self,
        _: &mut App,
        _: &pgroup::ZwpTabletPadGroupV2,
        _: pgroup::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
    fn event_created_child(
        opcode: u16,
        qh: &QueueHandle<App>,
    ) -> Arc<dyn sctk::reexports::client::backend::ObjectData> {
        match opcode {
            pgroup::EVT_STRIP_OPCODE => qh.make_data::<pstrip::ZwpTabletPadStripV2, Plain>(Plain),
            _ => qh.make_data::<pring::ZwpTabletPadRingV2, Plain>(Plain),
        }
    }
}

/// Protocol event → [`ToolEvent`] (None: not relevant for ink).
pub fn tool_event(e: &tool::Event) -> Option<ToolEvent> {
    Some(match e {
        tool::Event::Type { tool_type } => ToolEvent::Type(match tool_type {
            WEnum::Value(tool::Type::Eraser) => PenTool::Eraser,
            _ => PenTool::Pen,
        }),
        tool::Event::ProximityIn { .. } => ToolEvent::ProximityIn,
        tool::Event::ProximityOut => ToolEvent::ProximityOut,
        tool::Event::Down { .. } => ToolEvent::Down,
        tool::Event::Up => ToolEvent::Up,
        tool::Event::Motion { x, y } => ToolEvent::Motion { x: *x, y: *y },
        tool::Event::Pressure { pressure } => ToolEvent::Pressure(*pressure),
        tool::Event::Distance { distance } => ToolEvent::Distance(*distance),
        tool::Event::Tilt { tilt_x, tilt_y } => ToolEvent::Tilt { x: *tilt_x, y: *tilt_y },
        tool::Event::Button { button, state, .. } => ToolEvent::Button {
            button: *button,
            pressed: matches!(state, WEnum::Value(tool::ButtonState::Pressed)),
        },
        tool::Event::Frame { time } => ToolEvent::Frame { time_ms: *time },
        tool::Event::Removed => ToolEvent::Removed,
        _ => return None,
    })
}

impl Dispatch2<tool::ZwpTabletToolV2, App> for Plain {
    fn event(&self, app: &mut App, t: &tool::ZwpTabletToolV2, e: tool::Event, _: &Connection, qh: &QueueHandle<App>) {
        if let tool::Event::ProximityIn { surface, .. } = &e
            && surface != app.window.wl_surface()
        {
            return; // another surface of ours (none yet)
        }
        let Some(ev) = tool_event(&e) else { return };
        if let tool::Event::Down { serial } = &e {
            app.input_serial = *serial;
        }
        let Some(slot) = app.tools.iter_mut().find(|x| &x.tool == t) else {
            return;
        };
        match &e {
            tool::Event::ProximityIn { serial, .. } => slot.cursor.enter(*serial),
            tool::Event::ProximityOut => slot.cursor.serial = None,
            _ => {}
        }
        let out = slot.tr.feed(ev, mono_us());
        if ev == ToolEvent::Removed {
            app.tools.retain(|x| {
                let keep = &x.tool != t;
                if !keep && let Some(d) = &x.cursor.device {
                    d.destroy();
                }
                keep
            });
            t.destroy();
        }
        if !out.is_empty() {
            app.pen(out);
            app.request_draw(qh);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::full_line_rows;

    #[test]
    fn vim_selections_are_drawn_as_whole_rows() {
        // "b" on the first row, an empty line, "cd" on the third (two pieces of one row).
        let ranges = [
            [10.0, 0.0, 20.0, 20.0],
            [0.0, 20.0, 0.0, 40.0],
            [0.0, 40.0, 8.0, 60.0],
            [8.0, 40.0, 16.0, 60.0],
        ];
        let char = full_line_rows(&ranges, 0.0, 500.0, false, 6.0);
        assert_eq!(
            char,
            vec![
                [10.0, 0.0, 500.0, 20.0],
                [0.0, 20.0, 500.0, 40.0],
                [0.0, 40.0, 16.0, 60.0]
            ]
        );
        let line = full_line_rows(&ranges, 0.0, 500.0, true, 6.0);
        assert!(line.iter().all(|r| r[0] == 0.0 && r[2] == 500.0), "{line:?}");
        // A single empty row is still one cell wide.
        assert_eq!(
            full_line_rows(&ranges[1..2], 0.0, 500.0, false, 6.0),
            vec![[0.0, 20.0, 6.0, 40.0]]
        );
    }
}
