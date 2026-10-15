//! Workspace settings (M2.4): the selected tool and preferences that commands and the UI read.
//! Settings are not part of undo; they persist with the workspace. Missing fields take their
//! defaults and unknown fields are ignored, so older and newer files both load.

use crate::ModelError;
use serde::{Deserialize, Serialize};

/// Selectable tools (R-PEN-005). The pen's eraser end erases without changing this (R-PEN-006).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Pen,
    Highlighter,
    EraserStroke,
    EraserArea,
    Lasso,
    Shape,
    Text,
}

/// What the eraser end does (R-PEN-006): whole strokes (default) or the area under it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraserMode {
    #[default]
    Stroke,
    Area,
}

/// Prettify mode (R-HWR-006), default suggest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrettifyMode {
    Off,
    #[default]
    Suggest,
    Automatic,
}

/// What a held barrel button does (R-PEN-010, configurable; R-CANVAS-002 pen-barrel pan).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrelAction {
    /// Draw a lasso instead of ink.
    Lasso,
    /// Erase with the eraser mode.
    Eraser,
    /// Drag the view.
    Pan,
    /// Nothing: the pen inks as usual.
    None,
}

/// Note background pattern (R-CANVAS-006).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    #[default]
    Blank,
    Lined,
    Grid,
    Dots,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    pub kind: BackgroundKind,
    /// Line/grid/dot spacing in canvas units.
    pub spacing: f64,
}

impl Background {
    /// Checks the spacing range.
    pub fn validate(&self) -> Result<(), ModelError> {
        in_range("background.spacing", self.spacing, bounds::BACKGROUND_SPACING)
    }
}

impl Default for Background {
    fn default() -> Self {
        Background {
            kind: BackgroundKind::Blank,
            spacing: 32.0,
        }
    }
}

/// Bounds of the numeric settings.
pub mod bounds {
    /// Block idle threshold (R-HWR-001: 0.5–10 s).
    pub const BLOCK_IDLE_MS: (u32, u32) = (500, 10_000);
    /// Prediction horizon (ADR 0001 rule 5; 0 = off).
    pub const PREDICTION_MS: (f64, f64) = (0.0, 20.0);
    pub const BACKGROUND_SPACING: (f64, f64) = (4.0, 1_000.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub tool: ToolKind,
    pub eraser_mode: EraserMode,
    pub prettify_mode: PrettifyMode,
    /// A writing block is finished after this long without a touch-down in it (default 1.5 s).
    pub block_idle_ms: u32,
    /// Display-only ink prediction horizon (ADR 0001; 8 ms removed the visible lag at p50 [sim]).
    pub prediction_ms: f64,
    pub background: Background,
    /// Barrel button 1 (BTN_STYLUS) and 2 (BTN_STYLUS2) while held (R-PEN-010).
    pub barrel1: BarrelAction,
    pub barrel2: BarrelAction,
    /// What the shape tool draws (R-SHAPE-001).
    pub shape: crate::ShapeKind,
    /// Rough shapes snap to clean ones as soon as the pen lifts, without holding still
    /// (R-SHAPE-002 asks for the hold; this is the owner's opt-in, default off).
    pub snap_on_lift: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tool: ToolKind::Pen,
            eraser_mode: EraserMode::Stroke,
            prettify_mode: PrettifyMode::Suggest,
            block_idle_ms: 1_500,
            prediction_ms: 8.0,
            background: Background::default(),
            barrel1: BarrelAction::Lasso,
            barrel2: BarrelAction::Eraser,
            shape: crate::ShapeKind::Rectangle,
            snap_on_lift: false,
        }
    }
}

fn in_range<T: PartialOrd + Copy + std::fmt::Display>(name: &str, v: T, (lo, hi): (T, T)) -> Result<(), ModelError> {
    // `contains` is false for NaN, so non-finite floats are rejected too.
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(ModelError::Invalid(format!("{name} must be within {lo}..={hi}")))
    }
}

impl Settings {
    /// Checks every numeric setting against [`bounds`].
    pub fn validate(&self) -> Result<(), ModelError> {
        in_range("block_idle_ms", self.block_idle_ms, bounds::BLOCK_IDLE_MS)?;
        in_range("prediction_ms", self.prediction_ms, bounds::PREDICTION_MS)?;
        self.background.validate()
    }

    /// A copy with every out-of-range value replaced by its default (stored settings are
    /// hostile input; a bad value must not stop the workspace from opening).
    pub fn sanitised(mut self) -> (Settings, Vec<String>) {
        let d = Settings::default();
        let mut fixed = Vec::new();
        if in_range("", self.block_idle_ms, bounds::BLOCK_IDLE_MS).is_err() {
            self.block_idle_ms = d.block_idle_ms;
            fixed.push("block_idle_ms".to_string());
        }
        if in_range("", self.prediction_ms, bounds::PREDICTION_MS).is_err() {
            self.prediction_ms = d.prediction_ms;
            fixed.push("prediction_ms".to_string());
        }
        if in_range("", self.background.spacing, bounds::BACKGROUND_SPACING).is_err() {
            self.background.spacing = d.background.spacing;
            fixed.push("background.spacing".to_string());
        }
        (self, fixed)
    }
}
