//! GPU renderer (ADR 0003): ink as instanced anti-aliased capsule segments.
//!
//! Committed ink is encoded once into a compact, lossless per-cell format (ADR 0011) and kept on
//! the GPU; each frame does bounded work ([`FrameBudget`]). The headless path serves golden
//! images and the CLI (`render-png`). LOD, the tile cache, PDF/image layers and text boxes
//! arrive with their milestones (ADR 0003/0004/0002).

mod encode;
mod image;
mod lod_worker;
mod pipeline;
mod scene;
mod text;
mod tiles;

pub use image::{Image, ImageDiff, compare};
pub use pipeline::{
    GpuFault, HEADLESS_FORMAT, Headless, ImageQuad, Renderer, Scene, Viewport, instance, request_device,
};
pub use scene::{FrameBudget, FrameStats};
pub use text::TextItem;
/// Re-exported so the UI uses the exact wgpu build the renderer was compiled against.
pub use wgpu;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("GPU device: {0}")]
    Device(String),
    #[error("viewport out of range: {0}")]
    Viewport(String),
    #[error("image: {0}")]
    Image(String),
    #[error("GPU readback: {0}")]
    Readback(String),
}

/// Paper colour (sRGB) used as the canvas background.
pub const PAPER: [u8; 4] = [251, 250, 246, 255];

/// The bundled fonts (Inter 4.1, OFL-1.1; ATTRIBUTION.md): canvas text and the app chrome.
pub mod fonts {
    pub static INTER: &[u8] = include_bytes!("../../../assets/fonts/InterVariable.ttf");
    pub static INTER_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/InterVariable-Italic.ttf");
}
