//! Qualification only: a [`GpuPlan`] evaluated on a headless device and read back, for the readback
//! tests that qualify each program against its CPU unit and for the corpus harness that measures a
//! stack's GPU frame against the CPU frame it previews (`docs/design/gpu-preview.md`).
//!
//! It runs the stage's own shader: each step checked by [`validate_step`], the plan assembled and
//! its words packed exactly as `prepare` assembles and packs them, a spatial step's planes created
//! and its passes encoded before the frame's as the slot encodes them, a geometry tail's second
//! pass over the content pass's intermediate, and the boundary uploaded in its own format as the
//! slot uploads it. Only the target differs. [`Qualifier::evaluate`] writes `rgba32float`, so a
//! program's `f32` output is read before any encoding, which is where a non-finite value would be
//! hidden; [`Qualifier::evaluate_codes`] writes the stage's own output format and reads the codes
//! its last pass computes as the CPU's quantizer does.
//!
//! Built only with the crate's `qualification` feature, which only a `[dev-dependencies]` table
//! may turn on (`cargo xtask check-repository`), so no build of the desktop has it: the surface
//! itself never reads a pixel back or waits on the GPU. Everything here blocks the calling test.
use super::{
    BoundaryFormat, Compiled, GpuBoundary, GpuFallback, GpuPlan, GpuStep, GpuTail, OUTPUT_FORMAT,
    Support, answered, assemble_passes, compile, encode_pass, le_bytes, pack, slot_charge, spatial,
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

    /// How long the stage's own compile of `steps` takes on this device, from the programs' `naga`
    /// checks to the backend's pipelines, as the compile thread runs it
    /// ([`super::compile`](mod@super::compile)): wall-clock time on the calling thread.
    pub fn compile_time(&self, steps: &[GpuStep]) -> Result<std::time::Duration, String> {
        let started = std::time::Instant::now();
        compile(&self.device, &self.support, steps, OUTPUT_FORMAT)?;
        Ok(started.elapsed())
    }

    /// How many spatial pass pipelines this qualifier has created: a pass whose module another
    /// pass or an earlier plan already compiled reuses that pipeline and adds nothing.
    pub fn pass_pipelines_created(&self) -> u64 {
        self.support.passes.created()
    }

    /// What the photo surface's slot holding `plan` charges the GPU-preview budget on this device:
    /// the boundary, the output in the photograph's size bucket and its uniform, the words and
    /// blocks buffers and a spatial step's planes.
    pub fn charged_bytes(&self, plan: &GpuPlan) -> Result<u64, GpuFallback> {
        slot_charge(&self.device, plan)
    }

    /// Every texel of `plan`'s output, as the `f32` values its last step returned: row by row, the
    /// boundary's size, or its region's or tail's output, alpha one.
    pub fn evaluate(&self, plan: &GpuPlan) -> Result<Vec<[f32; 4]>, String> {
        let (bytes, _) = self.run(None, plan, None, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok(floats(&bytes))
    }

    /// `then`'s output as [`Qualifier::evaluate`] reads it, drawn as a slot that drew `first` draws
    /// its next tick: over the planes `first`'s passes wrote, running only the passes of `then` the
    /// slot's schedule runs; with how many those were. The two plans hold the same planes.
    pub fn evaluate_after(
        &self,
        first: &GpuPlan,
        then: &GpuPlan,
    ) -> Result<(Vec<[f32; 4]>, u64), String> {
        let (bytes, ran) = self.run(
            Some(first),
            then,
            None,
            wgpu::TextureFormat::Rgba32Float,
            16,
        )?;
        Ok((floats(&bytes), ran))
    }

    /// Every texel of `plan`'s output as the 8-bit sRGB codes the stage's output texture holds:
    /// the codes its last pass computes as the CPU's quantizer does, row by row, RGBA.
    pub fn evaluate_codes(&self, plan: &GpuPlan) -> Result<Vec<[u8; 4]>, String> {
        let (bytes, _) = self.run(None, plan, None, OUTPUT_FORMAT, 4)?;
        Ok(codes(&bytes))
    }

    /// [`Qualifier::evaluate`] over an `rgba32float` boundary holding `pixels`, row by row the
    /// boundary's size, as they are, in place of the plan's half floats: a measurement of what the
    /// boundary's format costs a program, which no slot draws.
    pub fn evaluate_over(
        &self,
        plan: &GpuPlan,
        pixels: &[[f32; 3]],
    ) -> Result<Vec<[f32; 4]>, String> {
        let float = Some(pixels);
        let (bytes, _) = self.run(None, plan, float, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok(floats(&bytes))
    }

    /// [`Qualifier::evaluate_codes`] over an `rgba32float` boundary holding `pixels`, as
    /// [`Qualifier::evaluate_over`] reads it.
    pub fn evaluate_codes_over(
        &self,
        plan: &GpuPlan,
        pixels: &[[f32; 3]],
    ) -> Result<Vec<[u8; 4]>, String> {
        let (bytes, _) = self.run(None, plan, Some(pixels), OUTPUT_FORMAT, 4)?;
        Ok(codes(&bytes))
    }

    /// `plan` checked as `prepare` checks it, and compiled for a target of `format`.
    fn compiled(&self, plan: &GpuPlan, format: wgpu::TextureFormat) -> Result<Compiled, String> {
        for step in &plan.steps {
            validate_step(step)?;
        }
        for source in assemble_passes(&plan.steps, true)? {
            validate(&source)?;
        }
        let spatial_steps = plan
            .steps
            .iter()
            .any(|step| matches!(step, GpuStep::Spatial(_)));
        if spatial_steps && !spatial::supported(&self.device.limits()) {
            return Err("the device cannot run a spatial step".into());
        }
        if spatial_steps && plan.texels.step != [1.0, 1.0] {
            return Err("a spatial step runs over the boundary's own texels".into());
        }
        if !super::region_drawable(plan) {
            return Err("the plan's region is not inside what it draws".into());
        }
        compile(&self.device, &self.support, &plan.steps, format)
    }

    /// `plan`'s boundary uploaded in its own format, or `float`'s pixels as `rgba32float` in its
    /// place, and its words and blocks packed and written, bound as group 0.
    fn inputs(&self, plan: &GpuPlan, float: Option<&[[f32; 3]]>) -> Result<Inputs, String> {
        let device = &self.device;
        let (width, height) = plan.boundary.size();
        let format = match float {
            Some(pixels) if pixels.len() != width as usize * height as usize => {
                return Err("an rgba32float boundary holds the boundary's pixels".into());
            }
            Some(_) => wgpu::TextureFormat::Rgba32Float,
            None => plan.boundary.format().texture(),
        };
        let boundary = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("luxforge.qualification.boundary"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        match float {
            Some(pixels) => {
                // Row by row, so no staging copy is larger than a row.
                for (y, row) in pixels.chunks_exact(width as usize).enumerate() {
                    let texels: Vec<u8> = row
                        .iter()
                        .flat_map(|[r, g, b]| [*r, *g, *b, 1.0])
                        .flat_map(f32::to_le_bytes)
                        .collect();
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &boundary,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: y as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &texels,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 16),
                            rows_per_image: Some(1),
                        },
                        wgpu::Extent3d {
                            width,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            None => upload_boundary(&self.queue, &boundary, &plan.boundary),
        }
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
        let words_buffer = storage("luxforge.qualification.words", &words);
        let blocks_buffer = storage("luxforge.qualification.blocks", &blocks);
        let boundary_view = boundary.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.qualification.bindings"),
            layout: &self.support.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: words_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: blocks_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&boundary_view),
                },
            ],
        });
        Ok(Inputs {
            bindings,
            words,
            blocks,
            words_buffer,
            blocks_buffer,
            _boundary: boundary,
        })
    }

    /// `plan` drawn into a target of `format`, `texel_bytes` a texel, read back unpadded, after
    /// `first`'s passes when it is given and over `float`'s boundary when that is; with how many
    /// of `plan`'s passes ran. A plan with a geometry tail draws its output stage; one without, the
    /// boundary's size.
    fn run(
        &self,
        first: Option<&GpuPlan>,
        plan: &GpuPlan,
        float: Option<&[[f32; 3]]>,
        format: wgpu::TextureFormat,
        texel_bytes: u32,
    ) -> Result<(Vec<u8>, u64), String> {
        let device = &self.device;
        let compiled = self.compiled(plan, format)?;
        let boundary_size = plan.boundary.size();
        let (width, height) = boundary_size;
        let tail = plan.steps.iter().find_map(|step| match step {
            GpuStep::Geometry(tail) => Some(tail),
            _ => None,
        });
        let origin_of = |plan: &GpuPlan| {
            (
                plan.texels.origin[0].max(0.0) as u32,
                plan.texels.origin[1].max(0.0) as u32,
            )
        };
        let key = spatial::PlanesKey::of(&plan.steps, (width, height), origin_of(plan));
        let mut planes = key.clone().map(|key| spatial::Planes::create(device, key));
        // The slot's schedule, which a first plan leaves knowing what the planes hold.
        let mut schedule = spatial::Schedule::default();
        if let Some(first) = first {
            let first_key =
                spatial::PlanesKey::of(&first.steps, first.boundary.size(), origin_of(first));
            if first_key != key {
                return Err("a plan drawn after another holds the same planes".into());
            }
            let first_compiled = self.compiled(first, format)?;
            let inputs = self.inputs(first, None)?;
            if let Some(planes) = planes.as_mut() {
                planes.write_parameters(&self.queue, &first.steps);
                let groups = spatial::Groups::new(device, &first_compiled.spatial, planes);
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("luxforge.qualification.first"),
                });
                let run = schedule.run(
                    &first.steps,
                    &inputs.words,
                    &inputs.blocks,
                    first.boundary.version(),
                    &planes.key,
                );
                groups.encode(
                    &mut encoder,
                    &first_compiled.spatial,
                    &inputs.bindings,
                    &run,
                );
                self.queue.submit([encoder.finish()]);
            }
        }
        let inputs = self.inputs(plan, float)?;
        let bindings = &inputs.bindings;
        // The output is the tail's output stage when the plan has a tail.
        let (width, height) = tail.map_or_else(
            || plan.region.map_or(boundary_size, |region| region.size()),
            GpuTail::output,
        );
        let texture = |label, (width, height): (u32, u32), format, usage| {
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
        let target = texture(
            "luxforge.qualification.target",
            (width, height),
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        // A tail's content pass writes the intermediate the boundary's size, which the tail reads
        // through bindings of its own over the same words and blocks.
        let intermediate = tail.map(|tail| {
            let texture = texture(
                "luxforge.qualification.intermediate",
                boundary_size,
                tail.intermediate(),
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("luxforge.qualification.intermediate_bindings"),
                layout: &self.support.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: inputs.words_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: inputs.blocks_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                ],
            });
            (texture, bindings)
        });
        let row = width * texel_bytes;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.qualification.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if let Some(planes) = planes.as_mut() {
            planes.write_parameters(&self.queue, &plan.steps);
        }
        let groups = planes
            .as_ref()
            .map(|planes| spatial::Groups::new(device, &compiled.spatial, planes));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.qualification.encoder"),
        });
        let mut ran = 0;
        if let (Some(groups), Some(planes)) = (&groups, &planes) {
            let run = schedule.run(
                &plan.steps,
                &inputs.words,
                &inputs.blocks,
                plan.boundary.version(),
                &planes.key,
            );
            ran = groups.encode(&mut encoder, &compiled.spatial, bindings, &run);
        }
        let planes_group = groups.as_ref().and_then(|groups| groups.fragment.as_ref());
        let size = |(width, height): (u32, u32)| (width as f32, height as f32);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        match (&intermediate, &compiled.tail) {
            (Some((texture, tail_bindings)), Some(tail)) => {
                let content = texture.create_view(&wgpu::TextureViewDescriptor::default());
                encode_pass(
                    &mut encoder,
                    &content,
                    &compiled.render,
                    (bindings, planes_group),
                    size(boundary_size),
                );
                encode_pass(
                    &mut encoder,
                    &view,
                    tail,
                    (tail_bindings, None),
                    size((width, height)),
                );
            }
            (None, None) => encode_pass(
                &mut encoder,
                &view,
                &compiled.render,
                (bindings, planes_group),
                size((width, height)),
            ),
            _ => return Err("a plan's passes do not match its tail".into()),
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
        Ok((bytes, ran))
    }
}

/// A plan's group 0 and what it binds, and the words and blocks it packed.
struct Inputs {
    bindings: wgpu::BindGroup,
    words: Vec<u32>,
    blocks: Vec<u32>,
    words_buffer: wgpu::Buffer,
    blocks_buffer: wgpu::Buffer,
    _boundary: wgpu::Texture,
}

/// `rgba8` texels as codes.
fn codes(bytes: &[u8]) -> Vec<[u8; 4]> {
    bytes
        .chunks_exact(4)
        .map(|texel| [texel[0], texel[1], texel[2], texel[3]])
        .collect()
}

/// Little-endian `rgba32float` texels as values.
fn floats(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(16)
        .map(|texel| {
            std::array::from_fn(|channel| {
                let at = channel * 4;
                f32::from_le_bytes([texel[at], texel[at + 1], texel[at + 2], texel[at + 3]])
            })
        })
        .collect()
}

/// A boundary of `width` × `height` texels from `f32` values in row order, each held as the nearest
/// half float, alpha one: what a qualification test hands [`Qualifier::evaluate`], and the values a
/// CPU reference must then read, which [`held`] gives.
pub fn boundary(width: u32, height: u32, version: u64, pixels: &[[f32; 3]]) -> Option<GpuBoundary> {
    boundary_as(BoundaryFormat::Half, width, height, version, pixels)
}

/// [`boundary`] in `format`: an `rgba32float` one holds each value as it is, as a RAW's does.
pub fn boundary_as(
    format: BoundaryFormat,
    width: u32,
    height: u32,
    version: u64,
    pixels: &[[f32; 3]],
) -> Option<GpuBoundary> {
    GpuBoundary::from_linear(
        format,
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
