//! Ink rendering: instanced capsule SDF segments (ADR 0003/0011). [`Renderer`] encodes a frame
//! into any colour target (the app's window surface); [`Headless`] renders offscreen with
//! readback. Committed ink lives on the GPU in per-cell chunks (`scene.rs`).

use crate::{Image, PAPER, RenderError};
use bytemuck::{Pod, Zeroable};
use vibenote_geom::{Point, Rect};
use vibenote_ink::tool_width;
use vibenote_model::{Canvas, ObjectKind, Stroke, Tool};

/// Which part of the canvas to render and at what size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Canvas coordinate at the top-left pixel corner.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Pixels per canvas unit.
    pub zoom: f64,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn check(&self) -> Result<(), RenderError> {
        let ok = self.origin_x.is_finite()
            && self.origin_y.is_finite()
            && self.zoom.is_finite()
            && (0.001..=1000.0).contains(&self.zoom)
            && (1..=8192).contains(&self.width)
            && (1..=8192).contains(&self.height);
        if ok {
            Ok(())
        } else {
            Err(RenderError::Viewport(format!("{self:?}")))
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Seg {
    /// Endpoints in pixels relative to the viewport's top-left (computed in f64 on the CPU).
    p0: [f32; 2],
    p1: [f32; 2],
    /// Radii in pixels.
    r: [f32; 2],
    color: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    size: [f32; 2],
    min_r: f32,
    _pad: f32,
}

const SHADER: &str = r#"
struct U { size: vec2<f32>, min_r: f32, _pad: f32 };
@group(0) @binding(0) var<uniform> u: U;
struct VO {
  @builtin(position) pos: vec4<f32>,
  @location(0) p: vec2<f32>,
  @location(1) a: vec2<f32>,
  @location(2) b: vec2<f32>,
  @location(3) r: vec2<f32>,
  @location(4) color: vec4<f32>,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) p0: vec2<f32>, @location(1) p1: vec2<f32>,
              @location(2) rr: vec2<f32>, @location(3) col: u32) -> VO {
  let r = max(rr, vec2<f32>(u.min_r));
  let rm = max(r.x, r.y) + 1.0;
  let d = p1 - p0;
  let l = length(d);
  var dir = vec2<f32>(1.0, 0.0);
  if (l > 1e-4) { dir = d / l; }
  let nrm = vec2<f32>(-dir.y, dir.x);
  var cx = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
  var cy = array<f32, 6>(-1.0, -1.0, 1.0, 1.0, -1.0, 1.0);
  let along = mix(p0 - dir * rm, p1 + dir * rm, cx[vi]);
  let pp = along + nrm * rm * cy[vi];
  var o: VO;
  o.pos = vec4<f32>(pp.x / u.size.x * 2.0 - 1.0, 1.0 - pp.y / u.size.y * 2.0, 0.0, 1.0);
  o.p = pp; o.a = p0; o.b = p1; o.r = r;
  // colour is 0xRRGGBBAA
  o.color = vec4<f32>(f32((col >> 24u) & 255u), f32((col >> 16u) & 255u), f32((col >> 8u) & 255u), f32(col & 255u)) / 255.0;
  return o;
}
"#;

/// Upper bound on segments per render (hostile input / memory bound).
pub const MAX_SEGMENTS: usize = 50_000_000;
/// Offscreen target format of [`Headless`] (goldens are RGBA8, non-sRGB).
pub const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Requests a Vulkan adapter + device. `surface`: the window surface it must present to (None
/// for headless). `VIBENOTE_ADAPTER=cpu` selects a software (CPU) adapter.
pub fn request_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), RenderError> {
    let want_cpu = std::env::var("VIBENOTE_ADAPTER").is_ok_and(|v| v == "cpu");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        force_fallback_adapter: want_cpu,
        apply_limit_buckets: false,
    }))
    .map_err(|e| RenderError::NoAdapter(e.to_string()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|e| RenderError::Device(e.to_string()))?;
    Ok((adapter, device, queue))
}

/// The Vulkan-only instance used by the app and the headless renderer (ADR 0001/0003).
pub fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// GPU errors reported by wgpu callbacks. wgpu's default uncaptured-error handler panics; we
/// record the error instead (never crash, AGENTS.md §3) and let the owner of the renderer
/// recover (recreate the device or fall back).
#[derive(Default)]
struct Faults {
    lost: std::sync::atomic::AtomicBool,
    error: std::sync::Mutex<Option<String>>,
}

/// What went wrong with the GPU since the last check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuFault {
    /// The device is gone (driver reset, suspend); recreate everything.
    Lost(String),
    /// A captured wgpu error (validation, out of memory); the frame may be incomplete.
    Error(String),
}

/// What to draw in one frame.
pub struct Scene<'a> {
    pub canvas: &'a Canvas,
    /// The canvas's change journal, if its owner keeps one (the engine does).
    pub changes: Option<&'a vibenote_model::ChangeLog>,
    pub live: &'a [&'a Stroke],
    pub viewport: Viewport,
}

/// Shared fragment stage: anti-aliased capsule SDF with interpolated radius (unchanged since M1).
const FRAGMENT: &str = r#"
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
  let ab = i.b - i.a;
  let l2 = dot(ab, ab);
  var t = 0.0;
  if (l2 > 1e-8) { t = clamp(dot(i.p - i.a, ab) / l2, 0.0, 1.0); }
  let q = i.a + ab * t;
  let d = length(i.p - q);
  let r = mix(i.r.x, i.r.y, t);
  let cov = clamp(r - d + 0.5, 0.0, 1.0) * i.color.a;
  if (cov <= 0.0) { discard; }
  return vec4<f32>(i.color.rgb * cov, cov);
}
"#;

/// Chunk vertex stage (ADR 0011): instance `i` is the segment from point `i - 1` to point `i`
/// of a batch; points are i16 offsets (1/1024 unit) from f32 chunk-local run origins.
const CHUNK_SHADER: &str = r#"
struct D { off: vec2<f32>, size: vec2<f32>, zoom: f32, min_r: f32, flags: u32, level: u32 };
@group(0) @binding(0) var<uniform> d: D;
@group(1) @binding(0) var<storage, read> pts: array<vec2<u32>>;
@group(1) @binding(1) var<storage, read> runs: array<vec4<u32>>;
@group(1) @binding(2) var<storage, read> strokes: array<vec4<u32>>;
@group(1) @binding(3) var<storage, read> lod3: array<u32>;
@group(1) @binding(4) var<storage, read> lod4: array<u32>;
@group(1) @binding(5) var<storage, read> lod5: array<u32>;
@group(1) @binding(6) var<storage, read> lod6: array<u32>;
// LOD mode (flags bit 1): instance i is the i-th kept point of level `level` (M3.2).
fn lod_at(i: u32) -> u32 {
  switch d.level {
    case 0u: { return lod3[i]; }
    case 1u: { return lod4[i]; }
    case 2u: { return lod5[i]; }
    default: { return lod6[i]; }
  }
}
struct VO {
  @builtin(position) pos: vec4<f32>,
  @location(0) p: vec2<f32>,
  @location(1) a: vec2<f32>,
  @location(2) b: vec2<f32>,
  @location(3) r: vec2<f32>,
  @location(4) color: vec4<f32>,
};
fn point_px(q: vec2<u32>) -> vec2<f32> {
  let run = runs[q.y & 0xffffu];
  let dx = f32(bitcast<i32>(q.x << 16u) >> 16u);
  let dy = f32(bitcast<i32>(q.x) >> 16u);
  let local = vec2<f32>(bitcast<f32>(run.x), bitcast<f32>(run.y)) + vec2<f32>(dx, dy) / 1024.0;
  return d.off + local * d.zoom;
}
@vertex fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VO {
  var o: VO;
  let lodm = (d.flags & 2u) != 0u;
  var k = ii;
  if (lodm) { k = lod_at(ii); }
  let q = pts[k];
  let fl = q.y >> 24u;
  let st = strokes[runs[q.y & 0xffffu].z];
  // CONT (predecessor only), dead strokes, and seam copies when drawing unscissored: no quad.
  if ((fl & 2u) != 0u || (st.z & 1u) != 0u || ((d.flags & 1u) != 0u && (fl & 4u) != 0u)) {
    o.pos = vec4<f32>(-2.0, -2.0, 0.0, 1.0);
    return o;
  }
  var kp = k;
  if ((fl & 1u) == 0u) {
    if (lodm) { kp = lod_at(ii - 1u); } else { kp = k - 1u; }
  }
  let qa = pts[kp];
  let p0 = point_px(qa);
  let p1 = point_px(q);
  let rmax = bitcast<f32>(st.y) * d.zoom;
  let r = max(vec2<f32>(rmax * f32((qa.y >> 16u) & 255u) / 255.0, rmax * f32((q.y >> 16u) & 255u) / 255.0), vec2<f32>(d.min_r));
  let rm = max(r.x, r.y) + 1.0;
  let dd = p1 - p0;
  let l = length(dd);
  var dir = vec2<f32>(1.0, 0.0);
  if (l > 1e-4) { dir = dd / l; }
  let nrm = vec2<f32>(-dir.y, dir.x);
  // one quad as a 4-vertex triangle strip (a third fewer vertex invocations than a list)
  var cx = array<f32, 4>(0.0, 1.0, 0.0, 1.0);
  var cy = array<f32, 4>(-1.0, -1.0, 1.0, 1.0);
  let along = mix(p0 - dir * rm, p1 + dir * rm, cx[vi]);
  let pp = along + nrm * rm * cy[vi];
  o.pos = vec4<f32>(pp.x / d.size.x * 2.0 - 1.0, 1.0 - pp.y / d.size.y * 2.0, 0.0, 1.0);
  o.p = pp; o.a = p0; o.b = p1; o.r = r;
  let col = st.x;
  o.color = vec4<f32>(f32((col >> 24u) & 255u), f32((col >> 16u) & 255u), f32((col >> 8u) & 255u), f32(col & 255u)) / 255.0;
  return o;
}
"#;

/// Highlighter layer composite: a full-screen triangle copying the (premultiplied) layer.
const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var layer: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let x = f32(i32(i & 1u) * 4 - 1);
  let y = f32(i32(i >> 1u) * 4 - 1);
  return vec4<f32>(x, y, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
  return textureLoad(layer, vec2<i32>(p.xy), 0);
}
"#;

/// Cached tiles composited onto the frame (M3.3): one textured quad per tile, sampled with
/// linear filtering (tiles are shown at scale 0.5–1), clamped inside the tile's atlas slot.
const TILE_SHADER: &str = r#"
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;
struct TI {
  @location(0) rect: vec4<f32>,
  @location(1) uv: vec4<f32>,
  @location(2) lim: vec4<f32>,
  @location(3) size: vec2<f32>,
  @location(4) layer: u32,
  @location(5) alpha: f32,
};
struct VO {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) lim: vec4<f32>,
  @location(2) @interpolate(flat) layer: u32,
  @location(3) alpha: f32,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, t: TI) -> VO {
  let c = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
  let p = mix(t.rect.xy, t.rect.zw, c);
  var o: VO;
  o.pos = vec4<f32>(p.x / t.size.x * 2.0 - 1.0, 1.0 - p.y / t.size.y * 2.0, 0.0, 1.0);
  o.uv = mix(t.uv.xy, t.uv.zw, c);
  o.lim = t.lim; o.layer = t.layer; o.alpha = t.alpha;
  return o;
}
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
  let uv = clamp(i.uv, i.lim.xy, i.lim.zw);
  return textureSampleLevel(atlas, samp, uv, i.layer, 0.0) * i.alpha;
}
"#;

/// Clears one tile slot (drawn with the slot as viewport and scissor; no blending).
const CLEAR_SHADER: &str = r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let x = f32(i32(i & 1u) * 4 - 1);
  let y = f32(i32(i >> 1u) * 4 - 1);
  return vec4<f32>(x, y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }
"#;

/// Note background (R-CANVAS-006): a full-screen pass over the paper with lines, a grid or dots
/// at the note's spacing. Lines are 1 px hairlines snapped to pixel centres; the pattern's
/// phase is computed in f64 on the CPU so far origins render exactly like the origin.
const BG_SHADER: &str = r#"
struct B { phase: vec2<f32>, spacing: f32, kind: u32, color: vec4<f32>, dot_r: f32, line_w: f32, _p0: f32, _p1: f32 };
@group(0) @binding(0) var<uniform> b: B;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let x = f32(i32(i & 1u) * 4 - 1);
  let y = f32(i32(i >> 1u) * 4 - 1);
  return vec4<f32>(x, y, 0.0, 1.0);
}
fn nearest(v: f32, phase: f32) -> f32 {
  return floor(phase + round((v - phase) / b.spacing) * b.spacing) + 0.5;
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
  let dx = abs(p.x - nearest(p.x, b.phase.x));
  let dy = abs(p.y - nearest(p.y, b.phase.y));
  let half = b.line_w * 0.5 + 0.5;
  var cov = 0.0;
  if (b.kind == 1u) {
    cov = clamp(half - dy, 0.0, 1.0);
  } else if (b.kind == 2u) {
    cov = max(clamp(half - dx, 0.0, 1.0), clamp(half - dy, 0.0, 1.0));
  } else if (b.kind == 3u) {
    cov = clamp(b.dot_r + 0.5 - length(vec2<f32>(dx, dy)), 0.0, 1.0);
  }
  let a = b.color.a * cov;
  if (a <= 0.0) { discard; }
  return vec4<f32>(b.color.rgb * a, a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BgU {
    phase: [f32; 2],
    spacing: f32,
    kind: u32,
    color: [f32; 4],
    dot_r: f32,
    line_w: f32,
    _pad: [f32; 2],
}

/// An image drawn under the ink (PDF pages): RGBA8 premultiplied pixels shown in a canvas
/// rectangle. `key` identifies the pixels (same key = same pixels) for the texture cache.
#[derive(Clone)]
pub struct ImageQuad {
    pub key: u64,
    pub rect: vibenote_geom::Rect,
    pub width: u32,
    pub height: u32,
    pub pixels: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl std::fmt::Debug for ImageQuad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageQuad({} {}×{} at {:?})",
            self.key, self.width, self.height, self.rect
        )
    }
}

struct ImageTex {
    bind: wgpu::BindGroup,
    bytes: usize,
    used: u64,
}

/// GPU memory for image textures before unused ones are dropped.
const TEXTURE_CAP: usize = 768 << 20;

/// Pattern colour (sRGB bytes) and opacity.
const BG_COLOR: [u8; 3] = [143, 166, 200];
const BG_ALPHA: f32 = 0.6;
/// On-screen spacing (px) below which a pattern fades out (it would turn into noise).
const BG_FADE_PX: (f64, f64) = (4.0, 12.0);

/// Uniforms for the background at `vp`, or `None` when nothing is drawn (blank, faded out,
/// or hostile values).
fn background_uniforms(bg: &vibenote_model::settings::Background, vp: &Viewport) -> Option<BgU> {
    use vibenote_model::settings::BackgroundKind as K;
    let kind = match bg.kind {
        K::Blank => return None,
        K::Lined => 1,
        K::Grid => 2,
        K::Dots => 3,
    };
    bg.validate().ok()?;
    let sp = bg.spacing * vp.zoom;
    let fade = ((sp - BG_FADE_PX.0) / (BG_FADE_PX.1 - BG_FADE_PX.0)).clamp(0.0, 1.0);
    if !(fade > 0.0 && sp.is_finite() && sp <= 1.0e6) {
        return None;
    }
    let phase = |o: f64| (-o).rem_euclid(bg.spacing) * vp.zoom;
    let c = BG_COLOR.map(|v| f32::from(v) / 255.0);
    Some(BgU {
        phase: [px(phase(vp.origin_x)), px(phase(vp.origin_y))],
        spacing: px(sp),
        kind,
        color: [c[0], c[1], c[2], BG_ALPHA * px(fade * fade * (3.0 - 2.0 * fade))],
        dot_r: px((sp * 0.04).clamp(0.9, 2.2)),
        line_w: 1.0,
        _pad: [0.0; 2],
    })
}

/// One tile on screen (instance data of the tile composite).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileInst {
    rect: [f32; 4],
    uv: [f32; 4],
    lim: [f32; 4],
    size: [f32; 2],
    layer: u32,
    alpha: f32,
}

/// Zoomed out beyond this, or with more visible instances than this, the frame is tiled.
/// ADR 0003 names 500k instances (from the spike's renderer); measured on this renderer, 341k
/// direct instances already took up to 5.7 ms, so the switch happens at 250k.
const TILE_ZOOM: f64 = 1.0;
const TILE_INSTANCES: u64 = 250_000;
/// Instances drawn into tiles per frame (at least one tile is always filled).
const FILL_INSTANCES: u64 = 100_000;

/// The cached committed frame: what it shows (viewport, format) and the texture.
struct Base {
    vp: Viewport,
    format: wgpu::TextureFormat,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

/// The tile atlas: one 2D array texture, a render view per layer, a sampling bind group.
struct Atlas {
    layers: Vec<wgpu::TextureView>,
    bind: wgpu::BindGroup,
}

/// Format of the offscreen highlighter layer.
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Below this zoom the cell margin no longer covers the minimum radius: draw unscissored.
const SCISSOR_MIN_ZOOM: f64 = 0.85 / crate::encode::MARGIN;
/// Uniform slot stride (default `min_uniform_buffer_offset_alignment`).
const SLOT: u64 = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Per-segment path (live strokes, reference renderer), premultiplied "over".
    Seg,
    /// Per-segment path into the highlighter layer (max blending).
    SegMax,
    Chunk,
    ChunkMax,
    Composite,
    /// Cached tiles onto the frame.
    Tile,
    /// Empties a tile slot.
    Clear,
    /// Copies the cached committed frame (no blending).
    Blit,
    /// Note background pattern over the paper.
    Background,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawU {
    off: [f32; 2],
    size: [f32; 2],
    zoom: f32,
    min_r: f32,
    /// bit 0: unscissored (skip seam copies); bit 1: LOD mode.
    flags: u32,
    /// LOD level index into `LOD_LEVELS`.
    level: u32,
}

/// Canvas renderer shared by the window (ADR 0001) and the headless harness: encodes one frame
/// into any colour target. Committed ink stays on the GPU between frames (ADR 0003/0011,
/// `scene.rs`); live strokes take the per-segment path every frame.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    seg_module: wgpu::ShaderModule,
    seg_layout: wgpu::PipelineLayout,
    seg_bgl: wgpu::BindGroupLayout,
    chunk_module: wgpu::ShaderModule,
    chunk_layout: wgpu::PipelineLayout,
    chunk_bgl0: wgpu::BindGroupLayout,
    chunk_bgl1: wgpu::BindGroupLayout,
    comp_module: wgpu::ShaderModule,
    comp_layout: wgpu::PipelineLayout,
    comp_bgl: wgpu::BindGroupLayout,
    tile_module: wgpu::ShaderModule,
    tile_layout: wgpu::PipelineLayout,
    tile_bgl: wgpu::BindGroupLayout,
    clear_module: wgpu::ShaderModule,
    clear_layout: wgpu::PipelineLayout,
    bg_module: wgpu::ShaderModule,
    bg_layout: wgpu::PipelineLayout,
    bg_bgl: wgpu::BindGroupLayout,
    /// Background of the canvas being drawn (R-CANVAS-006).
    background: vibenote_model::settings::Background,
    /// Images under the ink (PDF pages), in draw order, and a signature of the set.
    images: Vec<ImageQuad>,
    images_sig: u64,
    images_dirty: bool,
    /// Uploaded image textures by key, with their size and the frame they were last drawn.
    textures: std::collections::HashMap<u64, ImageTex>,
    texture_bytes: usize,
    frames: u64,
    /// Text boxes and link labels, and vector strokes (shapes), drawn above images and below
    /// ink; signatures mark the cached frame stale when they change.
    texts: Vec<crate::TextItem>,
    vectors: Vec<Stroke>,
    extras_sig: u64,
    text: crate::text::TextState,
    tiles: crate::tiles::TileCache,
    atlas: Option<Atlas>,
    /// Committed ink of the last frame (everything but live strokes), redrawn only when it
    /// changes: pen frames then cost a copy plus the live stroke.
    base: Option<Base>,
    pipelines: Vec<((wgpu::TextureFormat, Kind), wgpu::RenderPipeline)>,
    layer: Option<(u32, u32, wgpu::TextureView, wgpu::BindGroup)>,
    state: crate::scene::State,
    budget: crate::scene::FrameBudget,
    gpu_cap: u64,
    /// Computes LOD lists off this thread (`None`: the thread could not start; computed inline).
    lod: Option<crate::lod_worker::LodWorker>,
    pub adapter: String,
    faults: std::sync::Arc<Faults>,
}

fn uniform_entry(binding: u32, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Where a draw goes: the frame, or tile fill number `n` of this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    Frame,
    Fill(usize),
}

/// One draw of a batch (or part of it): uniform slot, scissor, which batch, instance range.
struct DrawItem {
    slot: u32,
    scissor: Option<[u32; 4]>,
    cell: crate::encode::Cell,
    hl: bool,
    batch: usize,
    instances: std::ops::Range<u32>,
    dest: Dest,
}

/// What a set of chunk draws looks at: canvas origin, zoom, size in px, and where its pixels
/// start in the render target (tile slots sit inside the atlas).
#[derive(Clone, Copy)]
struct Cam {
    x: f64,
    y: f64,
    zoom: f64,
    w: u32,
    h: u32,
    off: (u32, u32),
}

/// A tile rendered this frame: atlas layer and slot position.
struct Fill {
    layer: u32,
    x: u32,
    y: u32,
}

/// Per-frame draw lists and their uniform slots.
#[derive(Default)]
struct Frame {
    draws: Vec<DrawItem>,
    uniforms: Vec<u8>,
    instances: u64,
    has_hl: bool,
}

impl Frame {
    /// Draws of `cells` seen through `cam` (LOD by zoom; cells scissored to themselves when
    /// zoomed in enough, design §3.1).
    fn chunks(
        &mut self,
        state: &crate::scene::State,
        cells: &[crate::encode::Cell],
        cam: &Cam,
        dest: Dest,
    ) -> Result<(), RenderError> {
        let dim = |v: u32| f32::from(u16::try_from(v).unwrap_or(u16::MAX));
        let scissored = cam.zoom >= SCISSOR_MIN_ZOOM;
        let level = lod_level(cam.zoom);
        let slot_size = usize::try_from(SLOT).unwrap_or(256);
        let full = [cam.off.0, cam.off.1, cam.w, cam.h];
        for cell in cells {
            let Some(c) = state.chunks.get(cell) else { continue };
            let scissor = if scissored {
                match cell_scissor(*cell, cam) {
                    Some(s) => s,
                    None => continue,
                }
            } else {
                full
            };
            let (ox, oy) = cell.origin();
            // Offset computed in f64: chunk-local f32 stays exact far from the origin.
            let base = DrawU {
                off: [px((ox - cam.x) * cam.zoom), px((oy - cam.y) * cam.zoom)],
                size: [dim(cam.w), dim(cam.h)],
                zoom: px(cam.zoom),
                min_r: 0.35,
                flags: u32::from(!scissored),
                level: 0,
            };
            for (hl, list) in [(false, &c.pen), (true, &c.hl)] {
                for (bi, b) in list.iter().enumerate() {
                    if b.points_gpu == 0 {
                        continue;
                    }
                    self.has_hl |= hl && dest == Dest::Frame;
                    // LOD for the prefix whose lists are on the GPU (and whose points are), full
                    // detail for the rest.
                    let lod = level.filter(|_| b.lod_gpu_covered > 0 && b.lod_gpu_covered <= b.points_gpu);
                    let mut parts: Vec<(DrawU, std::ops::Range<u32>)> = Vec::new();
                    let mut rest = 0..b.points_gpu;
                    if let Some(li) = lod {
                        let n = b.lod_gpu.get(usize::try_from(li).unwrap_or(0)).copied().unwrap_or(0);
                        parts.push((
                            DrawU {
                                flags: base.flags | 2,
                                level: li,
                                ..base
                            },
                            0..n,
                        ));
                        rest = b.lod_gpu_covered..b.points_gpu;
                    }
                    if !rest.is_empty() {
                        parts.push((base, rest));
                    }
                    for (u, range) in parts {
                        let slot = u32::try_from(self.draws.len())
                            .map_err(|_| RenderError::Viewport("too many draws".into()))?;
                        self.uniforms.extend_from_slice(bytemuck::bytes_of(&u));
                        self.uniforms.resize(
                            self.uniforms
                                .len()
                                .saturating_add(slot_size.saturating_sub(std::mem::size_of::<DrawU>())),
                            0,
                        );
                        self.instances = self
                            .instances
                            .saturating_add(u64::from(range.end.saturating_sub(range.start)));
                        self.draws.push(DrawItem {
                            slot,
                            scissor: Some(scissor),
                            cell: *cell,
                            hl,
                            batch: bi,
                            instances: range,
                            dest,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Coarsest LOD level whose tolerance stays within one pixel at `zoom` (index into
/// `LOD_LEVELS`), or `None` for full detail.
fn lod_level(zoom: f64) -> Option<u32> {
    let levels = vibenote_ink::lod::LOD_LEVELS;
    (0..levels.len())
        .rev()
        .find(|&i| {
            levels
                .get(i)
                .is_some_and(|&l| vibenote_ink::lod::tolerance(l) * zoom <= 1.0)
        })
        .and_then(|i| u32::try_from(i).ok())
}

impl Renderer {
    pub fn new(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let info = adapter.get_info();
        let faults = std::sync::Arc::new(Faults::default());
        let f = std::sync::Arc::clone(&faults);
        device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| {
            let mut g = f.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            g.get_or_insert_with(|| e.to_string());
        }));
        let f = std::sync::Arc::clone(&faults);
        device.set_device_lost_callback(move |reason, msg| {
            f.lost.store(true, std::sync::atomic::Ordering::Release);
            let mut g = f.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            g.get_or_insert_with(|| format!("device lost ({reason:?}): {msg}"));
        });
        let module = |label: &str, src: String| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        };
        let seg_module = module("ink segments", format!("{SHADER}{FRAGMENT}"));
        let chunk_module = module("ink chunks", format!("{CHUNK_SHADER}{FRAGMENT}"));
        let comp_module = module("highlighter composite", COMPOSITE_SHADER.to_string());
        let bgl = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let seg_bgl = bgl("segments", &[uniform_entry(0, false)]);
        let chunk_bgl0 = bgl("chunk draw", &[uniform_entry(0, true)]);
        let chunk_bgl1 = bgl(
            "chunk batch",
            &[
                storage_entry(0),
                storage_entry(1),
                storage_entry(2),
                storage_entry(3),
                storage_entry(4),
                storage_entry(5),
                storage_entry(6),
            ],
        );
        let comp_bgl = bgl(
            "layer",
            &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        );
        let layout = |groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let seg_layout = layout(&[Some(&seg_bgl)]);
        let chunk_layout = layout(&[Some(&chunk_bgl0), Some(&chunk_bgl1)]);
        let comp_layout = layout(&[Some(&comp_bgl)]);
        let tile_module = module("tiles", TILE_SHADER.to_string());
        let clear_module = module("tile clear", CLEAR_SHADER.to_string());
        let tile_bgl = bgl(
            "tile atlas",
            &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        );
        let tile_layout = layout(&[Some(&tile_bgl)]);
        let clear_layout = layout(&[]);
        let bg_module = module("background", BG_SHADER.to_string());
        let bg_bgl = bgl(
            "background",
            &[wgpu::BindGroupLayoutEntry {
                visibility: wgpu::ShaderStages::FRAGMENT,
                ..uniform_entry(0, false)
            }],
        );
        let bg_layout = layout(&[Some(&bg_bgl)]);
        Renderer {
            faults,
            device,
            queue,
            seg_module,
            seg_layout,
            seg_bgl,
            chunk_module,
            chunk_layout,
            chunk_bgl0,
            chunk_bgl1,
            comp_module,
            comp_layout,
            comp_bgl,
            tile_module,
            tile_layout,
            tile_bgl,
            clear_module,
            clear_layout,
            bg_module,
            bg_layout,
            bg_bgl,
            background: vibenote_model::settings::Background::default(),
            images: Vec::new(),
            images_sig: 0,
            images_dirty: false,
            textures: std::collections::HashMap::new(),
            texture_bytes: 0,
            frames: 0,
            texts: Vec::new(),
            vectors: Vec::new(),
            extras_sig: 0,
            text: crate::text::TextState::new(),
            tiles: crate::tiles::TileCache::default(),
            atlas: None,
            base: None,
            pipelines: Vec::new(),
            layer: None,
            state: crate::scene::State::new(),
            budget: crate::scene::FrameBudget::HEADLESS,
            gpu_cap: crate::scene::MAX_GPU_BYTES,
            lod: crate::lod_worker::LodWorker::spawn(),
            adapter: format!("{} ({:?}, {})", info.name, info.device_type, info.driver_info),
        }
    }

    /// Takes the GPU fault recorded since the last call, if any (checked once per frame).
    pub fn take_fault(&self) -> Option<GpuFault> {
        let msg = self
            .faults
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if self.faults.lost.load(std::sync::atomic::Ordering::Acquire) {
            return Some(GpuFault::Lost(msg.unwrap_or_default()));
        }
        msg.map(GpuFault::Error)
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Work allowed per frame (default [`crate::FrameBudget::HEADLESS`]).
    pub fn set_budget(&mut self, budget: crate::FrameBudget) {
        self.budget = budget;
    }

    /// Forgets all GPU-resident canvas state; the next frame starts over. Needed after mutating
    /// a large canvas in place without changing its first/last object or length.
    pub fn invalidate(&mut self) {
        self.state.clear();
        self.tiles.clear();
        self.base = None;
    }

    /// GPU memory for committed ink before invisible chunks are evicted (default 512 MiB).
    pub fn set_gpu_cap(&mut self, bytes: u64) {
        self.gpu_cap = bytes;
    }

    /// Display transforms of strokes (prettify, ADR 0008; a selection being dragged), from the
    /// next frame on. Following a change journal only the strokes whose transform changed are
    /// re-encoded; otherwise a changed set re-encodes the canvas.
    pub fn set_display(&mut self, display: std::collections::HashMap<vibenote_model::ObjectId, [f64; 6]>) {
        if display != self.state.display && !self.state.set_display(display.clone()) {
            self.state.clear();
            self.state.display = display;
            self.tiles.clear();
            self.base = None;
        }
    }

    /// The images drawn under the ink from the next frame on (PDF pages, ADR 0004), in draw
    /// order. Pixels are uploaded once per key and kept while used (bounded GPU memory).
    pub fn set_images(&mut self, images: Vec<ImageQuad>) {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for i in &images {
            for v in [
                i.key,
                i.rect.min.x.to_bits(),
                i.rect.min.y.to_bits(),
                i.rect.max.x.to_bits(),
                i.rect.max.y.to_bits(),
            ] {
                h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        if h != self.images_sig || images.len() != self.images.len() {
            self.images_sig = h;
            self.images_dirty = true;
        }
        self.images = images;
    }

    /// Text editing geometry (canvas units, same layout as drawn): the caret before content
    /// offset `offset` as (x, top, height).
    pub fn text_caret(&mut self, item: &crate::TextItem, offset: u32) -> (f64, f64, f64) {
        self.text.caret(item, offset)
    }

    /// The content offset nearest to canvas point `p` in a text item.
    pub fn text_hit(&mut self, item: &crate::TextItem, p: (f64, f64)) -> u32 {
        self.text.hit(item, p)
    }

    /// Boxes [x0, y0, x1, y1] covering content range `[a, b)` of a text item.
    pub fn text_ranges(&mut self, item: &crate::TextItem, a: u32, b: u32) -> Vec<[f64; 4]> {
        self.text.ranges(item, a, b)
    }

    /// Text boxes/link labels and vector strokes (shapes) drawn from the next frame on, above
    /// images and below ink.
    pub fn set_extras(&mut self, texts: Vec<crate::TextItem>, vectors: Vec<Stroke>) {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        for t in &texts {
            for v in [t.key, t.origin.x.to_bits(), t.origin.y.to_bits(), t.width.to_bits()] {
                mix(v);
            }
        }
        for s in &vectors {
            mix(u64::from(s.color.0));
            mix(u64::from(s.width.to_bits()));
            for q in &s.samples {
                mix(q.x.to_bits());
                mix(q.y.to_bits());
            }
        }
        if h != self.extras_sig {
            self.extras_sig = h;
            self.images_dirty = true;
        }
        self.texts = texts;
        self.vectors = vectors;
    }

    /// The texture of an image, uploaded on first use; evicts unused ones over the cap.
    fn image_texture(&mut self, img: &ImageQuad) -> Option<wgpu::BindGroup> {
        if let Some(t) = self.textures.get_mut(&img.key) {
            t.used = self.frames;
            return Some(t.bind.clone());
        }
        let bytes = img.pixels.as_ref().as_ref();
        let len = usize::try_from(img.width)
            .ok()?
            .checked_mul(usize::try_from(img.height).ok()?)?
            .checked_mul(4)?;
        if img.width == 0 || img.height == 0 || img.width > 16_384 || img.height > 16_384 || bytes.len() < len {
            return None;
        }
        let size = wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes.get(..len)?,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width.checked_mul(4)?),
                rows_per_image: Some(img.height),
            },
            size,
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.tile_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.texture_bytes = self.texture_bytes.saturating_add(len);
        self.textures.insert(
            img.key,
            ImageTex {
                bind: bind.clone(),
                bytes: len,
                used: self.frames,
            },
        );
        // Drop the least recently drawn textures not used this frame.
        while self.texture_bytes > TEXTURE_CAP {
            let frames = self.frames;
            let Some(victim) = self
                .textures
                .iter()
                .filter(|(_, t)| t.used != frames)
                .min_by_key(|(_, t)| t.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(t) = self.textures.remove(&victim) {
                self.texture_bytes = self.texture_bytes.saturating_sub(t.bytes);
            }
        }
        Some(bind)
    }

    /// The background drawn under the ink from the next frame on (R-CANVAS-006; default blank).
    pub fn set_background(&mut self, bg: vibenote_model::settings::Background) {
        if bg != self.background {
            self.background = bg;
            self.base = None; // the cached committed frame includes the background
        }
    }

    /// Creates every pipeline for `format` up front (startup, not the first frame).
    pub fn warm_up(&mut self, format: wgpu::TextureFormat) {
        for k in [Kind::Seg, Kind::Chunk, Kind::Composite] {
            self.pipeline(format, k);
        }
        self.pipeline(format, Kind::Tile);
        self.pipeline(format, Kind::Blit);
        self.pipeline(format, Kind::Background);
        for k in [Kind::SegMax, Kind::ChunkMax, Kind::Chunk, Kind::Clear] {
            self.pipeline(LAYER_FORMAT, k);
        }
    }

    fn pipeline(&mut self, format: wgpu::TextureFormat, kind: Kind) -> wgpu::RenderPipeline {
        if let Some((_, p)) = self.pipelines.iter().find(|(k, _)| *k == (format, kind)) {
            return p.clone();
        }
        let over = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let max = wgpu::BlendState { color: max, alpha: max };
        let seg_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Seg>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32],
        })];
        let tile_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TileInst>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x2, 4 => Uint32, 5 => Float32],
        })];
        let (module, layout, buffers, blend): (_, _, &[Option<wgpu::VertexBufferLayout<'_>>], _) = match kind {
            Kind::Seg => (&self.seg_module, &self.seg_layout, &seg_buffers, over),
            Kind::SegMax => (&self.seg_module, &self.seg_layout, &seg_buffers, max),
            Kind::Chunk => (&self.chunk_module, &self.chunk_layout, &[], over),
            Kind::ChunkMax => (&self.chunk_module, &self.chunk_layout, &[], max),
            Kind::Composite => (&self.comp_module, &self.comp_layout, &[], over),
            Kind::Tile => (&self.tile_module, &self.tile_layout, &tile_buffers, over),
            Kind::Clear => (&self.clear_module, &self.clear_layout, &[], over),
            Kind::Blit => (&self.comp_module, &self.comp_layout, &[], over),
            Kind::Background => (&self.bg_module, &self.bg_layout, &[], over),
        };
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ink"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: (!matches!(kind, Kind::Clear | Kind::Blit)).then_some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: if matches!(kind, Kind::Chunk | Kind::ChunkMax | Kind::Tile) {
                    wgpu::PrimitiveTopology::TriangleStrip
                } else {
                    wgpu::PrimitiveTopology::TriangleList
                },
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push(((format, kind), p.clone()));
        p
    }

    /// Records the canvas (strokes in z order, highlighter below pen ink) plus live strokes on
    /// top into `target` (cleared to paper). Does bounded work per call (`set_budget`); content
    /// not ingested yet appears over the next frames (`FrameStats::complete`). Submitting is
    /// the caller's job. Untracked: changes are detected without a journal (see `scene.rs`).
    pub fn encode(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        canvas: &Canvas,
        live: &[&Stroke],
        vp: &Viewport,
    ) -> Result<crate::FrameStats, RenderError> {
        self.encode_scene(
            enc,
            target,
            format,
            &Scene {
                canvas,
                changes: None,
                live,
                viewport: *vp,
            },
        )
    }

    /// [`Renderer::encode`] for a scene, optionally with the canvas's change journal (the app):
    /// then only the changed objects are re-encoded each frame.
    pub fn encode_scene(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        scene: &Scene<'_>,
    ) -> Result<crate::FrameStats, RenderError> {
        let r = self.encode_scene_inner(enc, target, format, scene);
        if r.is_err() {
            // The encoder will not be submitted: uploads already counted as done (and recorded
            // buffer copies) would be lost, so start over rather than draw stale buffers.
            self.state.clear();
        }
        r
    }

    fn encode_scene_inner(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        scene: &Scene<'_>,
    ) -> Result<crate::FrameStats, RenderError> {
        let (canvas, live, vp) = (scene.canvas, scene.live, &scene.viewport);
        vp.check()?;
        let budget = self.budget;
        let ingested = self.state.sync(canvas, scene.changes, &budget)?;
        self.exchange_lod();
        let view = Rect {
            min: Point::new(vp.origin_x, vp.origin_y),
            max: Point::new(
                vp.origin_x + f64::from(vp.width) / vp.zoom,
                vp.origin_y + f64::from(vp.height) / vp.zoom,
            ),
        };
        let visible = self.state.visible(&view);
        self.state.evict(self.gpu_cap);
        let uploaded = self.state.upload(
            &self.device,
            &self.queue,
            enc,
            &self.chunk_bgl1,
            budget.upload_bytes,
            &visible,
        );

        let frame_cam = Cam {
            x: vp.origin_x,
            y: vp.origin_y,
            zoom: vp.zoom,
            w: vp.width,
            h: vp.height,
            off: (0, 0),
        };
        let mut fr = Frame::default();
        fr.chunks(&self.state, &visible, &frame_cam, Dest::Frame)?;
        // Zoomed out, or too much ink on screen: composite cached tiles instead (M3.3).
        let tiled = vp.zoom < TILE_ZOOM || fr.instances > TILE_INSTANCES;
        let mut fills: Vec<Fill> = Vec::new();
        let mut tile_insts: Vec<TileInst> = Vec::new();
        let mut tiles_done = true;
        let (mut tiles_missing, mut tiles_drawn) = (0u32, 0u32);
        if tiled {
            fr = Frame::default();
            let t = self.plan_tiles(&view, vp, budget.tile_fills, &mut fr, &mut fills)?;
            tile_insts = t.0;
            tiles_done = t.1;
            tiles_missing = t.2;
            tiles_drawn = u32::try_from(tile_insts.len()).unwrap_or(u32::MAX);
        }
        let dim = |v: u32| f32::from(u16::try_from(v).unwrap_or(u16::MAX));
        if fr.uniforms.is_empty() {
            fr.uniforms.resize(usize::try_from(SLOT).unwrap_or(256), 0);
        }
        use wgpu::util::DeviceExt;
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chunk draws"),
            contents: &fr.uniforms,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let draw_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.chunk_bgl0,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ubuf,
                    offset: 0,
                    size: std::num::NonZeroU64::new(std::mem::size_of::<DrawU>() as u64),
                }),
            }],
        });

        // Live strokes: per-segment path (highlighters into the layer).
        let mut live_pen = Vec::new();
        let mut live_hl = Vec::new();
        for s in live {
            match s.tool {
                Tool::Highlighter => push_stroke(&mut live_hl, s, vp)?,
                Tool::Pen => push_stroke(&mut live_pen, s, vp)?,
            }
        }
        let seg_u = Uniforms {
            size: [dim(vp.width), dim(vp.height)],
            min_r: 0.35,
            _pad: 0.0,
        };
        let seg_ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&seg_u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let seg_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.seg_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: seg_ubuf.as_entire_binding(),
            }],
        });
        let seg_vbuf = |segs: &[Seg]| {
            (!segs.is_empty()).then(|| {
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(segs),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            })
        };
        let (pen_vb, hl_vb) = (seg_vbuf(&live_pen), seg_vbuf(&live_hl));
        let count =
            |segs: &[Seg]| u32::try_from(segs.len()).map_err(|_| RenderError::Viewport("too many segments".into()));
        let (n_pen, n_hl) = (count(&live_pen)?, count(&live_hl)?);

        // Tile fills: one pass per atlas layer; each tile is cleared, then highlighter (max)
        // and pen ink are drawn into its slot.
        if !fills.is_empty() {
            let (clear_p, chunk_max, chunk_l) = (
                self.pipeline(LAYER_FORMAT, Kind::Clear),
                self.pipeline(LAYER_FORMAT, Kind::ChunkMax),
                self.pipeline(LAYER_FORMAT, Kind::Chunk),
            );
            let layers: Vec<wgpu::TextureView> = self.atlas.as_ref().map(|a| a.layers.clone()).unwrap_or_default();
            for (li, lv) in layers.iter().enumerate() {
                let mine: Vec<usize> = (0..fills.len())
                    .filter(|&i| fills.get(i).is_some_and(|f| usize::try_from(f.layer).ok() == Some(li)))
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                let mut rp = begin_load(enc, lv);
                for i in mine {
                    let Some(f) = fills.get(i) else { continue };
                    let t = f32::from(u16::try_from(crate::tiles::TILE).unwrap_or(256));
                    rp.set_viewport(
                        f32::from(u16::try_from(f.x).unwrap_or(0)),
                        f32::from(u16::try_from(f.y).unwrap_or(0)),
                        t,
                        t,
                        0.0,
                        1.0,
                    );
                    rp.set_scissor_rect(f.x, f.y, crate::tiles::TILE, crate::tiles::TILE);
                    rp.set_pipeline(&clear_p);
                    rp.draw(0..3, 0..1);
                    rp.set_pipeline(&chunk_max);
                    self.draw_chunks(&mut rp, &fr.draws, &draw_bind, Dest::Fill(i), true)?;
                    rp.set_pipeline(&chunk_l);
                    self.draw_chunks(&mut rp, &fr.draws, &draw_bind, Dest::Fill(i), false)?;
                }
            }
        }

        // The committed frame is redrawn only when something in it changed; otherwise the
        // cached copy is reused and only live ink is drawn (pen frames stay cheap on large
        // notes).
        let fading = tiled && !tiles_done;
        let changed = std::mem::take(&mut self.state.dirty)
            || std::mem::take(&mut self.images_dirty)
            || uploaded > 0
            || !fills.is_empty()
            || fading
            || !self.base.as_ref().is_some_and(|b| b.vp == *vp && b.format == format);
        let (chunk_p, seg_p, comp_p, blit_p) = (
            self.pipeline(format, Kind::Chunk),
            self.pipeline(format, Kind::Seg),
            self.pipeline(format, Kind::Composite),
            self.pipeline(format, Kind::Blit),
        );
        let chunk_max = self.pipeline(LAYER_FORMAT, Kind::ChunkMax);
        let seg_max = self.pipeline(LAYER_FORMAT, Kind::SegMax);
        let c = PAPER.map(|v| f64::from(v) / 255.0);
        let paper = wgpu::Color {
            r: c[0],
            g: c[1],
            b: c[2],
            a: 1.0,
        };
        if changed {
            let base_view = self.base_target(vp, format);
            // Committed highlighter ink into the layer (max blending: overlaps never darken).
            let committed_hl = if fr.has_hl {
                let (view, bind) = self.layer(vp.width, vp.height);
                let mut rp = begin(enc, &view, wgpu::Color::TRANSPARENT);
                rp.set_pipeline(&chunk_max);
                self.draw_chunks(&mut rp, &fr.draws, &draw_bind, Dest::Frame, true)?;
                Some(bind)
            } else {
                None
            };
            let tile_p = (!tile_insts.is_empty()).then(|| self.pipeline(format, Kind::Tile));
            let tile_vb = (!tile_insts.is_empty()).then(|| {
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("tiles"),
                    contents: bytemuck::cast_slice(&tile_insts),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let bg = background_uniforms(&self.background, vp).map(|u| {
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("background"),
                    contents: bytemuck::bytes_of(&u),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("background"),
                    layout: &self.bg_bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    }],
                });
                (self.pipeline(format, Kind::Background), bind)
            });
            // Images (PDF pages) in view, as one-layer textures through the tile composite.
            self.frames = self.frames.wrapping_add(1);
            let view_rect = vibenote_geom::Rect {
                min: vibenote_geom::Point::new(vp.origin_x, vp.origin_y),
                max: vibenote_geom::Point::new(
                    vp.origin_x + f64::from(vp.width) / vp.zoom,
                    vp.origin_y + f64::from(vp.height) / vp.zoom,
                ),
            };
            let visible: Vec<ImageQuad> = self
                .images
                .iter()
                .filter(|i| {
                    i.rect.min.x <= view_rect.max.x
                        && i.rect.max.x >= view_rect.min.x
                        && i.rect.min.y <= view_rect.max.y
                        && i.rect.max.y >= view_rect.min.y
                })
                .cloned()
                .collect();
            let mut image_draws: Vec<(wgpu::BindGroup, TileInst)> = Vec::new();
            for img in &visible {
                if let Some(bind) = self.image_texture(img) {
                    image_draws.push((bind, image_inst(img, vp)));
                }
            }
            let image_vb = (!image_draws.is_empty()).then(|| {
                let insts: Vec<TileInst> = image_draws.iter().map(|(_, i)| *i).collect();
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("images"),
                    contents: bytemuck::cast_slice(&insts),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let image_p = (!image_draws.is_empty()).then(|| self.pipeline(format, Kind::Tile));
            // Shapes and text decorations as capsule segments; text through glyphon.
            let mut vector_segs = Vec::new();
            for s in &self.vectors {
                push_stroke(&mut vector_segs, s, vp)?;
            }
            let texts = std::mem::take(&mut self.texts);
            for (a, b, t, color) in self.text.decorations(&texts) {
                let line = Stroke {
                    tool: Tool::Pen,
                    color,
                    width: t,
                    samples: [a, b]
                        .iter()
                        .map(|q| vibenote_model::Sample {
                            x: q.x,
                            y: q.y,
                            pressure: 1.0,
                            tilt_x: 0.0,
                            tilt_y: 0.0,
                            t_us: 0,
                        })
                        .collect(),
                };
                push_stroke(&mut vector_segs, &line, vp)?;
            }
            let has_text = self.text.prepare(&self.device, &self.queue, format, &texts, vp);
            self.texts = texts;
            let vector_vb = (!vector_segs.is_empty()).then(|| {
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vectors"),
                    contents: bytemuck::cast_slice(&vector_segs),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let n_vec = count(&vector_segs)?;
            let mut rp = begin(enc, &base_view, paper);
            if let Some((p, bind)) = &bg {
                rp.set_pipeline(p);
                rp.set_bind_group(0, bind, &[]);
                rp.draw(0..3, 0..1);
            }
            if let (Some(p), Some(vb)) = (&image_p, &image_vb) {
                rp.set_pipeline(p);
                rp.set_vertex_buffer(0, vb.slice(..));
                for (i, (bind, _)) in (0u32..).zip(&image_draws) {
                    rp.set_bind_group(0, bind, &[]);
                    rp.draw(0..4, i..i.saturating_add(1));
                }
            }
            if let Some(vb) = &vector_vb {
                rp.set_pipeline(&seg_p);
                rp.set_bind_group(0, &seg_bind, &[]);
                rp.set_vertex_buffer(0, vb.slice(..));
                rp.draw(0..6, 0..n_vec);
            }
            if has_text {
                self.text.render(&mut rp);
            }
            if let (Some(p), Some(vb), Some(a)) = (&tile_p, &tile_vb, &self.atlas) {
                rp.set_pipeline(p);
                rp.set_bind_group(0, &a.bind, &[]);
                rp.set_vertex_buffer(0, vb.slice(..));
                let n = u32::try_from(tile_insts.len()).map_err(|_| RenderError::Viewport("too many tiles".into()))?;
                rp.draw(0..4, 0..n);
            }
            if let Some(b) = &committed_hl {
                rp.set_scissor_rect(0, 0, vp.width, vp.height);
                rp.set_pipeline(&comp_p);
                rp.set_bind_group(0, b, &[]);
                rp.draw(0..3, 0..1);
            }
            rp.set_pipeline(&chunk_p);
            self.draw_chunks(&mut rp, &fr.draws, &draw_bind, Dest::Frame, false)?;
            drop(rp);
            self.text.trim();
        }
        // Live highlighter ink into the layer.
        let live_layer = if let Some(vb) = &hl_vb {
            let (view, bind) = self.layer(vp.width, vp.height);
            let mut rp = begin(enc, &view, wgpu::Color::TRANSPARENT);
            rp.set_pipeline(&seg_max);
            rp.set_bind_group(0, &seg_bind, &[]);
            rp.set_vertex_buffer(0, vb.slice(..));
            rp.draw(0..6, 0..n_hl);
            Some(bind)
        } else {
            None
        };
        let base_bind = self.base.as_ref().map(|b| b.bind.clone());
        let mut rp = begin(enc, target, paper);
        if let Some(b) = &base_bind {
            rp.set_pipeline(&blit_p);
            rp.set_bind_group(0, b, &[]);
            rp.draw(0..3, 0..1);
        }
        if let Some(b) = &live_layer {
            rp.set_pipeline(&comp_p);
            rp.set_bind_group(0, b, &[]);
            rp.draw(0..3, 0..1);
        }
        if let Some(vb) = &pen_vb {
            rp.set_scissor_rect(0, 0, vp.width, vp.height);
            rp.set_pipeline(&seg_p);
            rp.set_bind_group(0, &seg_bind, &[]);
            rp.set_vertex_buffer(0, vb.slice(..));
            rp.draw(0..6, 0..n_pen);
        }
        drop(rp);
        let gpu_bytes = self.state.chunks.values().map(crate::scene::Chunk::gpu_bytes).sum();
        Ok(crate::FrameStats {
            complete: self.state.complete() && tiles_done,
            chunks: u32::try_from(self.state.chunks.len()).unwrap_or(u32::MAX),
            chunks_visible: u32::try_from(visible.len()).unwrap_or(u32::MAX),
            instances: fr.instances,
            ingested_samples: ingested,
            upload_bytes: uploaded,
            gpu_bytes,
            tiled,
            tiles_drawn,
            tiles_filled: u32::try_from(fills.len()).unwrap_or(u32::MAX),
            tiles_missing,
            cached: !changed,
        })
    }

    /// Plans this frame's tiles: which to fill (within `fill_budget`, their draws go into
    /// `fr`), and the composite list (cached tiles, fallbacks, fades). Returns the composite
    /// instances, whether every needed tile is fresh, and how many visible tiles were missing.
    fn plan_tiles(
        &mut self,
        view: &Rect,
        vp: &Viewport,
        fill_budget: u32,
        fr: &mut Frame,
        fills: &mut Vec<Fill>,
    ) -> Result<(Vec<TileInst>, bool, u32), RenderError> {
        use crate::tiles::{FADE_FRAMES, Key, TILE, Tile, level_for, overview_level, slot_pos, tiles_in};
        let frame = self.state.frame;
        let level = level_for(vp.zoom);
        let visible_keys = tiles_in(view, level);
        let content = self
            .state
            .chunks
            .values()
            .filter(|c| !c.bounds.is_empty())
            .fold(Rect::EMPTY, |mut r, c| {
                r.include(c.bounds.min);
                r.include(c.bounds.max);
                r
            });
        let overview: Vec<Key> = if content.is_empty() {
            Vec::new()
        } else {
            tiles_in(&content, overview_level(&content))
        };
        // Signature of the chunks under a tile (stale when any changed).
        let under = |state: &crate::scene::State, r: &Rect| -> Vec<crate::encode::Cell> {
            let mut cells: Vec<_> = state
                .chunks
                .values()
                .filter(|c| {
                    !c.bounds.is_empty()
                        && c.bounds.min.x <= r.max.x
                        && c.bounds.max.x >= r.min.x
                        && c.bounds.min.y <= r.max.y
                        && c.bounds.max.y >= r.min.y
                })
                .map(|c| c.cell)
                .collect();
            cells.sort_unstable();
            cells
        };
        let sig = |state: &crate::scene::State, cells: &[crate::encode::Cell]| {
            cells.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
                let v = state.chunks.get(c).map_or(0, |ch| ch.version);
                [c.x.to_le_bytes(), c.y.to_le_bytes(), v.to_le_bytes()]
                    .iter()
                    .flatten()
                    .fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
            })
        };
        let mut all: Vec<Key> = visible_keys.clone();
        for k in &overview {
            if !all.contains(k) {
                all.push(*k);
            }
        }
        // What needs filling: stale visible tiles (edits) first, then missing ones nearest the
        // centre, then the overview.
        let (cx, cy) = (
            f64::midpoint(view.min.x, view.max.x),
            f64::midpoint(view.min.y, view.max.y),
        );
        let mut todo: Vec<(u8, u64, Key, Vec<crate::encode::Cell>, u64)> = Vec::new();
        let mut done = true;
        for k in &all {
            let cells = under(&self.state, &k.rect());
            let s = sig(&self.state, &cells);
            let existing = self.tiles.tiles.get(k).copied();
            if existing.is_some_and(|t| t.sig == s) {
                continue;
            }
            if cells.is_empty() {
                if let Some(slot) = existing.and_then(|t| t.slot) {
                    self.tiles.release(slot);
                }
                self.tiles.tiles.insert(
                    *k,
                    Tile {
                        slot: None,
                        sig: s,
                        last_used: frame,
                        fade: 0,
                    },
                );
                continue;
            }
            done = false;
            let r = k.rect();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // distance rank only
            let dist = (f64::midpoint(r.min.x, r.max.x) - cx)
                .hypot(f64::midpoint(r.min.y, r.max.y) - cy)
                .min(1e18) as u64;
            let prio = if !visible_keys.contains(k) {
                2
            } else if existing.is_some_and(|t| t.slot.is_some()) {
                0
            } else {
                1
            };
            todo.push((prio, dist, *k, cells, s));
        }
        todo.sort_by_key(|t| (t.0, t.1, t.2));
        let keep: Vec<Key> = all.clone();
        let budget = usize::try_from(fill_budget).unwrap_or(usize::MAX);
        let mut filled_now: Vec<Key> = Vec::new();
        for (_, _, k, cells, s) in todo.into_iter().take(budget) {
            // Fills also stop at an instance budget: a tile smaller than a chunk draws whole
            // chunks (scissored), so many fills at a high tile zoom cost more than a frame.
            if !fills.is_empty() && fr.instances >= FILL_INSTANCES {
                break;
            }
            let existing = self.tiles.tiles.get(&k).copied();
            let slot = match existing.and_then(|t| t.slot) {
                Some(slot) => slot,
                None => match self.tiles.slot(&keep) {
                    Some(slot) => slot,
                    None => continue,
                },
            };
            self.ensure_atlas();
            let (layer, x, y) = slot_pos(slot);
            let r = k.rect();
            let cam = Cam {
                x: r.min.x,
                y: r.min.y,
                zoom: k.zoom(),
                w: TILE,
                h: TILE,
                off: (x, y),
            };
            fr.chunks(&self.state, &cells, &cam, Dest::Fill(fills.len()))?;
            fills.push(Fill { layer, x, y });
            // A tile appearing where a fallback was shown fades in.
            let had_fallback = existing.is_none() && visible_keys.contains(&k) && self.fallback(k).is_some();
            self.tiles.tiles.insert(
                k,
                Tile {
                    slot: Some(slot),
                    sig: s,
                    last_used: frame,
                    fade: if had_fallback { FADE_FRAMES } else { 0 },
                },
            );
            filled_now.push(k);
        }
        // Composite: fresh tiles (fading in over their fallback), else a stale copy or the
        // nearest coarser tile.
        let mut insts = Vec::new();
        let mut missing = 0u32;
        let mut still_fresh = true;
        for k in &visible_keys {
            let cells = under(&self.state, &k.rect());
            let s = sig(&self.state, &cells);
            let tile = self.tiles.tiles.get(k).copied();
            let fresh = tile.is_some_and(|t| t.sig == s);
            still_fresh &= fresh;
            match tile {
                Some(t) if fresh => {
                    let Some(slot) = t.slot else { continue };
                    // Crossfade: the fallback fades out as the tile fades in (both premultiplied).
                    let alpha = if t.fade > 0 {
                        done = false; // this frame still mixes in the fallback
                        let f = f32::from(t.fade) / f32::from(FADE_FRAMES);
                        if let Some((fk, fslot)) = self.fallback(*k) {
                            insts.push(tile_inst(*k, fk, fslot, vp, f));
                        }
                        1.0 - f
                    } else {
                        1.0
                    };
                    insts.push(tile_inst(*k, *k, slot, vp, alpha));
                    if let Some(tm) = self.tiles.tiles.get_mut(k) {
                        tm.last_used = frame;
                        tm.fade = tm.fade.saturating_sub(1);
                    }
                }
                Some(Tile { slot: Some(slot), .. }) => {
                    missing = missing.saturating_add(1);
                    insts.push(tile_inst(*k, *k, slot, vp, 1.0));
                }
                _ => {
                    missing = missing.saturating_add(1);
                    if let Some((fk, fslot)) = self.fallback(*k) {
                        insts.push(tile_inst(*k, fk, fslot, vp, 1.0));
                    }
                }
            }
        }
        for k in &overview {
            let cells = under(&self.state, &k.rect());
            still_fresh &= self
                .tiles
                .tiles
                .get(k)
                .is_some_and(|t| t.sig == sig(&self.state, &cells));
        }
        let _ = filled_now;
        Ok((insts, done && still_fresh, missing))
    }

    /// The nearest coarser cached tile covering `k` (fresh or stale), with its slot.
    fn fallback(&self, k: crate::tiles::Key) -> Option<(crate::tiles::Key, u32)> {
        let mut level = k.level.saturating_sub(1);
        while level >= crate::tiles::LEVEL_MIN {
            let c = k.at_level(level);
            if let Some(slot) = self.tiles.tiles.get(&c).and_then(|t| t.slot) {
                return Some((c, slot));
            }
            level = level.saturating_sub(1);
        }
        None
    }

    /// Creates the tile atlas on first use.
    fn ensure_atlas(&mut self) {
        if self.atlas.is_some() {
            return;
        }
        use crate::tiles::{ATLAS, LAYERS};
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile atlas"),
            size: wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: LAYERS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let layers = (0..LAYERS)
            .map(|l| {
                tex.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: l,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let all = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tiles"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile atlas"),
            layout: &self.tile_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&all),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.atlas = Some(Atlas { layers, bind });
    }

    fn draw_chunks(
        &self,
        rp: &mut wgpu::RenderPass<'_>,
        draws: &[DrawItem],
        draw_bind: &wgpu::BindGroup,
        dest: Dest,
        hl: bool,
    ) -> Result<(), RenderError> {
        for d in draws.iter().filter(|d| d.hl == hl && d.dest == dest) {
            let Some(c) = self.state.chunks.get(&d.cell) else {
                continue;
            };
            let list = if hl { &c.hl } else { &c.pen };
            let Some(g) = list.get(d.batch).and_then(|b| b.gpu.as_ref()) else {
                continue;
            };
            if let Some([x, y, w, h]) = d.scissor {
                rp.set_scissor_rect(x, y, w, h);
            }
            let off = u32::try_from(u64::from(d.slot).saturating_mul(SLOT))
                .map_err(|_| RenderError::Viewport("uniforms".into()))?;
            rp.set_bind_group(0, draw_bind, &[off]);
            rp.set_bind_group(1, &g.bind, &[]);
            rp.draw(0..4, d.instances.clone());
        }
        Ok(())
    }

    /// Sends new shares to the LOD worker and takes its finished results (never waits).
    fn exchange_lod(&mut self) {
        let jobs = std::mem::take(&mut self.state.lod.jobs);
        match self.lod.as_mut() {
            Some(w) => {
                for j in jobs {
                    w.send(j);
                }
                while let Some(d) = w.try_done() {
                    self.state.apply_lod(d);
                }
            }
            None => {
                for j in jobs {
                    self.state.apply_lod(crate::lod_worker::run(j));
                }
            }
        }
    }

    /// Waits for every outstanding LOD result (settled renders).
    fn wait_lod(&mut self) {
        let jobs = std::mem::take(&mut self.state.lod.jobs);
        if let Some(w) = self.lod.as_mut() {
            for j in jobs {
                w.send(j);
            }
            while let Some(d) = w.wait_done() {
                self.state.apply_lod(d);
            }
        } else {
            for j in jobs {
                self.state.apply_lod(crate::lod_worker::run(j));
            }
        }
    }

    /// The cached committed-frame texture for this viewport and format (recreated on change).
    fn base_target(&mut self, vp: &Viewport, format: wgpu::TextureFormat) -> wgpu::TextureView {
        if let Some(b) = &self.base
            && b.format == format
            && (b.vp.width, b.vp.height) == (vp.width, vp.height)
        {
            let view = b.view.clone();
            if let Some(b) = self.base.as_mut() {
                b.vp = *vp;
            }
            return view;
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("committed frame"),
            size: wgpu::Extent3d {
                width: vp.width,
                height: vp.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.comp_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        self.base = Some(Base {
            vp: *vp,
            format,
            view: view.clone(),
            bind,
        });
        view
    }

    /// The highlighter layer texture for this size (cached).
    fn layer(&mut self, w: u32, h: u32) -> (wgpu::TextureView, wgpu::BindGroup) {
        if let Some((lw, lh, view, bind)) = &self.layer
            && (*lw, *lh) == (w, h)
        {
            return (view.clone(), bind.clone());
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("highlighter layer"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.comp_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        self.layer = Some((w, h, view.clone(), bind.clone()));
        (view, bind)
    }

    /// Reference renderer: every stroke as per-segment instances in (z, id) order, no state
    /// (the M1 path). Test oracle for the chunked renderer.
    fn encode_reference(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        canvas: &Canvas,
        vp: &Viewport,
    ) -> Result<(), RenderError> {
        vp.check()?;
        let segs = segments(canvas, &[], vp)?;
        let pipeline = self.pipeline(format, Kind::Seg);
        use wgpu::util::DeviceExt;
        let dim = |v: u32| f32::from(u16::try_from(v).unwrap_or(u16::MAX));
        let u = Uniforms {
            size: [dim(vp.width), dim(vp.height)],
            min_r: 0.35,
            _pad: 0.0,
        };
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.seg_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            }],
        });
        let vbuf = (!segs.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&segs),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let c = PAPER.map(|v| f64::from(v) / 255.0);
        let mut rp = begin(
            enc,
            target,
            wgpu::Color {
                r: c[0],
                g: c[1],
                b: c[2],
                a: 1.0,
            },
        );
        if let Some(vb) = &vbuf {
            rp.set_pipeline(&pipeline);
            rp.set_bind_group(0, &bind, &[]);
            rp.set_vertex_buffer(0, vb.slice(..));
            let n = u32::try_from(segs.len()).map_err(|_| RenderError::Viewport("too many segments".into()))?;
            rp.draw(0..6, 0..n);
        }
        Ok(())
    }
}

/// Opens a render pass that keeps the target's contents (tile slots in the atlas).
fn begin_load<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("tile fills"),
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
}

/// The composite quad for screen tile `k`, sampled from cached tile `src` (the same tile or a
/// coarser one containing it) in `slot`.
fn tile_inst(k: crate::tiles::Key, src: crate::tiles::Key, slot: u32, vp: &Viewport, alpha: f32) -> TileInst {
    use crate::tiles::{ATLAS, TILE, slot_pos};
    let r = k.rect();
    let sr = src.rect();
    let (layer, sx, sy) = slot_pos(slot);
    let atlas = f64::from(ATLAS);
    let (span_x, span_y) = (sr.max.x - sr.min.x, sr.max.y - sr.min.y);
    let u = |x: f64| (f64::from(sx) + (x - sr.min.x) / span_x * f64::from(TILE)) / atlas;
    let v = |y: f64| (f64::from(sy) + (y - sr.min.y) / span_y * f64::from(TILE)) / atlas;
    let half = 0.5 / atlas;
    let dim = |x: u32| f32::from(u16::try_from(x).unwrap_or(u16::MAX));
    TileInst {
        rect: [
            px((r.min.x - vp.origin_x) * vp.zoom),
            px((r.min.y - vp.origin_y) * vp.zoom),
            px((r.max.x - vp.origin_x) * vp.zoom),
            px((r.max.y - vp.origin_y) * vp.zoom),
        ],
        uv: [px(u(r.min.x)), px(v(r.min.y)), px(u(r.max.x)), px(v(r.max.y))],
        lim: [
            px(f64::from(sx) / atlas + half),
            px(f64::from(sy) / atlas + half),
            px(f64::from(sx.saturating_add(TILE)) / atlas - half),
            px(f64::from(sy.saturating_add(TILE)) / atlas - half),
        ],
        size: [dim(vp.width), dim(vp.height)],
        layer,
        alpha,
    }
}

/// The composite quad of an image (whole texture, opaque).
fn image_inst(img: &ImageQuad, vp: &Viewport) -> TileInst {
    let dim = |x: u32| f32::from(u16::try_from(x).unwrap_or(u16::MAX));
    let (hu, hv) = (0.5 / f64::from(img.width.max(1)), 0.5 / f64::from(img.height.max(1)));
    TileInst {
        rect: [
            px((img.rect.min.x - vp.origin_x) * vp.zoom),
            px((img.rect.min.y - vp.origin_y) * vp.zoom),
            px((img.rect.max.x - vp.origin_x) * vp.zoom),
            px((img.rect.max.y - vp.origin_y) * vp.zoom),
        ],
        uv: [0.0, 0.0, 1.0, 1.0],
        lim: [px(hu), px(hv), px(1.0 - hu), px(1.0 - hv)],
        size: [dim(vp.width), dim(vp.height)],
        layer: 0,
        alpha: 1.0,
    }
}

/// Opens a render pass with one colour attachment cleared to `clear`.
fn begin<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, clear: wgpu::Color) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// Pixels owned by a cell (pixel centres inside it) as seen through `cam`, clamped to the
/// camera's rectangle and placed at its offset; `None` if empty.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=8192 first
fn cell_scissor(cell: crate::encode::Cell, cam: &Cam) -> Option<[u32; 4]> {
    let r = cell.rect();
    let edge = |v: f64, origin: f64, max: u32| ((v - origin) * cam.zoom - 0.5).ceil().clamp(0.0, f64::from(max)) as u32;
    let (x0, x1) = (edge(r.min.x, cam.x, cam.w), edge(r.max.x, cam.x, cam.w));
    let (y0, y1) = (edge(r.min.y, cam.y, cam.h), edge(r.max.y, cam.y, cam.h));
    (x1 > x0 && y1 > y0).then(|| {
        [
            x0.saturating_add(cam.off.0),
            y0.saturating_add(cam.off.1),
            x1.saturating_sub(x0),
            y1.saturating_sub(y0),
        ]
    })
}

fn warm_stroke(tool: Tool) -> Stroke {
    Stroke {
        tool,
        color: vibenote_model::Color::INK,
        width: 2.0,
        samples: [(10.0, 10.0), (60.0, 40.0)]
            .iter()
            .map(|&(x, y)| vibenote_model::Sample {
                x,
                y,
                pressure: 0.5,
                tilt_x: 0.0,
                tilt_y: 0.0,
                t_us: 0,
            })
            .collect(),
    }
}

/// Offscreen renderer with CPU readback (harness: goldens, `render-png`, screenshots).
pub struct Headless {
    r: Renderer,
    target: Option<Target>,
    /// Readback buffer, reused for every size it fits (grown when too small).
    readback: Option<wgpu::Buffer>,
    pub adapter: String,
}

struct Target {
    w: u32,
    h: u32,
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    bpr: u32,
}

/// Readback space prepared up front: a 3840×2160 RGBA frame (covers screens up to 4K).
const READBACK_PREPARED: u64 = 15_360 * 2_160;

impl Headless {
    /// Creates a headless renderer and warms it up (pipelines, one small frame), so the first
    /// real frame does no one-time work. `VIBENOTE_ADAPTER=cpu` selects a software adapter.
    pub fn new() -> Result<Self, RenderError> {
        let (adapter, device, queue) = request_device(&instance(), None)?;
        let mut r = Renderer::new(&adapter, device, queue);
        r.warm_up(HEADLESS_FORMAT);
        // The harness renders zoomed-out frames too: allocate the tile atlas now, not in the
        // first tiled frame.
        r.ensure_atlas();
        let mut h = Headless {
            adapter: r.adapter.clone(),
            r,
            target: None,
            readback: None,
        };
        h.prepare_readback(READBACK_PREPARED)?;
        let vp = Viewport {
            origin_x: 0.0,
            origin_y: 0.0,
            zoom: 1.0,
            width: 256,
            height: 256,
        };
        // One pen and one highlighter stroke, committed and live, so every pipeline and buffer
        // path has run once before the first real frame.
        let mut warm = Canvas::default();
        for (k, tool) in [(1u64, Tool::Pen), (2, Tool::Highlighter)] {
            let id = vibenote_model::ObjectId(k);
            warm.objects.insert(
                id,
                vibenote_model::CanvasObject {
                    id,
                    z: 0,
                    kind: ObjectKind::Stroke(warm_stroke(tool)),
                },
            );
        }
        let live = warm_stroke(Tool::Highlighter);
        h.render(&warm, &[&live], &vp)?;
        // A few full-size frames: the GPU leaves its idle clocks before the first real frame
        // (the first frames of a process were ~2× slower without this).
        let big = Viewport {
            width: 1024,
            height: 1024,
            ..vp
        };
        for _ in 0..4 {
            h.render(&warm, &[&live], &big)?;
        }
        h.r.invalidate();
        Ok(h)
    }

    /// Work allowed per frame (default [`crate::FrameBudget::HEADLESS`]).
    pub fn set_budget(&mut self, budget: crate::FrameBudget) {
        self.r.set_budget(budget);
    }

    pub fn invalidate(&mut self) {
        self.r.invalidate();
    }

    pub fn set_gpu_cap(&mut self, bytes: u64) {
        self.r.set_gpu_cap(bytes);
    }

    /// See [`Renderer::set_images`].
    pub fn set_images(&mut self, images: Vec<ImageQuad>) {
        self.r.set_images(images);
    }

    /// See [`Renderer::set_display`].
    pub fn set_display(&mut self, display: std::collections::HashMap<vibenote_model::ObjectId, [f64; 6]>) {
        self.r.set_display(display);
    }

    /// See [`Renderer::text_caret`].
    pub fn text_caret(&mut self, item: &crate::TextItem, offset: u32) -> (f64, f64, f64) {
        self.r.text_caret(item, offset)
    }

    /// See [`Renderer::text_hit`].
    pub fn text_hit(&mut self, item: &crate::TextItem, p: (f64, f64)) -> u32 {
        self.r.text_hit(item, p)
    }

    /// See [`Renderer::text_ranges`].
    pub fn text_ranges(&mut self, item: &crate::TextItem, a: u32, b: u32) -> Vec<[f64; 4]> {
        self.r.text_ranges(item, a, b)
    }

    /// See [`Renderer::set_extras`].
    pub fn set_extras(&mut self, texts: Vec<crate::TextItem>, vectors: Vec<Stroke>) {
        self.r.set_extras(texts, vectors);
    }

    /// See [`Renderer::set_background`].
    pub fn set_background(&mut self, bg: vibenote_model::settings::Background) {
        self.r.set_background(bg);
    }

    /// Renders one frame of the canvas (strokes by z then id, highlighter below pen ink) plus
    /// optional live strokes on top. Does bounded work: a large canvas completes over several
    /// calls (see [`Headless::render_settled`]).
    pub fn render(&mut self, canvas: &Canvas, live: &[&Stroke], vp: &Viewport) -> Result<Image, RenderError> {
        Ok(self.render_stats(canvas, live, vp)?.0)
    }

    /// [`Headless::render`] plus what the frame did.
    pub fn render_stats(
        &mut self,
        canvas: &Canvas,
        live: &[&Stroke],
        vp: &Viewport,
    ) -> Result<(Image, crate::FrameStats), RenderError> {
        self.render_scene(&Scene {
            canvas,
            changes: None,
            live,
            viewport: *vp,
        })
    }

    /// One frame of a scene (tracked when it carries a change journal).
    pub fn render_scene(&mut self, scene: &Scene<'_>) -> Result<(Image, crate::FrameStats), RenderError> {
        self.render_scene_with(scene, &mut |_, _, _, _, _| {})
    }

    /// [`Headless::render_scene`] with an overlay pass encoded after the canvas (the app
    /// chrome in UI screenshots).
    pub fn render_scene_with(
        &mut self,
        scene: &Scene<'_>,
        overlay: &mut dyn FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
        ),
    ) -> Result<(Image, crate::FrameStats), RenderError> {
        let vp = &scene.viewport;
        vp.check()?;
        let t = self.target(vp.width, vp.height)?;
        let (view, tex, bpr) = (t.view.clone(), t.tex.clone(), t.bpr);
        let rb = self
            .readback
            .clone()
            .ok_or_else(|| RenderError::Readback("no readback buffer".into()))?;
        let mut enc = self.r.device.create_command_encoder(&Default::default());
        let stats = self.r.encode_scene(&mut enc, &view, HEADLESS_FORMAT, scene)?;
        overlay(&self.r.device, &self.r.queue, &mut enc, &view, HEADLESS_FORMAT);
        let img = self.finish(enc, &tex, &rb, bpr, vp)?;
        Ok((img, stats))
    }

    /// Renders until the whole canvas is drawn (or `max_frames`, then an error). For the CLI and
    /// tests that need complete pictures of large canvases.
    pub fn render_settled(
        &mut self,
        canvas: &Canvas,
        live: &[&Stroke],
        vp: &Viewport,
        max_frames: u32,
    ) -> Result<Image, RenderError> {
        let saved = self.r.budget;
        self.r.set_budget(crate::FrameBudget::UNLIMITED);
        let mut out = Err(RenderError::Viewport("did not settle".into()));
        for _ in 0..max_frames.max(1) {
            match self.render_stats(canvas, live, vp) {
                Ok((img, s)) if s.complete => {
                    out = Ok(img);
                    break;
                }
                Ok(_) => self.r.wait_lod(),
                Err(e) => {
                    out = Err(e);
                    break;
                }
            }
        }
        self.r.set_budget(saved);
        out
    }

    /// Test oracle: the stateless per-segment renderer (not for production use).
    #[doc(hidden)]
    pub fn render_reference(&mut self, canvas: &Canvas, vp: &Viewport) -> Result<Image, RenderError> {
        vp.check()?;
        let t = self.target(vp.width, vp.height)?;
        let (view, tex, bpr) = (t.view.clone(), t.tex.clone(), t.bpr);
        let rb = self
            .readback
            .clone()
            .ok_or_else(|| RenderError::Readback("no readback buffer".into()))?;
        let mut enc = self.r.device.create_command_encoder(&Default::default());
        self.r.encode_reference(&mut enc, &view, HEADLESS_FORMAT, canvas, vp)?;
        self.finish(enc, &tex, &rb, bpr, vp)
    }

    /// The cached target texture and readback buffer for this size.
    fn target(&mut self, w: u32, h: u32) -> Result<&Target, RenderError> {
        if !self.target.as_ref().is_some_and(|t| (t.w, t.h) == (w, h)) {
            let device = &self.r.device;
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("headless target"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: HEADLESS_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bpr = w
                .checked_mul(4)
                .and_then(|b| b.checked_add(255))
                .map(|b| b & !255)
                .ok_or_else(|| RenderError::Viewport("width".into()))?;
            let size = u64::from(bpr)
                .checked_mul(u64::from(h))
                .ok_or_else(|| RenderError::Viewport("size".into()))?;
            self.target = Some(Target { w, h, tex, view, bpr });
            if self.readback.as_ref().is_none_or(|b| b.size() < size) {
                self.prepare_readback(size)?;
            }
        }
        self.target
            .as_ref()
            .ok_or_else(|| RenderError::Viewport("target".into()))
    }

    /// Creates the readback buffer and touches its mapped memory once: the first read of fresh
    /// mapped memory costs ~5 ms for a 1920×1200 frame (page faults), which would otherwise land
    /// in the first frame.
    fn prepare_readback(&mut self, size: u64) -> Result<(), RenderError> {
        let rb = self.r.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let slice = rb.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.r
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        {
            let data = slice
                .get_mapped_range()
                .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
            let touched = data.iter().step_by(4096).fold(0u8, |a, b| a ^ b);
            std::hint::black_box(touched);
        }
        rb.unmap();
        self.readback = Some(rb);
        // Each frame copies the image into a new frame-sized `Vec`. glibc serves the first such
        // allocations of a process with fresh mmap'd memory (page faults every frame, ~1 ms at
        // 1920×1200) until a large block has been freed once; free one now, so frame images
        // come from already-touched heap memory.
        let prime = vec![1u8; usize::try_from(size.min(24 << 20)).unwrap_or(0)];
        std::hint::black_box(&prime);
        drop(prime);
        Ok(())
    }

    /// Copies the target to the readback buffer, submits, waits and reads the image.
    fn finish(
        &mut self,
        mut enc: wgpu::CommandEncoder,
        tex: &wgpu::Texture,
        rb: &wgpu::Buffer,
        bpr: u32,
        vp: &Viewport,
    ) -> Result<Image, RenderError> {
        let (w, h) = (vp.width, vp.height);
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: rb,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.r.queue.submit([enc.finish()]);
        let used = u64::from(bpr).saturating_mul(u64::from(h));
        let slice = rb.slice(..used);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.r
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let rgba = {
            let data = slice
                .get_mapped_range()
                .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
            let row = usize::try_from(w).unwrap_or(0).saturating_mul(4);
            let stride = usize::try_from(bpr).unwrap_or(0);
            if row == stride {
                data.to_vec()
            } else {
                let mut rgba = Vec::with_capacity(row.saturating_mul(usize::try_from(h).unwrap_or(0)));
                for chunk in data.chunks(stride) {
                    rgba.extend_from_slice(
                        chunk
                            .get(..row)
                            .ok_or_else(|| RenderError::Readback("short row".into()))?,
                    );
                }
                rgba
            }
        };
        rb.unmap();
        if let Some(f) = self.r.take_fault() {
            if matches!(f, GpuFault::Lost(_)) {
                self.r.invalidate();
            }
            return Err(RenderError::Device(format!("{f:?}")));
        }
        Ok(Image {
            width: w,
            height: h,
            rgba,
        })
    }
}

/// Ink segments for the canvas (strokes by z then id) plus live strokes on top.
fn segments(canvas: &Canvas, live: &[&Stroke], vp: &Viewport) -> Result<Vec<Seg>, RenderError> {
    let mut objs: Vec<_> = canvas.objects.values().collect();
    objs.sort_by(|a, b| a.z.cmp(&b.z).then(a.id.cmp(&b.id)));
    let mut segs: Vec<Seg> = Vec::new();
    let strokes = objs.iter().filter_map(|o| match &o.kind {
        ObjectKind::Stroke(s) => Some(s),
        _ => None, // other object kinds render in later milestones
    });
    for s in strokes.chain(live.iter().copied()) {
        push_stroke(&mut segs, s, vp)?;
    }
    Ok(segs)
}

/// Lossy by design: pixel positions/radii go to the GPU as f32 (relative to the viewport, so
/// magnitudes are small and precision is preserved far from the canvas origin).
#[allow(clippy::cast_possible_truncation)]
fn px(v: f64) -> f32 {
    v as f32
}

fn push_stroke(segs: &mut Vec<Seg>, s: &Stroke, vp: &Viewport) -> Result<(), RenderError> {
    let color = match s.tool {
        // Highlighter: translucent (per-stroke coverage pass is ADR 0003 open item).
        Tool::Highlighter => (s.color.0 & 0xffff_ff00) | 0x59,
        Tool::Pen => s.color.0,
    };
    let to_px = |x: f64, y: f64| [px((x - vp.origin_x) * vp.zoom), px((y - vp.origin_y) * vp.zoom)];
    let rad = |q: &vibenote_model::Sample| {
        px(f64::from(tool_width(s.tool, s.width, q.pressure, q.tilt_x, q.tilt_y)) * 0.5 * vp.zoom)
    };
    let mut prev: Option<&vibenote_model::Sample> = None;
    for smp in &s.samples {
        let a = prev.unwrap_or(smp);
        if segs.len() >= MAX_SEGMENTS {
            return Err(RenderError::Viewport("too many segments".into()));
        }
        segs.push(Seg {
            p0: to_px(a.x, a.y),
            p1: to_px(smp.x, smp.y),
            r: [rad(a), rad(smp)],
            color,
            _pad: 0,
        });
        prev = Some(smp);
    }
    Ok(())
}
