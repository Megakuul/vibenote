//! Workspace settings (M2.4): `settings.set` plus the per-setting commands named in SPEC.
//! Settings are not undoable; every change is validated as a whole and persisted with the next
//! storage batch. Tool selection (`tool.select.*`) arrives with the tools (M4.1).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::Deserialize;
use vibenote_model::Settings;
use vibenote_model::settings::{Background, BackgroundKind, BarrelAction, EraserMode, PrettifyMode, ToolKind};

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ToolP {
    Pen,
    Highlighter,
    EraserStroke,
    EraserArea,
    Lasso,
    Shape,
    Text,
    Cover,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum EraserP {
    Stroke,
    Area,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BarrelP {
    Lasso,
    Eraser,
    Pan,
    None,
}

fn barrel(b: BarrelP) -> BarrelAction {
    match b {
        BarrelP::Lasso => BarrelAction::Lasso,
        BarrelP::Eraser => BarrelAction::Eraser,
        BarrelP::Pan => BarrelAction::Pan,
        BarrelP::None => BarrelAction::None,
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PrettifyP {
    Off,
    Suggest,
    Automatic,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BackgroundKindP {
    Blank,
    Lined,
    Grid,
    Dots,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BackgroundP {
    kind: Option<BackgroundKindP>,
    /// Spacing in canvas units (4–1000).
    spacing: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteBackgroundP {
    /// Default: the current note.
    note: Option<u64>,
    kind: Option<BackgroundKindP>,
    /// Spacing in canvas units (4–1000).
    spacing: Option<f64>,
    /// Follow the workspace default (`settings.background`) again.
    #[serde(default)]
    default: bool,
}

/// Fields to change; omitted fields keep their value.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SettingsP {
    tool: Option<ToolP>,
    eraser_mode: Option<EraserP>,
    prettify_mode: Option<PrettifyP>,
    /// Writing-block idle threshold in ms (500–10000).
    block_idle_ms: Option<u32>,
    /// Ink prediction horizon in ms (0–20, 0 = off).
    prediction_ms: Option<f64>,
    background: Option<BackgroundP>,
    /// Barrel button 1 / 2 while held: lasso, eraser, pan or none (R-PEN-010).
    barrel1: Option<BarrelP>,
    barrel2: Option<BarrelP>,
    /// What the shape tool draws (R-SHAPE-001).
    shape: Option<ShapeP>,
    /// Snap rough shapes when the pen lifts instead of after holding still (R-SHAPE-002).
    snap_on_lift: Option<bool>,
    /// Vim mode for text boxes (R-VIM-001).
    vim: Option<bool>,
    /// Colours of new pen / highlighter strokes, 0xRRGGBBAA (R-PEN-013).
    pen_color: Option<u32>,
    highlighter_color: Option<u32>,
    /// Rest time (ms) at the end of a stroke that snaps it into a shape: 200–3000, 0 = never
    /// (R-SHAPE-002).
    snap_hold_ms: Option<u32>,
    /// How shapes look: `clean` or `hand_drawn` (R-SHAPE-003).
    shape_style: Option<ShapeStyleP>,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ShapeStyleP {
    Clean,
    HandDrawn,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ShapeP {
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Triangle,
    Polygon,
}

fn shape(p: ShapeP) -> vibenote_model::ShapeKind {
    use vibenote_model::ShapeKind as K;
    match p {
        ShapeP::Line => K::Line,
        ShapeP::Arrow => K::Arrow,
        ShapeP::Rectangle => K::Rectangle,
        ShapeP::Ellipse => K::Ellipse,
        ShapeP::Triangle => K::Triangle,
        ShapeP::Polygon => K::Polygon,
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ModeP {
    mode: PrettifyP,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdleP {
    ms: u32,
}

fn tool(t: ToolP) -> ToolKind {
    match t {
        ToolP::Pen => ToolKind::Pen,
        ToolP::Highlighter => ToolKind::Highlighter,
        ToolP::EraserStroke => ToolKind::EraserStroke,
        ToolP::EraserArea => ToolKind::EraserArea,
        ToolP::Lasso => ToolKind::Lasso,
        ToolP::Shape => ToolKind::Shape,
        ToolP::Text => ToolKind::Text,
        ToolP::Cover => ToolKind::Cover,
    }
}

fn prettify(m: PrettifyP) -> PrettifyMode {
    match m {
        PrettifyP::Off => PrettifyMode::Off,
        PrettifyP::Suggest => PrettifyMode::Suggest,
        PrettifyP::Automatic => PrettifyMode::Automatic,
    }
}

fn background(cur: Background, p: BackgroundP) -> Background {
    Background {
        kind: p.kind.map_or(cur.kind, |k| match k {
            BackgroundKindP::Blank => BackgroundKind::Blank,
            BackgroundKindP::Lined => BackgroundKind::Lined,
            BackgroundKindP::Grid => BackgroundKind::Grid,
            BackgroundKindP::Dots => BackgroundKind::Dots,
        }),
        spacing: p.spacing.unwrap_or(cur.spacing),
    }
}

/// Replaces the settings with `next` if valid; the session is unchanged on error.
pub(crate) fn update(s: &mut Session, next: Settings) -> Result<Settings, EngineError> {
    next.validate()?;
    if next.tool != s.settings.tool && s.text_edit.is_some() {
        super::text::end(s)?; // leaving the text tool ends typing
    }
    if next.tool != s.settings.tool || next.shape != s.settings.shape {
        s.live.polygon.clear(); // a polygon in progress belongs to the shape tool
    }
    if next != s.settings {
        s.settings = next;
        s.settings_dirty = true;
    }
    Ok(s.settings)
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "settings.set",
            doc: "Change workspace settings (tool, eraser_mode, prettify_mode, block_idle_ms, prediction_ms, background, barrel1, barrel2, shape, snap_on_lift, vim, pen_color, highlighter_color, snap_hold_ms, shape_style); returns all settings.",
            reqs: &["R-PEN-006", "R-PEN-010", "R-HWR-001", "R-HWR-006", "R-CANVAS-006", "R-VIM-001", "R-PEN-013", "R-SHAPE-002", "R-SHAPE-003"],
            implemented: true,
        },
        |s: &mut Session, p: SettingsP| {
            let c = s.settings;
            let next = Settings {
                tool: p.tool.map_or(c.tool, tool),
                eraser_mode: p.eraser_mode.map_or(c.eraser_mode, |m| match m {
                    EraserP::Stroke => EraserMode::Stroke,
                    EraserP::Area => EraserMode::Area,
                }),
                prettify_mode: p.prettify_mode.map_or(c.prettify_mode, prettify),
                block_idle_ms: p.block_idle_ms.unwrap_or(c.block_idle_ms),
                prediction_ms: p.prediction_ms.unwrap_or(c.prediction_ms),
                background: p.background.map_or(c.background, |b| background(c.background, b)),
                barrel1: p.barrel1.map_or(c.barrel1, barrel),
                barrel2: p.barrel2.map_or(c.barrel2, barrel),
                shape: p.shape.map_or(c.shape, shape),
                snap_on_lift: p.snap_on_lift.unwrap_or(c.snap_on_lift),
                vim: p.vim.unwrap_or(c.vim),
                pen_color: p.pen_color.map_or(c.pen_color, vibenote_model::Color),
                highlighter_color: p.highlighter_color.map_or(c.highlighter_color, vibenote_model::Color),
                snap_hold_ms: p.snap_hold_ms.unwrap_or(c.snap_hold_ms),
                shape_style: p.shape_style.map_or(c.shape_style, |st| match st {
                    ShapeStyleP::Clean => vibenote_model::settings::ShapeStyle::Clean,
                    ShapeStyleP::HandDrawn => vibenote_model::settings::ShapeStyle::HandDrawn,
                }),
            };
            update(s, next)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "canvas.set_background",
            doc: "Set a note's background (blank/lined/grid/dots, spacing 4–1000 canvas units; omitted fields keep the note's current background; `default: true` follows the workspace default again). One undo step in the note.",
            reqs: &["R-CANVAS-006"],
            implemented: true,
        },
        |s: &mut Session, p: NoteBackgroundP| {
            let id = super::ink::target_note(s, p.note)?;
            let cur = s.ws.note(id)?.background;
            let next = if p.default {
                if p.kind.is_some() || p.spacing.is_some() {
                    return Err(EngineError::Model("`default` takes no kind or spacing".into()));
                }
                None
            } else if p.kind.is_none() && p.spacing.is_none() {
                cur // nothing to change
            } else {
                let bg = background(
                    s.background_of(id),
                    BackgroundP {
                        kind: p.kind,
                        spacing: p.spacing,
                    },
                );
                bg.validate()?;
                Some(bg)
            };
            if next != cur {
                s.commit(
                    crate::Scope::Note(id),
                    "background",
                    vec![vibenote_model::Op::SetNoteBackground {
                        note: id,
                        background: next,
                    }],
                )?;
            }
            Ok(s.background_of(id))
        },
    )))?;
    for (id, doc, reqs, t) in [
        ("tool.select.pen", "Select the pen.", &["R-PEN-005"][..], ToolKind::Pen),
        (
            "tool.select.highlighter",
            "Select the highlighter (translucent, below pen ink, widens with tilt).",
            &["R-PEN-005", "R-PEN-003"],
            ToolKind::Highlighter,
        ),
        (
            "tool.select.eraser_stroke",
            "Select the stroke eraser.",
            &["R-PEN-005", "R-PEN-007"],
            ToolKind::EraserStroke,
        ),
        (
            "tool.select.eraser_area",
            "Select the area eraser.",
            &["R-PEN-005", "R-PEN-008"],
            ToolKind::EraserArea,
        ),
        (
            "tool.select.lasso",
            "Select the lasso.",
            &["R-PEN-005", "R-PEN-009"],
            ToolKind::Lasso,
        ),
        (
            "tool.select.shape",
            "Select the shape tool.",
            &["R-PEN-005", "R-SHAPE-001"],
            ToolKind::Shape,
        ),
        (
            "tool.select.text",
            "Select the text tool.",
            &["R-PEN-005", "R-TEXT-001"],
            ToolKind::Text,
        ),
        (
            "tool.select.cover",
            "Select the cover tool: opaque strokes that hide what is under them until tapped (flashcards).",
            &["R-PEN-005", "R-PEN-014"],
            ToolKind::Cover,
        ),
    ] {
        r.register(Box::new(Typed::new(
            Spec {
                id,
                doc,
                reqs,
                implemented: true,
            },
            move |s: &mut Session, _: NoParams| update(s, Settings { tool: t, ..s.settings }),
        )))?;
    }
    r.register(Box::new(Typed::new(
        Spec {
            id: "tool.select.eraser",
            doc: "Select the eraser in the current eraser mode; when an eraser is already selected, switch between stroke and area (the eraser end follows).",
            reqs: &["R-PEN-006", "R-PEN-007", "R-PEN-008", "R-CMD-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| {
            let mode = match s.settings.tool {
                ToolKind::EraserStroke => EraserMode::Area,
                ToolKind::EraserArea => EraserMode::Stroke,
                _ => s.settings.eraser_mode,
            };
            let tool = match mode {
                EraserMode::Stroke => ToolKind::EraserStroke,
                EraserMode::Area => ToolKind::EraserArea,
            };
            update(
                s,
                Settings {
                    tool,
                    eraser_mode: mode,
                    ..s.settings
                },
            )
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.set_mode",
            doc: "Set prettify mode: off / suggest / automatic (never changes existing ink).",
            reqs: &["R-HWR-006"],
            implemented: true,
        },
        |s: &mut Session, p: ModeP| {
            update(
                s,
                Settings {
                    prettify_mode: prettify(p.mode),
                    ..s.settings
                },
            )
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "settings.set_block_idle",
            doc: "Set the writing-block idle threshold in ms (500–10000, default 1500).",
            reqs: &["R-HWR-001"],
            implemented: true,
        },
        |s: &mut Session, p: IdleP| {
            update(
                s,
                Settings {
                    block_idle_ms: p.ms,
                    ..s.settings
                },
            )
        },
    )))?;
    Ok(())
}
