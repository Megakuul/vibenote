//! vello throughput lower bound: same synthetic strokes as `bench`, drawn as constant-width
//! polyline strokes (vello has no variable-width stroke; real ink would need outline fills).
//! Measures CPU scene encode + GPU render (wall time incl. wait) per frame at 1920x1200.
use vello::kurbo::{Affine, BezPath, Stroke};
use vello::peniko::{color::palette, Color};
use vello::{AaConfig, RenderParams, Renderer, RendererOptions, Scene};

struct Rng(u64);
impl Rng { fn f(&mut self) -> f64 { self.0 ^= self.0 >> 12; self.0 ^= self.0 << 25; self.0 ^= self.0 >> 27; (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64 } }

fn main() -> anyhow::Result<()> {
    let n: usize = std::env::var("STROKES").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let per_page = 44 * 12; let pages = n.div_ceil(per_page); let cols = (pages as f64).sqrt().ceil() as usize;
    let mut paths = vec![];
    for i in 0..n {
        let (pg, k) = (i / per_page, i % per_page); let (line, word) = (k / 12, k % 12);
        let ox = (pg % cols) as f64 * 1600.0 + 60.0 + word as f64 * 110.0; let oy = (pg / cols) as f64 * 2200.0 + 80.0 + line as f64 * 44.0;
        let samples = 40 + (r.f() * 80.0) as usize; let width = 60.0 + r.f() * 40.0; let loops = 3.0 + r.f() * 4.0; let ph1 = r.f() * 6.28; let _ = r.f();
        let mut p = BezPath::new();
        for s in 0..samples { let u = s as f64 / (samples - 1) as f64; let w = loops * std::f64::consts::TAU * u;
            let pt = (ox + width * u - 5.0 * w.sin(), oy - 9.0 * (1.0 - (w + ph1).cos()) - 6.0 * (u * 3.0).sin());
            if s == 0 { p.move_to(pt) } else { p.line_to(pt) } }
        paths.push(p);
    }
    let inst = vello::wgpu::Instance::new(vello::wgpu::InstanceDescriptor { backends: vello::wgpu::Backends::VULKAN, ..vello::wgpu::InstanceDescriptor::new_without_display_handle() });
    let a = pollster::block_on(inst.request_adapter(&Default::default()))?;
    let (dev, q) = pollster::block_on(a.request_device(&Default::default()))?;
    let mut ren = Renderer::new(&dev, RendererOptions { use_cpu: false, antialiasing_support: vello::AaSupport::area_only(), num_init_threads: None, pipeline_cache: None })?;
    let tex = dev.create_texture(&vello::wgpu::TextureDescriptor { label: None, size: vello::wgpu::Extent3d { width: 1920, height: 1200, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: vello::wgpu::TextureDimension::D2, format: vello::wgpu::TextureFormat::Rgba8Unorm, usage: vello::wgpu::TextureUsages::STORAGE_BINDING | vello::wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let view = tex.create_view(&Default::default());
    let bbs: Vec<vello::kurbo::Rect> = paths.iter().map(vello::kurbo::Shape::bounding_box).collect();
    let rows = pages.div_ceil(cols); let (bw, bh) = (cols as f64 * 1600.0, rows as f64 * 2200.0);
    let fit = (1920.0 / bw).min(1200.0 / bh) * 0.95;
    for (name, zoom, cx, cy) in [("page", 1.0, 700.0, 1000.0), ("overview", fit, bw / 2.0, bh / 2.0)] {
        let mut v = vec![]; let mut enc_v = vec![];
        for f in 0..60 {
            let t0 = std::time::Instant::now();
            let mut sc = Scene::new();
            let xf = Affine::translate((960.0, 600.0)) * Affine::scale(zoom) * Affine::translate((-cx - f as f64, -cy));
            let (hw, hh) = (960.0 / zoom, 600.0 / zoom);
            for (p, b) in paths.iter().zip(&bbs) {
                if b.x1 < cx - hw || b.x0 > cx + hw || b.y1 < cy - hh || b.y0 > cy + hh { continue; }
                sc.stroke(&Stroke::new(1.8), xf, Color::from_rgb8(30, 23, 20), None, p); }
            let te = t0.elapsed().as_secs_f64() * 1e3;
            ren.render_to_texture(&dev, &q, &sc, &view, &RenderParams { base_color: palette::css::WHITE, width: 1920, height: 1200, antialiasing_method: AaConfig::Area })?;
            let _ = dev.poll(vello::wgpu::PollType::wait_indefinitely());
            v.push(t0.elapsed().as_secs_f64() * 1e3); enc_v.push(te);
        }
        v.sort_by(|a, b| a.total_cmp(b)); enc_v.sort_by(|a, b| a.total_cmp(b));
        println!("vello {name}: frame (encode+render+wait) p50 {:.2} ms p95 {:.2} ms max {:.2}; scene encode p50 {:.2} ms", v[30], v[57], v[59], enc_v[30]);
    }
    Ok(())
}
