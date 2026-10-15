//! Thin UI: Wayland client (ADR 0001), egui chrome (ADR 0002).
//!
//! Layering is enforced by `cargo xtask layers` (AGENTS.md §2 Phase 3, item 1).

pub mod app;
pub mod chrome;
pub mod clipboard;
pub mod cursor;
pub mod gpu_surface;
pub mod latency;
pub mod tablet;
pub mod theme;

pub use app::{Options, UiError, run};
