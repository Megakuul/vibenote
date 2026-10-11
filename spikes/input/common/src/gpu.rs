//! Minimal wgpu ink renderer shared by variants (b) and (c).
//!
//! Committed geometry is appended to a growing GPU vertex buffer (incremental upload);
//! transient geometry (prediction) goes to a small per-frame buffer. Every frame clears and
//! redraws all vertices (GPU cost is negligible at spike scale; measured separately in Phase 2).

use crate::ink::{Vertex, PAPER};

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vbuf: wgpu::Buffer,
    vcap: u64,
    vlen: u64,
    tbuf: wgpu::Buffer,
    pub adapter_info: wgpu::AdapterInfo,
}

const SHADER: &str = r#"
struct U { size: vec2<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct VO { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VO {
  var o: VO;
  o.pos = vec4<f32>(p.x / u.size.x * 2.0 - 1.0, 1.0 - p.y / u.size.y * 2.0, 0.0, 1.0);
  o.color = c;
  return o;
}
@fragment fn fs(i: VO) -> @location(0) vec4<f32> { return i.color; }
"#;

pub fn present_mode_from_env() -> wgpu::PresentMode {
    match std::env::var("INK_PRESENT").as_deref() {
        Ok("mailbox") => wgpu::PresentMode::Mailbox,
        Ok("immediate") => wgpu::PresentMode::Immediate,
        Ok("fifo_relaxed") => wgpu::PresentMode::FifoRelaxed,
        _ => wgpu::PresentMode::Fifo,
    }
}

impl Renderer {
    pub fn new(instance: &wgpu::Instance, surface: wgpu::Surface<'static>, w: u32, h: u32) -> anyhow::Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let mut config = surface
            .get_default_config(&adapter, w.max(1), h.max(1))
            .ok_or_else(|| anyhow::anyhow!("surface unsupported"))?;
        let caps = surface.get_capabilities(&adapter);
        let want = present_mode_from_env();
        config.present_mode = if caps.present_modes.contains(&want) { want } else { wgpu::PresentMode::Fifo };
        config.desired_maximum_frame_latency = crate::env_or("INK_FRAME_LATENCY", 1u32);
        // Prefer plain 8-bit BGRA/RGBA (no HDR/float formats).
        for want in [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm] {
            if caps.formats.contains(&want) {
                config.format = want;
                break;
            }
        }
        surface.configure(&device, &config);

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let vcap = 1 << 20;
        let mk = |size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let vbuf = mk(vcap);
        let tbuf = mk(1 << 16);
        Ok(Renderer { device, queue, surface, config, pipeline, uniform, bind, vbuf, vcap, vlen: 0, tbuf, adapter_info })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w == self.config.width && h == self.config.height) {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }

    /// Appends committed vertices (uploaded once).
    pub fn append(&mut self, v: &[Vertex]) {
        let bytes: &[u8] = bytemuck::cast_slice(v);
        let need = self.vlen + bytes.len() as u64;
        if need > self.vcap {
            let ncap = need.next_power_of_two();
            let nb = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: ncap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let mut enc = self.device.create_command_encoder(&Default::default());
            enc.copy_buffer_to_buffer(&self.vbuf, 0, &nb, 0, self.vlen);
            self.queue.submit([enc.finish()]);
            self.vbuf = nb;
            self.vcap = ncap;
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.vbuf, self.vlen, bytes);
            self.vlen = need;
        }
    }

    /// Acquire, draw everything (+ transient vertices), present. Returns false if no frame.
    /// `before_present` runs right before `present()` (e.g. to request presentation feedback).
    pub fn render(&mut self, transient: &[Vertex], before_present: impl FnOnce()) -> bool {
        self.render_ex(transient, |_, _, _, _| {}, before_present)
    }

    /// Like `render`, but `extra` can record further passes (LoadOp::Load) on top of the ink.
    pub fn render_ex(
        &mut self,
        transient: &[Vertex],
        extra: impl FnOnce(&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder, &wgpu::TextureView),
        before_present: impl FnOnce(),
    ) -> bool {
        let st = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
        };
        let size = [self.config.width as f32, self.config.height as f32, 0.0, 0.0];
        self.queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&size));
        let tbytes: &[u8] = bytemuck::cast_slice(transient);
        let tlen = tbytes.len().min(1 << 16) as u64;
        if tlen > 0 {
            self.queue.write_buffer(&self.tbuf, 0, &tbytes[..tlen as usize]);
        }
        let view = st.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let c = PAPER;
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &self.bind, &[]);
            let vs = std::mem::size_of::<Vertex>() as u64;
            if self.vlen > 0 {
                rp.set_vertex_buffer(0, self.vbuf.slice(0..self.vlen));
                rp.draw(0..(self.vlen / vs) as u32, 0..1);
            }
            if tlen > 0 {
                rp.set_vertex_buffer(0, self.tbuf.slice(0..tlen));
                rp.draw(0..(tlen / vs) as u32, 0..1);
            }
        }
        extra(&self.device, &self.queue, &mut enc, &view);
        self.queue.submit([enc.finish()]);
        before_present();
        self.queue.present(st);
        true
    }
}
