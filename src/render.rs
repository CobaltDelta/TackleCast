#![allow(clippy::items_after_test_module)]

use crate::capture::{CaptureFrame, PixelFormat};
use crate::ui::PreparedUi;
use bytemuck::{Pod, Zeroable};
use egui_wgpu::Renderer as EguiRenderer;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use wgpu::{CompositeAlphaMode, PresentMode, SurfaceConfiguration, TextureUsages};
use winit::dpi::PhysicalSize;
use winit::window::Window;

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 10.0 / 255.0,
    g: 10.0 / 255.0,
    b: 20.0 / 255.0,
    a: 1.0,
};

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    size: PhysicalSize<u32>,
    video_pipeline: wgpu::RenderPipeline,
    video_bind_group_layout: wgpu::BindGroupLayout,
    video_sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    video_frame: Option<VideoFrameResources>,
    egui_renderer: EguiRenderer,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, RenderError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(RenderError::CreateSurface)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RenderError::AdapterUnavailable)?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tacklecast-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            }, None)
            .await
            .map_err(RenderError::RequestDevice)?;

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|format| *format == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.first().copied())
            .ok_or(RenderError::SurfaceFormatUnavailable)?;

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: caps
                .present_modes
                .iter()
                .copied()
                .find(|mode| *mode == PresentMode::AutoVsync)
                .unwrap_or(PresentMode::Fifo),
            alpha_mode: caps
                .alpha_modes
                .iter()
                .copied()
                .find(|mode| *mode == CompositeAlphaMode::Opaque)
                .unwrap_or(CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let video_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tacklecast-video-bind-group-layout"),
                entries: &[
                    texture_layout_entry(0),
                    texture_layout_entry(1),
                    texture_layout_entry(2),
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tacklecast-video-uniforms"),
            size: std::mem::size_of::<VideoUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tacklecast-video-shader"),
            source: wgpu::ShaderSource::Wgsl(VIDEO_SHADER.into()),
        });

        let video_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tacklecast-video-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tacklecast-video-pipeline-layout"),
            bind_group_layouts: &[&video_bind_group_layout],
            push_constant_ranges: &[],
        });

        let video_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tacklecast-video-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let egui_renderer = EguiRenderer::new(&device, surface_format, None, 1, false);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            video_pipeline,
            video_bind_group_layout,
            video_sampler,
            uniforms,
            video_frame: None,
            egui_renderer,
        })
    }

    pub fn max_texture_side(&self) -> usize {
        self.device.limits().max_texture_dimension_2d as usize
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            self.size = size;
            return;
        }

        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn upload_frame(&mut self, frame: &CaptureFrame) {
        let needs_rebuild = self
            .video_frame
            .as_ref()
            .map(|video_frame| {
                video_frame.width != frame.width
                    || video_frame.height != frame.height
                    || video_frame.format != frame.format
            })
            .unwrap_or(true);

        if needs_rebuild {
            self.video_frame = Some(VideoFrameResources::new(
                &self.device,
                &self.video_bind_group_layout,
                &self.video_sampler,
                &self.uniforms,
                frame.width,
                frame.height,
                frame.format,
            ));
        }

        let Some(video_frame) = &self.video_frame else {
            return;
        };

        self.queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&VideoUniforms::for_format(frame.format)),
        );

        upload_plane_r8(
            &self.queue,
            &video_frame.y_texture,
            frame.width,
            frame.height,
            &frame.y_data,
        );

        match frame.format {
            PixelFormat::Nv12 => {
                let (u_plane, v_plane) = deinterleave_nv12(frame.width, frame.height, &frame.u_data);
                upload_plane_r8(
                    &self.queue,
                    &video_frame.u_texture,
                    frame.width / 2,
                    frame.height / 2,
                    &u_plane,
                );
                upload_plane_r8(
                    &self.queue,
                    &video_frame.v_texture,
                    frame.width / 2,
                    frame.height / 2,
                    &v_plane,
                );
            }
            PixelFormat::Yuvj422p => {
                upload_plane_r8(
                    &self.queue,
                    &video_frame.u_texture,
                    frame.width / 2,
                    frame.height,
                    &frame.u_data,
                );
                upload_plane_r8(
                    &self.queue,
                    &video_frame.v_texture,
                    frame.width / 2,
                    frame.height,
                    &frame.v_data,
                );
            }
        }
    }

    pub fn render(&mut self, ui: Option<PreparedUi>) -> Result<(), RenderError> {
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(());
        }

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(RenderError::OutOfMemory),
            Err(error) => return Err(RenderError::Surface(error)),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tacklecast-clear-encoder"),
            });

        let mut ui_texture_free = Vec::new();
        let mut ui_user_command_buffers = Vec::new();

        if let Some(ui) = ui.as_ref() {
            for (texture_id, image_delta) in &ui.textures_delta.set {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *texture_id, image_delta);
            }

            ui_user_command_buffers = self.egui_renderer.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &ui.paint_jobs,
                &ui.screen_descriptor,
            );

            ui_texture_free.extend(ui.textures_delta.free.iter().copied());
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tacklecast-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if let Some(video_frame) = &self.video_frame {
                let viewport = calculate_video_viewport(
                    self.size.width as f32,
                    self.size.height as f32,
                    video_frame.width as f32,
                    video_frame.height as f32,
                );
                pass.set_pipeline(&self.video_pipeline);
                pass.set_bind_group(0, &video_frame.bind_group, &[]);
                pass.set_viewport(
                    viewport.x,
                    viewport.y,
                    viewport.width.max(1.0),
                    viewport.height.max(1.0),
                    0.0,
                    1.0,
                );
                pass.draw(0..6, 0..1);
            }
        }

        if let Some(ui) = ui.as_ref() {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tacklecast-egui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            self.egui_renderer
                .render(&mut pass, &ui.paint_jobs, &ui.screen_descriptor);
        }

        self.queue
            .submit(ui_user_command_buffers.into_iter().chain(std::iter::once(encoder.finish())));

        for texture_id in ui_texture_free {
            self.egui_renderer.free_texture(&texture_id);
        }
        frame.present();
        Ok(())
    }
}

#[derive(Debug)]
pub enum RenderError {
    AdapterUnavailable,
    CreateSurface(wgpu::CreateSurfaceError),
    OutOfMemory,
    RequestDevice(wgpu::RequestDeviceError),
    Surface(wgpu::SurfaceError),
    SurfaceFormatUnavailable,
}

impl Display for RenderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdapterUnavailable => write!(f, "no suitable GPU adapter found"),
            Self::CreateSurface(error) => write!(f, "failed to create surface: {error}"),
            Self::OutOfMemory => write!(f, "GPU ran out of memory"),
            Self::RequestDevice(error) => write!(f, "failed to request device: {error}"),
            Self::Surface(error) => write!(f, "surface error: {error}"),
            Self::SurfaceFormatUnavailable => write!(f, "surface reported no usable formats"),
        }
    }
}

impl std::error::Error for RenderError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VideoUniforms {
    format_mode: u32,
    _padding: [u32; 7],
}

impl VideoUniforms {
    fn for_format(format: PixelFormat) -> Self {
        Self {
            format_mode: match format {
                PixelFormat::Nv12 => 0,
                PixelFormat::Yuvj422p => 1,
            },
            _padding: [0; 7],
        }
    }
}

struct VideoFrameResources {
    width: u32,
    height: u32,
    format: PixelFormat,
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl VideoFrameResources {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniforms: &wgpu::Buffer,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Self {
        let y_texture = create_plane_texture(device, width, height, "y");
        let (chroma_width, chroma_height) = match format {
            PixelFormat::Nv12 => (width / 2, height / 2),
            PixelFormat::Yuvj422p => (width / 2, height),
        };
        let u_texture = create_plane_texture(device, chroma_width, chroma_height, "u");
        let v_texture = create_plane_texture(device, chroma_width, chroma_height, "v");

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tacklecast-video-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&u_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&v_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        });

        Self {
            width,
            height,
            format,
            y_texture,
            u_texture,
            v_texture,
            bind_group,
        }
    }
}

fn texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn create_plane_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&format!("tacklecast-{label}-plane")),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload_plane_r8(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    data: &[u8],
) {
    let (padded_data, bytes_per_row) = pad_rows(data, width as usize, height as usize);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &padded_data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn pad_rows(data: &[u8], row_bytes: usize, rows: usize) -> (Vec<u8>, u32) {
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_row_bytes = row_bytes.next_multiple_of(alignment);

    if row_bytes == padded_row_bytes {
        return (data.to_vec(), row_bytes as u32);
    }

    let mut padded = vec![0_u8; padded_row_bytes * rows];
    for row in 0..rows {
        let src_start = row * row_bytes;
        let dst_start = row * padded_row_bytes;
        padded[dst_start..dst_start + row_bytes]
            .copy_from_slice(&data[src_start..src_start + row_bytes]);
    }

    (padded, padded_row_bytes as u32)
}

fn deinterleave_nv12(width: u32, height: u32, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let chroma_width = (width / 2) as usize;
    let chroma_height = (height / 2) as usize;
    let mut u_plane = vec![0_u8; chroma_width * chroma_height];
    let mut v_plane = vec![0_u8; chroma_width * chroma_height];

    for y in 0..chroma_height {
        for x in 0..chroma_width {
            let src_index = (y * chroma_width + x) * 2;
            let dst_index = y * chroma_width + x;
            u_plane[dst_index] = data[src_index];
            v_plane[dst_index] = data[src_index + 1];
        }
    }

    (u_plane, v_plane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_deinterleave_splits_uv_pairs() {
        let (u_plane, v_plane) = deinterleave_nv12(4, 2, &[10, 20, 30, 40]);
        assert_eq!(u_plane, vec![10, 30]);
        assert_eq!(v_plane, vec![20, 40]);
    }

    #[test]
    fn viewport_letterboxes_wider_surface() {
        let viewport = calculate_video_viewport(1920.0, 1080.0, 4.0, 3.0);
        assert!(viewport.width < 1920.0);
        assert_eq!(viewport.height, 1080.0);
    }
}

struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn calculate_video_viewport(
    surface_width: f32,
    surface_height: f32,
    video_width: f32,
    video_height: f32,
) -> Viewport {
    let surface_aspect = surface_width / surface_height;
    let video_aspect = video_width / video_height;

    if surface_aspect > video_aspect {
        let width = surface_height * video_aspect;
        Viewport {
            x: (surface_width - width) * 0.5,
            y: 0.0,
            width,
            height: surface_height,
        }
    } else {
        let height = surface_width / video_aspect;
        Viewport {
            x: 0.0,
            y: (surface_height - height) * 0.5,
            width: surface_width,
            height,
        }
    }
}

const VIDEO_SHADER: &str = r#"
struct VideoUniforms {
    format_mode: u32,
    _padding0: vec3<u32>,
};

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var u_tex: texture_2d<f32>;
@group(0) @binding(2) var v_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var<uniform> uniforms: VideoUniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );

    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );

    var out: VertexOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn sample_yuvj422p(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSample(y_tex, tex_sampler, uv).r;
    let u = textureSample(u_tex, tex_sampler, uv).r - 0.5;
    let v = textureSample(v_tex, tex_sampler, uv).r - 0.5;
    return vec3<f32>(
        y + 1.402 * v,
        y - 0.344136 * u - 0.714136 * v,
        y + 1.772 * u,
    );
}

fn sample_nv12(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSample(y_tex, tex_sampler, uv).r;
    let u = textureSample(u_tex, tex_sampler, uv).r;
    let v = textureSample(v_tex, tex_sampler, uv).r;
    let y_limited = clamp((y - (16.0 / 255.0)) * (255.0 / 219.0), 0.0, 1.0);
    let u_limited = (u - (128.0 / 255.0)) * (255.0 / 224.0);
    let v_limited = (v - (128.0 / 255.0)) * (255.0 / 224.0);
    return vec3<f32>(
        y_limited + 1.402 * v_limited,
        y_limited - 0.344136 * u_limited - 0.714136 * v_limited,
        y_limited + 1.772 * u_limited,
    );
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var rgb: vec3<f32>;
    if uniforms.format_mode == 0u {
        rgb = sample_nv12(in.uv);
    } else {
        rgb = sample_yuvj422p(in.uv);
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;
