//! Phase 2 spike 2 — rendering at scale (headless, offscreen 1920x1200, GPU timestamps).
//!
//! Synthetic note: ~100k handwriting-like strokes (avg ~80 samples, pressure-varying width) on
//! ~190 page-sized blocks. Strategies:
//!   A  "direct": instanced SDF capsule segments, chunk culling (2048-unit grid, chunk-relative
//!      f32 coordinates; chunk origins f64 on CPU), LOD levels by decimation (tolerance 2^L·0.5).
//!   B  "tiles": 256 px tiles per power-of-two zoom level, rendered with A into an atlas,
//!      LRU cache, per-frame tile budget; composite = one textured quad per visible tile.
//! Usage: bench [A|B] [scenario|all] [--png out_prefix] [--far]

use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;

const W: u32 = 1920;
const H: u32 = 1200;
const CHUNK: f64 = 2048.0;
const LODS: usize = 7;
const TILE: u32 = 256;
const ATLAS: u32 = 4096; // 16x16 tiles per atlas page
const ATLAS_PAGES: u32 = 4; // 1024 tiles = 256 MB
const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Seg {
    p0: [f32; 2],
    p1: [f32; 2],
    r0: f32,
    r1: f32,
    chunk: u32,
    color: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct U {
    half: [f32; 2],
    zoom: f32,
    min_r_px: f32,
}

struct Stroke {
    pts: Vec<[f64; 3]>, // x, y, pressure
}

// ---------- synthetic data ----------
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn generate(n: usize, base: (f64, f64)) -> (Vec<Stroke>, (f64, f64, f64, f64)) {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let (pw, ph) = (1400.0, 2000.0);
    let per_page = 44 * 12;
    let pages = n.div_ceil(per_page);
    let cols = (pages as f64).sqrt().ceil() as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pg = i / per_page;
        let k = i % per_page;
        let (line, word) = (k / 12, k % 12);
        let ox = base.0 + (pg % cols) as f64 * (pw + 200.0) + 60.0 + word as f64 * 110.0;
        let oy = base.1 + (pg / cols) as f64 * (ph + 200.0) + 80.0 + line as f64 * 44.0;
        let samples = 40 + (r.f() * 80.0) as usize;
        let width = 60.0 + r.f() * 40.0;
        let loops = 3.0 + r.f() * 4.0;
        let ph1 = r.f() * 6.28;
        let pts = (0..samples)
            .map(|s| {
                let u = s as f64 / (samples - 1) as f64;
                let w = loops * std::f64::consts::TAU * u;
                let x = ox + width * u - 5.0 * w.sin();
                let y = oy - 9.0 * (1.0 - (w + ph1).cos()) - 6.0 * (u * 3.0).sin();
                [x, y, 0.35 + 0.4 * (u * 3.1 + ph1).sin().abs()]
            })
            .collect();
        out.push(Stroke { pts });
    }
    let rows = pages.div_ceil(cols);
    (out, (base.0, base.1, cols as f64 * (pw + 200.0), rows as f64 * (ph + 200.0)))
}

fn radius(p: f64) -> f32 {
    ((0.6 + 2.4 * p) * 0.5) as f32
}

// ---------- scene ----------
struct Chunk {
    origin: [f64; 2],
    bbox: [f64; 4],
    ranges: [(u32, u32); LODS],
}

struct Scene {
    chunks: Vec<Chunk>,
    bufs: Vec<wgpu::Buffer>, // per LOD
    counts: [usize; LODS],
    bounds: (f64, f64, f64, f64),
}

fn build_scene(dev: &wgpu::Device, strokes: &[Stroke], bounds: (f64, f64, f64, f64)) -> Scene {
    let mut map: HashMap<(i64, i64), usize> = HashMap::new();
    let mut chunks: Vec<Chunk> = vec![];
    let mut per: Vec<Vec<usize>> = vec![];
    for (i, s) in strokes.iter().enumerate() {
        let p = s.pts[0];
        let key = ((p[0] / CHUNK).floor() as i64, (p[1] / CHUNK).floor() as i64);
        let ci = *map.entry(key).or_insert_with(|| {
            chunks.push(Chunk {
                origin: [key.0 as f64 * CHUNK, key.1 as f64 * CHUNK],
                bbox: [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
                ranges: [(0, 0); LODS],
            });
            per.push(vec![]);
            chunks.len() - 1
        });
        per[ci].push(i);
        let c = &mut chunks[ci];
        for q in &s.pts {
            c.bbox[0] = c.bbox[0].min(q[0] - 4.0);
            c.bbox[1] = c.bbox[1].min(q[1] - 4.0);
            c.bbox[2] = c.bbox[2].max(q[0] + 4.0);
            c.bbox[3] = c.bbox[3].max(q[1] + 4.0);
        }
    }
    let mut bufs = vec![];
    let mut counts = [0usize; LODS];
    for l in 0..LODS {
        let tol = if l == 0 { 0.0 } else { 0.5 * (1u64 << l) as f64 };
        let mut segs: Vec<Seg> = vec![];
        for (ci, c) in chunks.iter_mut().enumerate() {
            let start = segs.len() as u32;
            for &si in &per[ci] {
                let s = &strokes[si];
                let mut last = s.pts[0];
                let rel = |q: [f64; 3]| [(q[0] - c.origin[0]) as f32, (q[1] - c.origin[1]) as f32];
                let n = s.pts.len();
                for (k, q) in s.pts.iter().enumerate().skip(1) {
                    let d = ((q[0] - last[0]).powi(2) + (q[1] - last[1]).powi(2)).sqrt();
                    if d < tol && k + 1 < n {
                        continue;
                    }
                    segs.push(Seg { p0: rel(last), p1: rel(*q), r0: radius(last[2]), r1: radius(q[2]), chunk: ci as u32, color: 0xff1e1714 });
                    last = *q;
                }
            }
            c.ranges[l] = (start, segs.len() as u32);
        }
        counts[l] = segs.len();
        use wgpu::util::DeviceExt;
        bufs.push(dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&segs),
            usage: wgpu::BufferUsages::VERTEX,
        }));
    }
    Scene { chunks, bufs, counts, bounds }
}

const SHADER: &str = r#"
struct U { half: vec2<f32>, zoom: f32, min_r_px: f32 };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> offs: array<vec2<f32>>;
struct VO {
  @builtin(position) pos: vec4<f32>,
  @location(0) p: vec2<f32>,
  @location(1) a: vec2<f32>,
  @location(2) b: vec2<f32>,
  @location(3) r: vec2<f32>,
  @location(4) color: vec4<f32>,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) p0: vec2<f32>, @location(1) p1: vec2<f32>,
              @location(2) rr: vec2<f32>, @location(3) chunk: u32, @location(4) col: u32) -> VO {
  let o = offs[chunk];
  let a = (p0 + o) * u.zoom + u.half;
  let b = (p1 + o) * u.zoom + u.half;
  let r = max(rr * u.zoom, vec2<f32>(u.min_r_px));
  let rm = max(r.x, r.y) + 1.0;
  var d = b - a;
  let l = length(d);
  var dir = vec2<f32>(1.0, 0.0);
  if (l > 1e-4) { dir = d / l; }
  let nrm = vec2<f32>(-dir.y, dir.x);
  // quad corners: (0,0) (1,0) (0,1) (1,1) as two triangles
  var cx = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
  var cy = array<f32, 6>(-1.0, -1.0, 1.0, 1.0, -1.0, 1.0);
  let t = cx[vi];
  let s = cy[vi];
  let along = mix(a - dir * rm, b + dir * rm, t);
  let pp = along + nrm * rm * s;
  var o2: VO;
  o2.pos = vec4<f32>(pp.x / u.half.x - 1.0, 1.0 - pp.y / u.half.y, 0.0, 1.0);
  o2.p = pp; o2.a = a; o2.b = b; o2.r = r;
  o2.color = unpack4x8unorm(col).zyxw;
  return o2;
}
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
  let ab = i.b - i.a;
  let l2 = dot(ab, ab);
  var t = 0.0;
  if (l2 > 1e-8) { t = clamp(dot(i.p - i.a, ab) / l2, 0.0, 1.0); }
  let q = i.a + ab * t;
  let d = length(i.p - q);
  let r = mix(i.r.x, i.r.y, t);
  let cov = clamp(r - d + 0.5, 0.0, 1.0);
  if (cov <= 0.0) { discard; }
  return vec4<f32>(i.color.rgb * cov, cov);
}
"#;

const BLIT: &str = r#"
struct T { rect: vec4<f32>, uv: vec4<f32>, layer: u32, _p: vec3<u32> };
@group(0) @binding(0) var tex: texture_2d_array<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<storage, read> tiles: array<T>;
@group(0) @binding(3) var<uniform> half: vec4<f32>;
struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) @interpolate(flat) layer: u32 };
@vertex fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VO {
  var cx = array<f32, 6>(0.0, 1.0, 0.0, 0.0, 1.0, 1.0);
  var cy = array<f32, 6>(0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
  let t = tiles[ii];
  let p = t.rect.xy + vec2<f32>(cx[vi], cy[vi]) * t.rect.zw;
  var o: VO;
  o.pos = vec4<f32>(p.x / half.x - 1.0, 1.0 - p.y / half.y, 0.0, 1.0);
  o.uv = t.uv.xy + vec2<f32>(cx[vi], cy[vi]) * t.uv.zw;
  o.layer = t.layer;
  return o;
}
@fragment fn fs(i: VO) -> @location(0) vec4<f32> { return textureSample(tex, smp, i.uv, i.layer); }
"#;

struct Gpu {
    dev: wgpu::Device,
    q: wgpu::Queue,
    pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    ubuf: wgpu::Buffer,
    obuf: wgpu::Buffer,
    qset: wgpu::QuerySet,
    qres: wgpu::Buffer,
    qread: wgpu::Buffer,
    period: f32,
}

fn mk_pipeline(dev: &wgpu::Device, bgl: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let m = dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
    let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
    dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &m,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Seg>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32, 4 => Uint32],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &m,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: FMT, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Gpu {
    fn new() -> anyhow::Result<Self> {
        let inst = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let a = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: None, force_fallback_adapter: false, apply_limit_buckets: false }))?;
        let mut lim = wgpu::Limits::default();
        lim.max_buffer_size = a.limits().max_buffer_size;
        lim.max_storage_buffer_binding_size = a.limits().max_storage_buffer_binding_size;
        let (dev, q) = pollster::block_on(a.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
            required_limits: lim,
            ..Default::default()
        }))?;
        let bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pipe = mk_pipeline(&dev, &bgl);
        let mkb = |size, usage| dev.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
        let ubuf = mkb(16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let obuf = mkb(8 * 65536, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let qset = dev.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
        let qres = mkb(16, wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
        let qread = mkb(16, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let period = q.get_timestamp_period();
        Ok(Gpu { dev, q, pipe, bgl, ubuf, obuf, qset, qres, qread, period })
    }

    fn gpu_ms(&self) -> f64 {
        let s = self.qread.slice(..);
        s.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.dev.poll(wgpu::PollType::wait_indefinitely());
        let v: Vec<u64> = s.get_mapped_range().map(|r| bytemuck::cast_slice::<u8, u64>(&r).to_vec()).unwrap_or_else(|_| vec![0, 0]);
        self.qread.unmap();
        (v[1].saturating_sub(v[0])) as f64 * self.period as f64 / 1e6
    }
}

#[derive(Clone, Copy)]
struct Cam {
    cx: f64,
    cy: f64,
    zoom: f64,
}

fn lod_for(zoom: f64) -> usize {
    // tolerance 0.5·2^L units ≤ ~0.5 px  →  2^L ≤ 1/zoom
    let bias: f64 = std::env::var("LOD_BIAS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let l = (1.0 / zoom).log2() + bias;
    if l <= 0.0 { 0 } else { (l.floor() as usize).min(LODS - 1) }
}

/// Strategy A: draw visible chunks into `view` (size w×h) for camera `cam`. Returns instances.
fn draw_direct(g: &Gpu, sc: &Scene, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, cam: Cam, w: u32, h: u32, clear: bool, viewport: Option<[f32; 4]>) -> u64 {
    let lod = lod_for(cam.zoom);
    let (hw, hh) = (w as f64 / 2.0 / cam.zoom, h as f64 / 2.0 / cam.zoom);
    let vr = [cam.cx - hw, cam.cy - hh, cam.cx + hw, cam.cy + hh];
    let offs: Vec<[f32; 2]> = sc.chunks.iter().map(|c| [(c.origin[0] - cam.cx) as f32, (c.origin[1] - cam.cy) as f32]).collect();
    g.q.write_buffer(&g.obuf, 0, bytemuck::cast_slice(&offs));
    let u = U { half: [w as f32 / 2.0, h as f32 / 2.0], zoom: cam.zoom as f32, min_r_px: 0.35 };
    g.q.write_buffer(&g.ubuf, 0, bytemuck::bytes_of(&u));
    let bg = g.dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: g.ubuf.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: g.obuf.as_entire_binding() }],
    });
    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: if clear { wgpu::LoadOp::Clear(wgpu::Color { r: 0.984, g: 0.98, b: 0.965, a: 1.0 }) } else { wgpu::LoadOp::Load }, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    if let Some(v) = viewport {
        rp.set_viewport(v[0], v[1], v[2], v[3], 0.0, 1.0);
        rp.set_scissor_rect(v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32);
    }
    rp.set_pipeline(&g.pipe);
    rp.set_bind_group(0, &bg, &[]);
    rp.set_vertex_buffer(0, sc.bufs[lod].slice(..));
    let mut n = 0u64;
    for c in &sc.chunks {
        if c.bbox[2] < vr[0] || c.bbox[0] > vr[2] || c.bbox[3] < vr[1] || c.bbox[1] > vr[3] {
            continue;
        }
        let (a, b) = c.ranges[lod];
        if b > a {
            rp.draw(0..6, a..b);
            n += (b - a) as u64;
        }
    }
    n
}

// ---------- Strategy B: tiles ----------
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileInst {
    rect: [f32; 4],
    uv: [f32; 4],
    layer: u32,
    _p: [u32; 3],
}

struct Tiles {
    atlas: wgpu::Texture,
    layer_views: Vec<wgpu::TextureView>,
    blit: wgpu::RenderPipeline,
    blit_bg: wgpu::BindGroup,
    tbuf: wgpu::Buffer,
    half: wgpu::Buffer,
    map: HashMap<(i32, i64, i64), (u32, u64)>, // key -> (slot, last_used)
    free: Vec<u32>,
    frame: u64,
    budget: usize,
}

impl Tiles {
    fn new(g: &Gpu, budget: usize) -> Self {
        let atlas = g.dev.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: ATLAS_PAGES },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FMT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let layer_views = (0..ATLAS_PAGES)
            .map(|l| atlas.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: l, array_layer_count: Some(1), ..Default::default() }))
            .collect();
        let arr = atlas.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let smp = g.dev.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let tbuf = g.dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 48 * 4096, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let half = g.dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        g.q.write_buffer(&half, 0, bytemuck::cast_slice(&[W as f32 / 2.0, H as f32 / 2.0, 0.0, 0.0]));
        let m = g.dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(BLIT.into()) });
        let blit = g.dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState { module: &m, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState { module: &m, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: FMT, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let blit_bg = g.dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &blit.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&arr) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&smp) },
                wgpu::BindGroupEntry { binding: 2, resource: tbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: half.as_entire_binding() },
            ],
        });
        let per = (ATLAS / TILE) * (ATLAS / TILE);
        Tiles { atlas, layer_views, blit, blit_bg, tbuf, half, map: HashMap::new(), free: (0..per * ATLAS_PAGES).rev().collect(), frame: 0, budget }
    }

    fn slot_rect(slot: u32) -> (u32, u32, u32) {
        let per_row = ATLAS / TILE;
        let per = per_row * per_row;
        let layer = slot / per;
        let i = slot % per;
        (layer, (i % per_row) * TILE, (i / per_row) * TILE)
    }

    /// Returns (tiles rendered, instances drawn into tiles, tiles composited, missing).
    fn frame(&mut self, g: &Gpu, sc: &Scene, enc: &mut wgpu::CommandEncoder, out: &wgpu::TextureView, cam: Cam) -> (usize, u64, usize, usize) {
        self.frame += 1;
        let k = cam.zoom.log2().ceil() as i32; // render level ≥ display zoom
        let lz = 2f64.powi(k);
        let tile_units = TILE as f64 / lz;
        let (hw, hh) = (W as f64 / 2.0 / cam.zoom, H as f64 / 2.0 / cam.zoom);
        let (x0, y0, x1, y1) = (cam.cx - hw, cam.cy - hh, cam.cx + hw, cam.cy + hh);
        let (tx0, ty0, tx1, ty1) = ((x0 / tile_units).floor() as i64, (y0 / tile_units).floor() as i64, (x1 / tile_units).floor() as i64, (y1 / tile_units).floor() as i64);
        let mut rendered = 0;
        let mut inst = 0;
        let mut quads: Vec<TileInst> = vec![];
        let mut missing = 0;
        // Pass 1: render missing tiles (budgeted), nearest to centre first.
        let mut need: Vec<(i64, i64)> = vec![];
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                if !self.map.contains_key(&(k, tx, ty)) {
                    need.push((tx, ty));
                }
            }
        }
        let (ccx, ccy) = (cam.cx / tile_units, cam.cy / tile_units);
        need.sort_by(|a, b| ((a.0 as f64 - ccx).powi(2) + (a.1 as f64 - ccy).powi(2)).total_cmp(&((b.0 as f64 - ccx).powi(2) + (b.1 as f64 - ccy).powi(2))));
        for &(tx, ty) in need.iter().take(self.budget) {
            let slot = match self.free.pop() {
                Some(s) => s,
                None => {
                    // evict LRU
                    let Some((&key, &(s, _))) = self.map.iter().filter(|(_, v)| v.1 < self.frame).min_by_key(|(_, v)| v.1) else { break };
                    self.map.remove(&key);
                    s
                }
            };
            let (layer, px, py) = Self::slot_rect(slot);
            let tcam = Cam { cx: (tx as f64 + 0.5) * tile_units, cy: (ty as f64 + 0.5) * tile_units, zoom: lz };
            // clear + draw into the tile's region of the atlas page (viewport/scissor)
            inst += draw_tile(g, sc, enc, &self.layer_views[layer as usize], tcam, [px as f32, py as f32, TILE as f32, TILE as f32]);
            self.map.insert((k, tx, ty), (slot, self.frame));
            rendered += 1;
        }
        // Pass 2: composite (fallback to coarser cached level if missing)
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let mut found = None;
                for dk in 0..4 {
                    let kk = k - dk;
                    let s = 1i64 << dk;
                    let key = (kk, tx.div_euclid(s), ty.div_euclid(s));
                    if let Some(v) = self.map.get_mut(&key) {
                        v.1 = self.frame;
                        found = Some((v.0, dk, s));
                        break;
                    }
                }
                let Some((slot, dk, s)) = found else {
                    missing += 1;
                    continue;
                };
                let (layer, px, py) = Self::slot_rect(slot);
                let sub = 1.0 / s as f32;
                let (fx, fy) = (tx.rem_euclid(s) as f32 * sub, ty.rem_euclid(s) as f32 * sub);
                let sx = ((tx as f64 * tile_units - x0) * cam.zoom) as f32;
                let sy = ((ty as f64 * tile_units - y0) * cam.zoom) as f32;
                let sz = (tile_units * cam.zoom) as f32;
                let a = ATLAS as f32;
                let _ = dk;
                quads.push(TileInst {
                    rect: [sx, sy, sz, sz],
                    uv: [(px as f32 + fx * TILE as f32) / a, (py as f32 + fy * TILE as f32) / a, TILE as f32 * sub / a, TILE as f32 * sub / a],
                    layer,
                    _p: [0; 3],
                });
            }
        }
        g.q.write_buffer(&self.tbuf, 0, bytemuck::cast_slice(&quads));
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: out,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.984, g: 0.98, b: 0.965, a: 1.0 }), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(&self.blit);
        rp.set_bind_group(0, &self.blit_bg, &[]);
        rp.draw(0..6, 0..quads.len() as u32);
        let _ = (&self.atlas, &self.half);
        (rendered, inst, quads.len(), missing)
    }
}

/// Draw one tile: separate uniform/offset buffers per tile so multiple tiles can be encoded
/// in one command buffer.
fn draw_tile(g: &Gpu, sc: &Scene, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, cam: Cam, vp: [f32; 4]) -> u64 {
    let lod = lod_for(cam.zoom);
    let (hw, hh) = (TILE as f64 / 2.0 / cam.zoom, TILE as f64 / 2.0 / cam.zoom);
    let vr = [cam.cx - hw, cam.cy - hh, cam.cx + hw, cam.cy + hh];
    let offs: Vec<[f32; 2]> = sc.chunks.iter().map(|c| [(c.origin[0] - cam.cx) as f32, (c.origin[1] - cam.cy) as f32]).collect();
    use wgpu::util::DeviceExt;
    let ob = g.dev.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&offs), usage: wgpu::BufferUsages::STORAGE });
    let u = U { half: [TILE as f32 / 2.0, TILE as f32 / 2.0], zoom: cam.zoom as f32, min_r_px: 0.35 };
    let ub = g.dev.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::bytes_of(&u), usage: wgpu::BufferUsages::UNIFORM });
    let bg = g.dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: ob.as_entire_binding() }],
    });
    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    rp.set_viewport(vp[0], vp[1], vp[2], vp[3], 0.0, 1.0);
    rp.set_scissor_rect(vp[0] as u32, vp[1] as u32, vp[2] as u32, vp[3] as u32);
    // clear tile region: draw nothing special — use a clear via a full-tile paper quad is
    // omitted; tiles start transparent in a fresh slot and the composite shows paper behind.
    rp.set_pipeline(&g.pipe);
    rp.set_bind_group(0, &bg, &[]);
    rp.set_vertex_buffer(0, sc.bufs[lod].slice(..));
    let mut n = 0;
    for c in &sc.chunks {
        if c.bbox[2] < vr[0] || c.bbox[0] > vr[2] || c.bbox[3] < vr[1] || c.bbox[1] > vr[3] {
            continue;
        }
        let (a, b) = c.ranges[lod];
        if b > a {
            rp.draw(0..6, a..b);
            n += (b - a) as u64;
        }
    }
    n
}

// ---------- scenarios ----------
fn scenario(name: &str, b: (f64, f64, f64, f64), f: usize) -> Cam {
    let (bx, by, bw, bh) = b;
    let page = (bx + 700.0, by + 1000.0);
    let fit = (W as f64 / bw).min(H as f64 / bh) * 0.95;
    let t = f as f64;
    match name {
        "page_pan" => Cam { cx: page.0 + t * 8.0, cy: page.1, zoom: 1.0 },
        "page_fling" => Cam { cx: page.0 + t * 60.0, cy: page.1 + t * 10.0, zoom: 1.0 },
        "deep" => Cam { cx: page.0 + t * 0.5, cy: page.1, zoom: 16.0 },
        "overview" => Cam { cx: bx + bw / 2.0 + t * 2.0 / fit, cy: by + bh / 2.0, zoom: fit },
        "zoom_sweep" => {
            let u = if f < 150 { f as f64 / 149.0 } else { (299 - f) as f64 / 149.0 };
            let z = (fit.ln() * u).exp(); // 1 → fit → 1
            let c = (page.0 + (bx + bw / 2.0 - page.0) * u, page.1 + (by + bh / 2.0 - page.1) * u);
            Cam { cx: c.0, cy: c.1, zoom: z }
        }
        _ => Cam { cx: page.0, cy: page.1, zoom: 1.0 },
    }
}

fn save_png(g: &Gpu, tex: &wgpu::Texture, path: &str) -> anyhow::Result<()> {
    let bpr = W * 4;
    let buf = g.dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: (bpr * H) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = g.dev.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(tex.as_image_copy(), wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(H) } }, wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 });
    g.q.submit([enc.finish()]);
    let s = buf.slice(..);
    s.map_async(wgpu::MapMode::Read, |_| {});
    let _ = g.dev.poll(wgpu::PollType::wait_indefinitely());
    let data = s.get_mapped_range().map_err(|e| anyhow::anyhow!("{e:?}"))?.to_vec();
    let f = std::fs::File::create(path)?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), W, H);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(&data)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let strat = args.get(1).map(String::as_str).unwrap_or("A").to_string();
    let which = args.get(2).map(String::as_str).unwrap_or("all").to_string();
    let png = args.iter().position(|a| a == "--png").and_then(|i| args.get(i + 1)).cloned();
    let far = args.iter().any(|a| a == "--far");
    let n: usize = std::env::var("STROKES").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let g = Gpu::new()?;
    let t0 = std::time::Instant::now();
    let base = if far { (1.0e7, -1.0e7) } else { (0.0, 0.0) };
    let (strokes, bounds) = generate(n, base);
    let samples: usize = strokes.iter().map(|s| s.pts.len()).sum();
    let gen_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = std::time::Instant::now();
    let sc = build_scene(&g.dev, &strokes, bounds);
    let build_ms = t1.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "strokes {n} samples {samples} chunks {} segs/LOD {:?} gen {gen_ms:.0} ms build {build_ms:.0} ms gpu-mem L0 {:.0} MB all {:.0} MB",
        sc.chunks.len(),
        sc.counts,
        sc.counts[0] as f64 * 32.0 / 1e6,
        sc.counts.iter().sum::<usize>() as f64 * 32.0 / 1e6
    );
    let target = g.dev.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FMT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let scenarios: Vec<&str> = if which == "all" { vec!["page_pan", "page_fling", "deep", "overview", "zoom_sweep"] } else { vec![which.as_str()] };
    let mut tiles = if strat == "B" { Some(Tiles::new(&g, std::env::var("TILE_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(16))) } else { None };
    for s in scenarios {
        let mut gpu = vec![];
        let mut cpu = vec![];
        let mut inst_v = vec![];
        let mut extra = vec![];
        for f in 0..300 {
            let cam = scenario(s, sc.bounds, f);
            let tc = std::time::Instant::now();
            let mut enc = g.dev.create_command_encoder(&Default::default());
            enc.write_timestamp(&g.qset, 0);
            let inst = if let Some(t) = tiles.as_mut() {
                let (r, i, q, m) = t.frame(&g, &sc, &mut enc, &view, cam);
                extra.push((r, q, m));
                i
            } else {
                draw_direct(&g, &sc, &mut enc, &view, cam, W, H, true, None)
            };
            enc.write_timestamp(&g.qset, 1);
            enc.resolve_query_set(&g.qset, 0..2, &g.qres, 0);
            enc.copy_buffer_to_buffer(&g.qres, 0, &g.qread, 0, 16);
            g.q.submit([enc.finish()]);
            cpu.push(tc.elapsed().as_secs_f64() * 1e3);
            gpu.push(g.gpu_ms());
            inst_v.push(inst);
            if f == 0 || f == 150 {
                if let Some(p) = &png {
                    save_png(&g, &target, &format!("{p}-{strat}-{s}-{f}.png"))?;
                }
            }
        }
        let q = |v: &mut Vec<f64>, p: f64| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[((v.len() - 1) as f64 * p).round() as usize]
        };
        let over = gpu.iter().zip(&cpu).filter(|(a, b)| a.max(**b) > 5.555).count();
        let mut g2 = gpu.clone();
        let mut c2 = cpu.clone();
        let tr: usize = extra.iter().map(|e| e.0).sum();
        let miss: usize = extra.iter().map(|e| e.2).sum();
        let maxr = extra.iter().map(|e| e.0).max().unwrap_or(0);
        let inst_p50 = { let mut v = inst_v.clone(); v.sort(); v[v.len() / 2] };
        println!(
            "{}",
            serde_json::json!({"strategy": strat, "scenario": s, "strokes": n,
            "gpu_ms": {"p50": q(&mut g2, 0.5), "p95": q(&mut g2, 0.95), "p99": q(&mut g2, 0.99), "max": q(&mut g2, 1.0)},
            "cpu_ms": {"p50": q(&mut c2, 0.5), "p99": q(&mut c2, 0.99)},
            "frames_over_5_555ms": over, "instances_p50": inst_p50, "instances_max": inst_v.iter().max(),
            "tiles_rendered_total": tr, "tiles_rendered_max_per_frame": maxr, "tile_missing_total": miss})
        );
    }
    Ok(())
}
