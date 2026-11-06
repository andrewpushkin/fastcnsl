//! wgpu renderer. A frame is a list of instanced quads in six layers
//! (cell backgrounds, text, UI shapes and text, overlay shapes and text),
//! drawn in that order with one draw call.
//!
//! The layers are reusable `Vec`s that keep their capacity between frames,
//! and each one is written into the GPU buffer straight from its slice, so
//! building a frame allocates nothing in steady state.

pub mod glyphs;

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use winit::window::Window;

use crate::theme::Rgb;
use glyphs::{CellMetrics, GlyphCache, GlyphStyle};

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Instance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub kind: u32,
    pub radius: f32,
}

const KIND_RECT: u32 = 0;
const KIND_MASK_GLYPH: u32 = 1;
const KIND_COLOR_GLYPH: u32 = 2;
const KIND_ROUNDED: u32 = 3;

#[derive(Clone, Copy)]
pub enum Layer {
    Background = 0,
    Text = 1,
    /// Tab bar, scrollbar, search bar.
    UiShapes = 2,
    UiText = 3,
    /// Menus and panels, above everything else.
    OverlayShapes = 4,
    OverlayText = 5,
}

pub(crate) const LAYERS: usize = 6;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    atlas: [f32; 2],
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    layers: [Vec<Instance>; LAYERS],
    pub glyphs: GlyphCache,
    /// Adapter name and backend, for the About panel.
    pub gpu: String,
}

/// Draws into the frame's layers. Borrowed from the renderer for the
/// duration of one frame.
pub struct Painter<'a> {
    layers: &'a mut [Vec<Instance>; LAYERS],
    glyphs: &'a mut GlyphCache,
    queue: &'a wgpu::Queue,
    pub cell: CellMetrics,
    pub width: f32,
    pub height: f32,
}

impl<'a> Painter<'a> {
    pub(crate) fn new(
        layers: &'a mut [Vec<Instance>; LAYERS],
        glyphs: &'a mut GlyphCache,
        queue: &'a wgpu::Queue,
        width: f32,
        height: f32,
    ) -> Self {
        let cell = glyphs.metrics;
        Self { layers, glyphs, queue, cell, width, height }
    }

    pub fn rect(&mut self, layer: Layer, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.layers[layer as usize].push(Instance {
            pos: [x, y],
            size: [w, h],
            uv: [0.0; 4],
            color,
            kind: KIND_RECT,
            radius: 0.0,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rounded(&mut self, layer: Layer, x: f32, y: f32, w: f32, h: f32, r: f32, color: [f32; 4]) {
        self.layers[layer as usize].push(Instance {
            pos: [x, y],
            size: [w, h],
            uv: [0.0; 4],
            color,
            kind: KIND_ROUNDED,
            radius: r,
        });
    }

    /// Draws `c` with its pen at (x, top-of-cell y).
    pub fn glyph(&mut self, layer: Layer, c: char, style: GlyphStyle, x: f32, y: f32, color: [f32; 4]) {
        let Some(g) = self.glyphs.get(self.queue, c, style) else { return };
        self.layers[layer as usize].push(Instance {
            pos: [x + g.left, y + self.cell.baseline - g.top],
            size: [g.uv[2], g.uv[3]],
            uv: g.uv,
            color,
            kind: if g.colored { KIND_COLOR_GLYPH } else { KIND_MASK_GLYPH },
            radius: 0.0,
        });
    }

    /// Draws `c` with its ink centred on (cx, cy), for icons. Text glyphs
    /// sit on a baseline, which puts symbols like × visibly low in a button.
    pub fn icon(&mut self, layer: Layer, c: char, cx: f32, cy: f32, color: [f32; 4]) {
        let Some(g) = self.glyphs.get(self.queue, c, GlyphStyle::REGULAR) else { return };
        let (w, h) = (g.uv[2], g.uv[3]);
        self.layers[layer as usize].push(Instance {
            pos: [(cx - w / 2.0).round(), (cy - h / 2.0).round()],
            size: [w, h],
            uv: g.uv,
            color,
            kind: if g.colored { KIND_COLOR_GLYPH } else { KIND_MASK_GLYPH },
            radius: 0.0,
        });
    }

    /// Draws a string on the cell grid starting at x; returns the end x.
    pub fn text(&mut self, layer: Layer, s: &str, x: f32, y: f32, color: [f32; 4]) -> f32 {
        let mut x = x;
        for c in s.chars() {
            self.glyph(layer, c, GlyphStyle::REGULAR, x, y, color);
            x += self.cell.width * crate::grid_width(c) as f32;
        }
        x
    }
}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        display: winit::event_loop::OwnedDisplayHandle,
        font_family: &str, font_px: f32, line_height: f32) -> Result<Self, String> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display)));
        let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        log::info!("GPU: {info:?}");
        let gpu = format!("{} ({:?})", info.name, info.backend);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ragamuffin"),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;

        let caps = surface.get_capabilities(&adapter);
        // A non-sRGB surface keeps terminal colours exactly as specified.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let glyphs = GlyphCache::new(&device, font_family, font_px, line_height);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_view = glyphs.texture().create_view(&wgpu::TextureViewDescriptor::default());

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layout"),
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4, 4 => Uint32, 5 => Float32
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_capacity = 16 * 1024;
        let instances = Self::make_instance_buffer(&device, instance_capacity);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            globals,
            instances,
            instance_capacity,
            layers: Default::default(),
            glyphs,
            gpu,
        })
    }

    fn make_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn cell(&self) -> CellMetrics {
        self.glyphs.metrics
    }

    /// True if the glyph atlas overflowed while building the last frame,
    /// in which case some glyphs were dropped and the frame should be rebuilt.
    pub fn needs_rebuild(&self) -> bool {
        self.glyphs.overflowed
    }

    /// Fills the frame's layers. Kept separate from [`present`](Self::present)
    /// so callers can hold the terminal lock only while building: presenting
    /// may block on vsync, and the PTY thread must not wait for that.
    pub fn build(&mut self, build: impl FnOnce(&mut Painter<'_>)) {
        for l in &mut self.layers {
            l.clear();
        }
        self.glyphs.overflowed = false;
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let mut painter = Painter::new(&mut self.layers, &mut self.glyphs, &self.queue, w, h);
        build(&mut painter);
    }

    /// Uploads the built layers and presents the frame.
    pub fn present(&mut self, clear: Rgb) {
        let total: usize = self.layers.iter().map(Vec::len).sum();
        if total > self.instance_capacity {
            self.instance_capacity = total.next_power_of_two();
            self.instances = Self::make_instance_buffer(&self.device, self.instance_capacity);
        }
        let mut offset = 0u64;
        for l in &self.layers {
            if !l.is_empty() {
                self.queue.write_buffer(&self.instances, offset, bytemuck::cast_slice(l));
                offset += std::mem::size_of_val(l.as_slice()) as u64;
            }
        }
        let globals = Globals {
            screen: [self.config.width as f32, self.config.height as f32],
            atlas: [glyphs::ATLAS_SIZE as f32; 2],
        };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                log::debug!("skipping frame: {other:?}");
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let c = clear.rgba(1.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c[0] as f64,
                            g: c[1] as f64,
                            b: c[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if total > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.draw(0..4, 0..total as u32);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
    }
}

/// Test support: a GPU device without a window, and a painter over it.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A headless device, or None where no adapter exists (tests then skip).
    pub fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .ok()
    }

    /// Everything a test needs to paint a frame and inspect the quads.
    pub struct Canvas {
        /// Kept alive for the queue and atlas texture.
        _device: wgpu::Device,
        pub queue: wgpu::Queue,
        pub glyphs: GlyphCache,
        pub layers: [Vec<Instance>; LAYERS],
    }

    impl Canvas {
        pub fn new() -> Option<Self> {
            let Some((device, queue)) = device() else {
                eprintln!("no GPU adapter: skipping GPU test");
                return None;
            };
            let glyphs = GlyphCache::new(&device, "DejaVu Sans Mono", 16.0, 1.2);
            Some(Self { _device: device, queue, glyphs, layers: Default::default() })
        }

        pub fn paint(&mut self, width: f32, height: f32, f: impl FnOnce(&mut Painter<'_>)) {
            for l in &mut self.layers {
                l.clear();
            }
            let mut p = Painter::new(&mut self.layers, &mut self.glyphs, &self.queue, width, height);
            f(&mut p);
        }

        pub fn layer(&self, l: Layer) -> &[Instance] {
            &self.layers[l as usize]
        }

        pub fn glyph_count(&self, l: Layer) -> usize {
            self.layer(l).iter().filter(|i| i.kind == KIND_MASK_GLYPH || i.kind == KIND_COLOR_GLYPH).count()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Canvas;
    use super::*;

    #[test]
    fn painter_writes_each_shape_to_its_layer() {
        let Some(mut c) = Canvas::new() else { return };
        c.paint(800.0, 600.0, |p| {
            p.rect(Layer::Background, 1.0, 2.0, 3.0, 4.0, [1.0; 4]);
            p.rounded(Layer::UiShapes, 0.0, 0.0, 10.0, 10.0, 3.0, [1.0; 4]);
            let end = p.text(Layer::OverlayText, "ab c", 10.0, 0.0, [1.0; 4]);
            assert_eq!(end, 10.0 + 4.0 * p.cell.width);
        });
        let bg = c.layer(Layer::Background);
        assert_eq!(bg.len(), 1);
        assert_eq!((bg[0].pos, bg[0].size, bg[0].kind), ([1.0, 2.0], [3.0, 4.0], KIND_RECT));
        assert_eq!(c.layer(Layer::UiShapes)[0].kind, KIND_ROUNDED);
        assert_eq!(c.layer(Layer::UiShapes)[0].radius, 3.0);
        // Three visible glyphs; the space draws nothing.
        assert_eq!(c.glyph_count(Layer::OverlayText), 3);
        assert!(c.layer(Layer::Text).is_empty());
    }

    #[test]
    fn icons_are_centred_on_their_ink() {
        let Some(mut c) = Canvas::new() else { return };
        c.paint(100.0, 100.0, |p| p.icon(Layer::UiText, '×', 50.0, 40.0, [1.0; 4]));
        let i = c.layer(Layer::UiText)[0];
        let (mx, my) = (i.pos[0] + i.size[0] / 2.0, i.pos[1] + i.size[1] / 2.0);
        assert!((mx - 50.0).abs() <= 1.0 && (my - 40.0).abs() <= 1.0, "centre at ({mx}, {my})");
    }

    #[test]
    fn instance_layout_matches_the_shader() {
        // pos, size (2+2), uv (4), color (4), kind, radius = 14 × 4 bytes.
        assert_eq!(std::mem::size_of::<Instance>(), 56);
    }
}
