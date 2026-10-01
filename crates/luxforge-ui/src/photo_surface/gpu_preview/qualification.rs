//! Qualification only: a [`GpuPlan`] evaluated on a headless device and read back, for the readback
//! tests that qualify each program against its CPU unit and for the corpus harness that measures a
//! stack's GPU frame against the CPU frame it previews (`docs/design/gpu-preview.md`).
//!
//! It runs the stage's own shader: each step checked by [`validate_step`], the plan assembled and
//! its words packed exactly as `prepare` assembles and packs them, and the boundary uploaded as the
//! slot uploads it. Only the target differs. [`Qualifier::evaluate`] writes `rgba32float`, so a
//! program's `f32` output is read before any encoding, which is where a non-finite value would be
//! hidden; [`Qualifier::evaluate_codes`] writes the stage's own sRGB-typed output format and reads
//! the codes the surface draws.
//!
//! Built only with the crate's `qualification` feature, which only a `[dev-dependencies]` table
//! may turn on (`cargo xtask check-repository`), so no build of the desktop has it: the surface
//! itself never reads a pixel back or waits on the GPU. Everything here blocks the calling test.
use super::{
    GpuBoundary, GpuPlan, OUTPUT_FORMAT, Support, answered, assemble, compile, le_bytes, pack,
    upload_boundary, validate, validate_step,
};
use std::sync::mpsc;

/// A headless device, and the stage's layouts on it.
pub struct Qualifier {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::AdapterInfo,
    support: Support,
}

impl Qualifier {
    /// A device of this host's default adapter, or `None` after printing that `test` was skipped:
    /// a test without one ran nothing and is not GPU evidence.
    pub fn headless(test: &str) -> Option<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let Some(Ok(adapter)) =
            answered(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("skipped: no GPU adapter; {test} ran nothing and is not GPU evidence");
            return None;
        };
        let Some(Ok((device, queue))) =
            answered(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!(
                "skipped: no device for the adapter; {test} ran nothing and is not GPU evidence"
            );
            return None;
        };
        let support = Support::new(&device);
        Some(Self {
            device,
            queue,
            adapter: adapter.get_info(),
            support,
        })
    }

    /// The adapter, its backend and its driver, for a report.
    pub fn adapter(&self) -> String {
        let info = &self.adapter;
        format!(
            "{} ({:?}, {:?}, driver {:?} {:?})",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        )
    }

    /// Every texel of `plan`'s output, as the `f32` values its last step returned: row by row, the
    /// boundary's size, alpha one.
    pub fn evaluate(&self, plan: &GpuPlan) -> Result<Vec<[f32; 4]>, String> {
        let bytes = self.run(plan, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok(bytes
            .chunks_exact(16)
            .map(|texel| {
                std::array::from_fn(|channel| {
                    let at = channel * 4;
                    f32::from_le_bytes([texel[at], texel[at + 1], texel[at + 2], texel[at + 3]])
                })
            })
            .collect())
    }

    /// Every texel of `plan`'s output as the 8-bit sRGB codes the stage's output texture holds:
    /// the hardware's encoding of the same values, row by row, RGBA.
    pub fn evaluate_codes(&self, plan: &GpuPlan) -> Result<Vec<[u8; 4]>, String> {
        let bytes = self.run(plan, OUTPUT_FORMAT, 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|texel| [texel[0], texel[1], texel[2], texel[3]])
            .collect())
    }

    /// The plan drawn into a target of `format`, `texel_bytes` a texel, read back unpadded.
    fn run(
        &self,
        plan: &GpuPlan,
        format: wgpu::TextureFormat,
        texel_bytes: u32,
    ) -> Result<Vec<u8>, String> {
        let device = &self.device;
        for step in &plan.steps {
            validate_step(step)?;
        }
        validate(&assemble(&plan.steps)?)?;
        let pipeline = compile(device, &self.support.pipeline_layout, &plan.steps, format)?;
        let (width, height) = plan.boundary.size();
        let texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let boundary = texture(
            "luxforge.qualification.boundary",
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        upload_boundary(&self.queue, &boundary, &plan.boundary);
        let (mut words, mut blocks) = (Vec::new(), Vec::new());
        pack(plan, &mut words, &mut blocks);
        let storage = |label, data: &[u32]| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (data.len() * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &le_bytes(data));
            buffer
        };
        let words = storage("luxforge.qualification.words", &words);
        let blocks = storage("luxforge.qualification.blocks", &blocks);
        let boundary_view = boundary.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.qualification.bindings"),
            layout: &self.support.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: words.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: blocks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&boundary_view),
                },
            ],
        });
        let target = texture(
            "luxforge.qualification.target",
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let row = width * texel_bytes;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.qualification.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.qualification.encoder"),
        });
        {
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("luxforge.qualification.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let index = self.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|error| format!("waiting for the qualification pass: {error}"))?;
        receiver
            .try_recv()
            .map_err(|_| "the readback was not mapped once its submission finished".to_owned())?
            .map_err(|error| format!("mapping the readback: {error}"))?;
        let mapped = readback.slice(..).get_mapped_range();
        let mut bytes = Vec::with_capacity((row * height) as usize);
        for line in mapped.chunks_exact(padded as usize) {
            bytes.extend_from_slice(&line[..row as usize]);
        }
        drop(mapped);
        readback.unmap();
        Ok(bytes)
    }
}

/// A boundary of `width` × `height` texels from `f32` values in row order, each held as the nearest
/// half float, alpha one: what a qualification test hands [`Qualifier::evaluate`], and the values a
/// CPU reference must then read, which [`held`] gives.
pub fn boundary(width: u32, height: u32, version: u64, pixels: &[[f32; 3]]) -> Option<GpuBoundary> {
    GpuBoundary::from_linear(
        width,
        height,
        version,
        pixels.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
}

/// `value` as the boundary holds it: the nearest half float, widened.
pub fn held(value: f32) -> f32 {
    half::f16::from_f32(value).to_f32()
}
