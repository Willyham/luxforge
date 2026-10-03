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
    SpatialSlot, Support, answered, assemble_passes, chain, compile, encode_pass_over, le_bytes,
    slot_charge, spatial, upload_rows, validate, validate_step,
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
        let chain = chain::chain(steps);
        for link in &chain.links {
            compile(
                &self.device,
                &self.support,
                link,
                BoundaryFormat::Half.texture(),
            )?;
        }
        compile(&self.device, &self.support, chain.last, OUTPUT_FORMAT)?;
        Ok(started.elapsed())
    }

    /// How many spatial pass pipelines this qualifier has created: a pass whose module another
    /// pass or an earlier plan already compiled reuses that pipeline and adds nothing.
    pub fn pass_pipelines_created(&self) -> u64 {
        self.support.passes.created()
    }

    /// What the photo surface's slot holding `plan` charges the GPU-preview budget on this device:
    /// the boundary, the output in its size bucket and its uniform, the words and blocks buffers
    /// and a spatial step's planes.
    pub fn charged_bytes(&self, plan: &GpuPlan) -> Result<u64, GpuFallback> {
        slot_charge(&self.device, plan)
    }

    /// Every texel of `plan`'s output, as the `f32` values its last step returned: row by row, the
    /// boundary's size, or its region's or tail's output, alpha one.
    pub fn evaluate(&self, plan: &GpuPlan) -> Result<Vec<[f32; 4]>, String> {
        let (bytes, _) = self.run(&[], plan, None, wgpu::TextureFormat::Rgba32Float, 16)?;
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
        self.evaluate_ticks(&[first, then])
    }

    /// The last of `ticks`' outputs as [`Qualifier::evaluate`] reads it, drawn as a slot that drew
    /// every plan before it in turn draws its next tick: over the planes their passes left, running
    /// only the passes of the last that the slot's schedule runs; with how many those were. Every
    /// plan holds the same planes.
    pub fn evaluate_ticks(&self, ticks: &[&GpuPlan]) -> Result<(Vec<[f32; 4]>, u64), String> {
        let (last, before) = ticks.split_last().ok_or("a tick to draw")?;
        let (bytes, ran) = self.run(before, last, None, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok((floats(&bytes), ran))
    }

    /// `then`'s output as [`Qualifier::evaluate`] reads it, drawn as a slot that drew `first`
    /// draws its next tick when `then` changes only `inside`, `[x0, y0, x1, y1)` of its boundary's
    /// stage ([`super::GpuChange`]): each link evaluated again only where that change reaches, the
    /// rest of what it holds kept. The two plans hold the same planes.
    pub fn evaluate_changed(
        &self,
        first: &GpuPlan,
        then: &GpuPlan,
        inside: [u32; 4],
    ) -> Result<Vec<[f32; 4]>, String> {
        let (bytes, _) =
            self.run_changed(first, then, inside, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok(floats(&bytes))
    }

    /// Every texel of `plan`'s output as the 8-bit sRGB codes the stage's output texture holds:
    /// the codes its last pass computes as the CPU's quantizer does, row by row, RGBA.
    pub fn evaluate_codes(&self, plan: &GpuPlan) -> Result<Vec<[u8; 4]>, String> {
        let (bytes, _) = self.run(&[], plan, None, OUTPUT_FORMAT, 4)?;
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
        let (bytes, _) = self.run(&[], plan, float, wgpu::TextureFormat::Rgba32Float, 16)?;
        Ok(floats(&bytes))
    }

    /// [`Qualifier::evaluate_codes`] over an `rgba32float` boundary holding `pixels`, as
    /// [`Qualifier::evaluate_over`] reads it.
    pub fn evaluate_codes_over(
        &self,
        plan: &GpuPlan,
        pixels: &[[f32; 3]],
    ) -> Result<Vec<[u8; 4]>, String> {
        let (bytes, _) = self.run(&[], plan, Some(pixels), OUTPUT_FORMAT, 4)?;
        Ok(codes(&bytes))
    }

    /// `plan` checked as `prepare` checks it: every step and every pass of its chain validated,
    /// a spatial step only over the boundary's own texels on a device that can run one, and a
    /// region inside what it draws.
    fn checked(&self, plan: &GpuPlan) -> Result<(), String> {
        for step in &plan.steps {
            validate_step(step)?;
        }
        for source in assemble_passes(&plan.steps, super::End::Codes)? {
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
        Ok(())
    }

    /// `plan` drawn into a target of `format`, `texel_bytes` a texel, read back unpadded, after
    /// the passes of each plan `before` it in turn and over `float`'s boundary when that is given;
    /// with how many of `plan`'s passes ran. A plan with a geometry tail draws its output stage;
    /// one without, the boundary's size, or its region's.
    fn run(
        &self,
        before: &[&GpuPlan],
        plan: &GpuPlan,
        float: Option<&[[f32; 3]]>,
        format: wgpu::TextureFormat,
        texel_bytes: u32,
    ) -> Result<(Vec<u8>, u64), String> {
        self.run_inner(before, plan, float, format, texel_bytes, None)
    }

    /// [`Qualifier::run`] of `then` after `first`, as an incremental tick that changes `inside`.
    fn run_changed(
        &self,
        first: &GpuPlan,
        then: &GpuPlan,
        inside: [u32; 4],
        format: wgpu::TextureFormat,
        texel_bytes: u32,
    ) -> Result<(Vec<u8>, u64), String> {
        self.run_inner(&[first], then, None, format, texel_bytes, Some(inside))
    }

    fn run_inner(
        &self,
        before: &[&GpuPlan],
        plan: &GpuPlan,
        float: Option<&[[f32; 3]]>,
        format: wgpu::TextureFormat,
        texel_bytes: u32,
        inside: Option<[u32; 4]>,
    ) -> Result<(Vec<u8>, u64), String> {
        let device = &self.device;
        let mut session = Session::new(self, plan, float, format)?;
        // The slot's schedule, which the plans drawn first leave knowing what the planes hold.
        for first in before {
            session.same_planes(first)?;
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luxforge.qualification.first"),
            });
            session.encode(self, first, &mut encoder)?;
            self.queue.submit([encoder.finish()]);
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("luxforge.qualification.encoder"),
        });
        let ran = session.encode_changed(self, plan, &mut encoder, inside)?;
        let bytes = self.read_target(&session, encoder, texel_bytes)?;
        Ok((bytes, ran))
    }

    /// The session's target after `encoder`'s passes, read back unpadded, `texel_bytes` a texel:
    /// the encoder is submitted with the copy and waited for.
    fn read_target(
        &self,
        session: &Session,
        mut encoder: wgpu::CommandEncoder,
        texel_bytes: u32,
    ) -> Result<Vec<u8>, String> {
        let device = &self.device;
        let (width, height) = session.output;
        let row = width * texel_bytes;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.qualification.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            session.target.as_image_copy(),
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

    /// Each plan of `ticks` drawn as one slot draws a gesture's ticks, one after another over the
    /// same planes and intermediates, each tick after the first an incremental one that changes
    /// its rectangle of the boundary's stage when it names one ([`super::GpuChange`]); and every
    /// tick's output as [`Qualifier::evaluate`] reads it. The plans hold the same planes.
    pub fn evaluate_sequence(
        &self,
        ticks: &[(GpuPlan, Option<[u32; 4]>)],
    ) -> Result<Vec<Vec<[f32; 4]>>, String> {
        let (first, _) = ticks.first().ok_or("no plans to evaluate")?;
        let mut session = Session::new(self, first, None, wgpu::TextureFormat::Rgba32Float)?;
        let mut outputs = Vec::with_capacity(ticks.len());
        for (tick, (plan, inside)) in ticks.iter().enumerate() {
            session.same_planes(plan)?;
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("luxforge.qualification.sequence"),
                });
            session.encode_changed(self, plan, &mut encoder, inside.filter(|_| tick > 0))?;
            outputs.push(floats(&self.read_target(&session, encoder, 16)?));
        }
        Ok(outputs)
    }

    /// The GPU's throughput over `ticks`, all of one sequence over one boundary, each an
    /// incremental tick that changes its rectangle of the boundary's stage after the first, which
    /// runs whole: the first is run and waited for, then every later tick is encoded and submitted
    /// without waiting, as a slot's ticks are, and the last is waited for. Answers the CPU time
    /// encoding took a tick and the wall time from the first later submission to the last one's
    /// completion, a tick. Measurement only.
    pub fn time_throughput(
        &self,
        ticks: &[(GpuPlan, Option<[u32; 4]>)],
    ) -> Result<(std::time::Duration, std::time::Duration), String> {
        let (first, _) = ticks.first().ok_or("no plans to time")?;
        let mut session = Session::new(self, first, None, OUTPUT_FORMAT)?;
        let mut encoding = std::time::Duration::ZERO;
        let mut last = None;
        let mut started = std::time::Instant::now();
        for (tick, (plan, inside)) in ticks.iter().enumerate() {
            let encode = std::time::Instant::now();
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("luxforge.qualification.throughput"),
                });
            session.encode_changed(self, plan, &mut encoder, *inside)?;
            if tick > 0 {
                encoding += encode.elapsed();
            }
            let index = self.queue.submit([encoder.finish()]);
            if tick == 0 {
                self.device
                    .poll(wgpu::PollType::Wait {
                        submission_index: Some(index.clone()),
                        timeout: None,
                    })
                    .map_err(|error| format!("waiting for the first tick: {error}"))?;
                started = std::time::Instant::now();
            }
            last = Some(index);
        }
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: last,
                timeout: None,
            })
            .map_err(|error| format!("waiting for the last tick: {error}"))?;
        let count = (ticks.len().max(2) - 1) as u32;
        Ok((encoding / count, started.elapsed() / count))
    }
}

/// One link of a [`Session`]'s chain: its steps' pipeline, buffers and group 0, its planes and
/// schedule, and for every link but the last the intermediate it writes and what that holds.
struct Link {
    compiled: Compiled,
    id: u64,
    words: wgpu::Buffer,
    blocks: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    spatial: Option<SpatialSlot>,
    intermediate: Option<(wgpu::Texture, wgpu::TextureView)>,
    key: Option<u64>,
}

/// A plan's chain on the qualifier's device, as a slot holds it: the boundary, every link, a
/// geometry tail's intermediate and the target, kept across the plans of one sequence it encodes,
/// so a later one runs only the links and passes that changed.
struct Session {
    /// Held for the bindings that read it.
    _boundary: wgpu::Texture,
    boundary_version: u64,
    links: Vec<Link>,
    /// A geometry tail's intermediate and the tail's group 0 over it.
    tail: Option<(wgpu::Texture, wgpu::BindGroup)>,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    output: (u32, u32),
    origin: (u32, u32),
}

/// A buffer of `words`, at least one, with room to spare for a later tick's.
fn words_buffer(device: &wgpu::Device, label: &str, words: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: ((words.max(1) * 4) as u64).next_power_of_two().max(1024) * 2,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl Session {
    /// `plan`'s chain created, its boundary uploaded in its own format or as `float`'s pixels in
    /// `rgba32float`, its last link writing a target of `format`.
    fn new(
        qualifier: &Qualifier,
        plan: &GpuPlan,
        float: Option<&[[f32; 3]]>,
        format: wgpu::TextureFormat,
    ) -> Result<Self, String> {
        qualifier.checked(plan)?;
        let device = &qualifier.device;
        let (width, height) = plan.boundary.size();
        let boundary_format = match float {
            Some(pixels) if pixels.len() != width as usize * height as usize => {
                return Err("an rgba32float boundary holds the boundary's pixels".into());
            }
            Some(_) => BoundaryFormat::Float,
            None => plan.boundary.format(),
        };
        let texture = |label, (width, height), format, usage| {
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
            (width, height),
            boundary_format.texture(),
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        match float {
            Some(pixels) => {
                // Row by row, so no staging copy is larger than a row.
                for (y, row) in pixels.chunks_exact(width as usize).enumerate() {
                    let texels: Vec<u8> = row
                        .iter()
                        .flat_map(|[r, g, b]| [*r, *g, *b, 1.0])
                        .flat_map(f32::to_le_bytes)
                        .collect();
                    qualifier.queue.write_texture(
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
            // The slot's own chunked upload, every row at once: a slot spreads the same chunks over
            // frames, which only changes when they are written.
            None => {
                let (row, _) =
                    upload_rows(&qualifier.queue, &boundary, &plan.boundary, 0, u64::MAX);
                debug_assert!(
                    !plan.boundary.holds_texels() || row == plan.boundary.size().1,
                    "every row is written"
                );
            }
        }
        let origin = (
            plan.texels.origin[0].max(0.0) as u32,
            plan.texels.origin[1].max(0.0) as u32,
        );
        let chain = chain::chain(&plan.steps);
        let intermediate_format = boundary_format.texture();
        let count = chain.links.len() + 1;
        let mut links: Vec<Link> = Vec::with_capacity(count);
        let (mut words, mut blocks) = (Vec::new(), Vec::new());
        for (index, steps) in chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last))
            .enumerate()
        {
            let last = index + 1 == count;
            let link_format = if last { format } else { intermediate_format };
            let compiled = compile(device, &qualifier.support, steps, link_format)?;
            let offset = if last {
                super::output_offset(plan)
            } else {
                (0, 0)
            };
            chain::pack_steps(plan.texels, offset, steps, &mut words, &mut blocks);
            let words_held = words_buffer(device, "luxforge.qualification.words", words.len());
            let blocks_held = words_buffer(device, "luxforge.qualification.blocks", blocks.len());
            let input = links
                .last()
                .and_then(|link| link.intermediate.as_ref())
                .map_or(&boundary, |(texture, _)| texture);
            let bindings = qualifier.bindings(input, &words_held, &blocks_held);
            let spatial = spatial::PlanesKey::of(steps, (width, height), origin)
                .map(|key| SpatialSlot::new(spatial::Planes::create(device, key)));
            let intermediate = (!last).then(|| {
                let texture = texture(
                    "luxforge.qualification.link",
                    (width, height),
                    intermediate_format,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                );
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                (texture, view)
            });
            links.push(Link {
                compiled,
                id: index as u64,
                words: words_held,
                blocks: blocks_held,
                bindings,
                spatial,
                intermediate,
                key: None,
            });
        }
        let tail = chain.last.iter().find_map(|step| match step {
            GpuStep::Geometry(tail) => Some(tail),
            _ => None,
        });
        // The output is the tail's output stage when the plan has a tail.
        let output = tail.map_or_else(
            || plan.region.map_or((width, height), |region| region.size()),
            GpuTail::output,
        );
        // A tail's content pass writes the intermediate the boundary's size, which the tail reads
        // through bindings of its own over the same words and blocks.
        let last = links.last().expect("a chain has a last link");
        let tail = tail.map(|tail| {
            let texture = texture(
                "luxforge.qualification.intermediate",
                (width, height),
                tail.intermediate(),
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let bindings = qualifier.bindings(&texture, &last.words, &last.blocks);
            (texture, bindings)
        });
        let target = texture(
            "luxforge.qualification.target",
            output,
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            _boundary: boundary,
            boundary_version: plan.boundary.version(),
            links,
            tail,
            target,
            view,
            output,
            origin,
        })
    }

    /// Whether `other`'s chain holds the planes this one's does, link for link, as a plan drawn
    /// after another in one slot must.
    fn same_planes(&self, other: &GpuPlan) -> Result<(), String> {
        let chain = chain::chain(&other.steps);
        let steps = chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last));
        let size = other.boundary.size();
        if steps.clone().count() != self.links.len() {
            return Err("a plan drawn after another holds the same planes".into());
        }
        for (link, steps) in self.links.iter().zip(steps) {
            let key = spatial::PlanesKey::of(steps, size, self.origin);
            if link.spatial.as_ref().map(|spatial| &spatial.planes.key) != key.as_ref() {
                return Err("a plan drawn after another holds the same planes".into());
            }
        }
        Ok(())
    }

    /// Encode `plan`, of the sequence the session was made for, as the slot's next tick: each link
    /// whose content changed into its intermediate, then the last into the target. Answers how
    /// many spatial passes ran.
    fn encode(
        &mut self,
        qualifier: &Qualifier,
        plan: &GpuPlan,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<u64, String> {
        self.encode_changed(qualifier, plan, encoder, None)
    }

    /// [`Session::encode`] as an incremental tick when `inside` is given: the rectangle of the
    /// plan's boundary stage it changes in since the plan the session last encoded, as the slot runs
    /// it ([`super::GpuChange`]).
    fn encode_changed(
        &mut self,
        qualifier: &Qualifier,
        plan: &GpuPlan,
        encoder: &mut wgpu::CommandEncoder,
        inside: Option<[u32; 4]>,
    ) -> Result<u64, String> {
        if plan.boundary.version() != self.boundary_version {
            return Err("a session draws over the boundary it was made with".into());
        }
        let chain = chain::chain(&plan.steps);
        let count = self.links.len();
        let mut input = chain::boundary_key(self.boundary_version);
        let mut ran = 0;
        let (mut words, mut blocks) = (Vec::new(), Vec::new());
        let size = plan.boundary.size();
        let incremental = inside.map(|[x0, y0, x1, y1]| {
            let texel = |at: u32, origin: u32, limit: u32| at.saturating_sub(origin).min(limit);
            spatial::Rect {
                x0: texel(x0, self.origin.0, size.0),
                y0: texel(y0, self.origin.1, size.1),
                x1: texel(x1, self.origin.0, size.0),
                y1: texel(y1, self.origin.1, size.1),
            }
        });
        let mut dirty = incremental;
        for (index, steps) in chain
            .links
            .iter()
            .copied()
            .chain(std::iter::once(chain.last))
            .enumerate()
        {
            let last = index + 1 == count;
            let link = &mut self.links[index];
            let offset = if last {
                super::output_offset(plan)
            } else {
                (0, 0)
            };
            chain::pack_steps(plan.texels, offset, steps, &mut words, &mut blocks);
            if (words.len() * 4) as u64 > link.words.size()
                || (blocks.len() * 4) as u64 > link.blocks.size()
            {
                return Err("a later tick outgrows the session's buffers".into());
            }
            qualifier
                .queue
                .write_buffer(&link.words, 0, &le_bytes(&words));
            qualifier
                .queue
                .write_buffer(&link.blocks, 0, &le_bytes(&blocks));
            let key = chain::link_key(input, &words, &blocks, link.id);
            if !last && link.key == Some(key) {
                input = key;
                dirty = incremental;
                continue;
            }
            let link_dirty = dirty.filter(|_| link.key.is_some());
            let mut reached = link_dirty;
            if let Some(spatial) = link.spatial.as_mut() {
                let (passes, over) = spatial.tick(
                    &qualifier.device,
                    &qualifier.queue,
                    encoder,
                    (&link.compiled.spatial, link.id),
                    &link.bindings,
                    steps,
                    (&words, &blocks),
                    input,
                    (plan.texels, size),
                    link_dirty,
                );
                ran += passes;
                reached = link_dirty.zip(over).map(|(dirty, over)| dirty.union(&over));
            }
            dirty = incremental
                .zip(reached)
                .map(|(changed, reached)| changed.union(&reached));
            let planes_group = link
                .spatial
                .as_ref()
                .and_then(|spatial| spatial.groups.as_ref())
                .and_then(|(_, groups)| groups.fragment.as_ref());
            let as_size = |(width, height): (u32, u32)| (width as f32, height as f32);
            match (&link.intermediate, &self.tail, &link.compiled.tail) {
                (Some((_, view)), _, _) => encode_pass_over(
                    encoder,
                    view,
                    &link.compiled.render,
                    (&link.bindings, planes_group),
                    as_size(size),
                    reached,
                ),
                (None, Some((texture, tail_bindings)), Some(tail)) => {
                    let content = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    // As the slot draws an identity tail on an incremental tick: both passes only
                    // where the changes reached.
                    let identity = steps
                        .iter()
                        .any(|step| matches!(step, GpuStep::Geometry(tail) if tail.identity()));
                    let scissors = reached.filter(|_| identity).map(|reached| {
                        let grown = reached.grown(1, size);
                        let (dx, dy) = super::output_offset(plan);
                        let origin = plan.texels.origin.map(|value| value.max(0.0) as u32);
                        let to = |at: u32, origin: u32, offset: u32, limit: u32| {
                            (at + origin).saturating_sub(offset).min(limit)
                        };
                        let output = spatial::Rect {
                            x0: to(grown.x0, origin[0], dx, self.output.0),
                            y0: to(grown.y0, origin[1], dy, self.output.1),
                            x1: to(grown.x1, origin[0], dx, self.output.0),
                            y1: to(grown.y1, origin[1], dy, self.output.1),
                        };
                        (grown, output)
                    });
                    encode_pass_over(
                        encoder,
                        &content,
                        &link.compiled.render,
                        (&link.bindings, planes_group),
                        as_size(size),
                        scissors.map(|(content, _)| content),
                    );
                    encode_pass_over(
                        encoder,
                        &self.view,
                        tail,
                        (tail_bindings, None),
                        as_size(self.output),
                        scissors.map(|(_, output)| output),
                    );
                }
                (None, None, None) => {
                    let (dx, dy) = super::output_offset(plan);
                    let scissor = reached.map(|reached| {
                        let grown = reached.grown(1, size);
                        spatial::Rect {
                            x0: grown.x0.saturating_sub(dx).min(self.output.0),
                            y0: grown.y0.saturating_sub(dy).min(self.output.1),
                            x1: grown.x1.saturating_sub(dx).min(self.output.0),
                            y1: grown.y1.saturating_sub(dy).min(self.output.1),
                        }
                    });
                    encode_pass_over(
                        encoder,
                        &self.view,
                        &link.compiled.render,
                        (&link.bindings, planes_group),
                        as_size(self.output),
                        scissor,
                    )
                }
                _ => return Err("a plan's passes do not match its tail".into()),
            }
            link.key = Some(key);
            input = key;
        }
        Ok(ran)
    }
}

impl Qualifier {
    /// Group 0 over `input`: the words, the blocks and the texture a link reads as its boundary.
    fn bindings(
        &self,
        input: &wgpu::Texture,
        words: &wgpu::Buffer,
        blocks: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let view = input.create_view(&wgpu::TextureViewDescriptor::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        })
    }
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
