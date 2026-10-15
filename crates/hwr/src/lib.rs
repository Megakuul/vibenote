//! Handwriting: recognition (ADR 0006), writing-vs-drawing (ADR 0007), prettify (ADR 0008).
//!
//! Layering is enforced by `cargo xtask layers` (AGENTS.md §2 Phase 3, item 1).

pub mod blocks;
pub mod ctc;
pub mod features;
pub mod pack;
pub mod prettify;
pub mod raster;
pub mod recognizer;
pub mod trees;
