//! Headless only: the photo surface's own drawing of a picture at rest and of one of its tiles
//! (`docs/design/gpu-preview.md`, "The picture at rest"), run on a device the caller holds, with
//! no window, and read back — what a test or the corpus harness holds the editor's GPU frame at
//! rest to, drawn by the code the desktop draws it with, not a copy of it.
//!
//! [`HeadlessSurface::rest`] hands a source and a picture at rest to a pipeline frame after frame,
//! as the desktop's surface is handed them while the editor is idle — the source uploaded a frame's
//! rows at a time, the tiles drawn [`REST_TILES_PER_FRAME`](super::REST_TILES_PER_FRAME) a frame
//! through the rest's own slot, each a fresh evaluation, reduced as it is drawn, the accumulator
//! quantized after the last — and reads the rest output back. [`HeadlessSurface::tile`] draws one
//! tile's plan as the rest draws each, through a slot of its own, and reads the tile's codes back;
//! [`HeadlessSurface::draw`] draws any plan so, a gesture's or a picture at rest's view plan.
//! A source may be a window of a larger one ([`GpuSource::window`]), so a caller drawing tile by
//! tile need hold only each tile's window.
//!
//! Built only with the crate's `qualification` feature, as [`super::qualification`] is: everything
//! here blocks the calling thread on the GPU, which no surface of the desktop does, and waits for
//! the compile thread through the test base's one hang-bounded wait, which the feature brings in.
use super::{GpuFallback, GpuPlan, GpuRest, GpuSource, answered};
use crate::{Executor, State};
use std::sync::{Arc, mpsc};

/// A device of this host's default adapter at `limits`, its queue and the adapter's description,
/// or `None` after printing that `run` was skipped: a run without one drew nothing and is not GPU
/// evidence. The default adapter is wgpu's first by rank, a hardware one before a software one;
/// with `LUXFORGE_GPU_ADAPTER=software` it is the platform's software adapter alone (lavapipe,
/// WARP), asked for as wgpu's fallback adapter, so a host with both measures the software one.
pub fn device(
    run: &str,
    limits: wgpu::Limits,
) -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let software = std::env::var("LUXFORGE_GPU_ADAPTER").as_deref() == Ok("software");
    let options = wgpu::RequestAdapterOptions {
        force_fallback_adapter: software,
        ..wgpu::RequestAdapterOptions::default()
    };
    let Some(Ok(adapter)) = answered(instance.request_adapter(&options)) else {
        let what = if software { "software" } else { "GPU" };
        eprintln!("skipped: no {what} adapter; {run} ran nothing and is not GPU evidence");
        return None;
    };
    let descriptor = wgpu::DeviceDescriptor {
        required_limits: limits,
        ..wgpu::DeviceDescriptor::default()
    };
    let Some(Ok((device, queue))) = answered(adapter.request_device(&descriptor)) else {
        eprintln!("skipped: no device for the adapter; {run} ran nothing and is not GPU evidence");
        return None;
    };
    Some((device, queue, adapter.get_info()))
}

/// A picture at rest drawn headless: the view's codes, RGBA row by row — none for tiles drawn for
/// their counts alone — the frames it took, those that waited for a compile or the upload among
/// them, and its tiles' histogram and clipping counts, read back, or why there are none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestDrawn {
    pub codes: Vec<[u8; 4]>,
    pub frames: u32,
    pub counts: Result<super::histogram::Counts, super::histogram::HistogramError>,
    /// What its tiles did, as the surface's diagnostics report it once the last is in: the sweeps
    /// a staged picture was drawn in among them.
    pub figures: super::RestFigures,
}

/// A drag's ticks drawn headless ([`HeadlessSurface::ticks`]): each timed tick's duration, and the
/// last tick's output codes, RGBA row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticked {
    pub times: Vec<std::time::Duration>,
    pub last: Vec<[u8; 4]>,
}

/// A plan drawn headless: its output's codes, RGBA row by row, and its size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawn {
    pub codes: Vec<[u8; 4]>,
    pub size: (u32, u32),
}

/// One surface of a pipeline of its own on a headless device, drawing as the desktop's does.
pub struct HeadlessSurface {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: Executor,
    surface: State,
}

impl HeadlessSurface {
    /// A surface on `device`, its pipeline drawing to an sRGB target as the desktop's does and
    /// counting into figures of its own.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let pipeline = Executor::with_figures(
            device,
            queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            Arc::default(),
        );
        let surface = pipeline.new_surface();
        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            surface,
        }
    }

    /// One frame's source: held, a frame's rows of it written, and the frame ended.
    fn hand(&mut self, source: &GpuSource) {
        self.pipeline
            .fit_source(&self.device, &self.queue, Some(source));
        self.pipeline.trim_source();
    }

    /// `rest` drawn from `source` frame after frame, as a surface handed both draws it while the
    /// editor is idle, until its last tile is in the rest output: the output read back. The
    /// fallback that stops it otherwise, or `compiling` once a sequence's compile has kept it
    /// waiting past the test base's hang bound.
    pub fn rest(&mut self, source: &GpuSource, rest: &GpuRest) -> Result<RestDrawn, GpuFallback> {
        let mut frames = 0;
        let mut done = super::RestFigures::default();
        // One frame a look, through the one hang-bounded wait: what can keep it waiting is a
        // sequence's compile on the compile thread, since the source's rows are written here.
        let drawn = luxforge_testbase::try_wait_for("the picture at rest's last tile", || {
            frames += 1;
            self.pipeline
                .fit_source(&self.device, &self.queue, Some(source));
            self.pipeline
                .prepare_rest(&mut self.surface, &self.device, &self.queue, Some(rest));
            self.pipeline.trim_source();
            let Some(figures) = self.surface.rest_figures() else {
                return Some(Err(GpuFallback::PipelineFailed));
            };
            if let Some(fallback) = figures.fallback {
                return Some(Err(fallback));
            }
            if !figures.done {
                return None;
            }
            done = figures;
            Some(Ok(()))
        });
        let codes = drawn
            .map_err(|_| GpuFallback::Compiling)
            .and_then(|drawn| drawn)
            .and_then(|()| match self.surface.rest_output() {
                Some(output) => self.read(&output.tiles[0].texture, (output.width, output.height)),
                None if rest.reduction.is_none() => Ok(Vec::new()),
                None => Err(GpuFallback::PipelineFailed),
            });
        let counts = self.surface.rest.as_ref().map(|slot| slot.counts().1);
        // Whatever stopped it, the rest's slot goes with its charge.
        self.pipeline.release_rest(&mut self.surface);
        let codes = codes?;
        // The counts' readback, its mapping taken up by the polls that complete it.
        let counts = match counts {
            None => Err(super::histogram::HistogramError::ReadbackFailed),
            Some(counts) => luxforge_testbase::try_wait_for("the picture at rest's counts", || {
                let _ = self.device.poll(wgpu::PollType::Poll);
                match counts.outcome() {
                    super::CountsOutcome::Counting => None,
                    super::CountsOutcome::Ready(counts) => Some(Ok(*counts)),
                    super::CountsOutcome::Failed(error) => Some(Err(error)),
                }
            })
            .unwrap_or(Err(super::histogram::HistogramError::ReadbackFailed)),
        };
        self.retired();
        Ok(RestDrawn {
            codes,
            frames,
            counts,
            figures: done,
        })
    }

    /// Wait, through the test base's one hang-bounded wait, until every resource this surface's
    /// pipeline released has retired: the editor's surface holds one slot and its picture at rest,
    /// never a run of released ones still charged, so the next draw is charged as it would be.
    fn retired(&self) {
        let _ = luxforge_testbase::try_wait_for("the released slots' retirement", || {
            (self
                .pipeline
                .figures
                .retirement_pending
                .load(std::sync::atomic::Ordering::Acquire)
                == 0)
                .then_some(())
        });
    }

    /// `plan`, a region plan whose boundary is derived from `source`, drawn as a picture at rest
    /// draws each of its tiles — a fresh evaluation through a slot of its own, its codes at the
    /// output's first texels, without the clipping marks — and read back: the region's codes, RGBA
    /// row by row.
    pub fn tile(
        &mut self,
        source: &GpuSource,
        plan: &GpuPlan,
    ) -> Result<Vec<[u8; 4]>, GpuFallback> {
        plan.region.ok_or(GpuFallback::PipelineFailed)?;
        self.draw(source, plan).map(|drawn| drawn.codes)
    }

    /// `plan`, whose boundary is derived from `source`, drawn as the surface draws a gesture's
    /// plan or a picture at rest's view plan — a fresh evaluation through a slot of its own, its
    /// codes at the output's first texels — and read back: the output's codes, RGBA row by row,
    /// and its size, the region's for a region plan.
    pub fn draw(&mut self, source: &GpuSource, plan: &GpuPlan) -> Result<Drawn, GpuFallback> {
        let mut slots = self.pipeline.new_surface();
        let mut waited = GpuFallback::Compiling;
        // One frame a look, through the one hang-bounded wait, while its sequence compiles or its
        // source's rows are written.
        let drawn = luxforge_testbase::try_wait_for("a tile's evaluation", || {
            self.hand(source);
            match self
                .pipeline
                .evaluate_lit(&mut slots, &self.device, &self.queue, plan, None)
            {
                Ok(_) => Some(Ok(())),
                Err(waiting @ (GpuFallback::Compiling | GpuFallback::SourceUploading { .. })) => {
                    waited = waiting;
                    None
                }
                Err(fallback) => Some(Err(fallback)),
            }
        })
        .unwrap_or(Err(waited));
        let codes = drawn.and_then(|()| {
            let slot = slots.gpu.as_ref().ok_or(GpuFallback::PipelineFailed)?;
            let output = slot.output();
            let size = plan.region.map_or((output.width, output.height), |region| {
                let [x0, y0, x1, y1] = region.rect;
                (x1 - x0, y1 - y0)
            });
            let [tile] = output.tiles.as_slice() else {
                return Err(GpuFallback::PipelineFailed);
            };
            Ok(Drawn {
                codes: self.read(&tile.texture, size)?,
                size,
            })
        });
        self.pipeline.release_gpu(&mut slots);
        self.retired();
        codes
    }

    /// `plans` drawn one after another through one slot, as a drag's ticks are handed to the
    /// surface's slot, each with the change its caller measured since the one before, and the last
    /// tick's codes read back after the timing, for a caller to hold to a fresh draw: the time each
    /// tick took from handing the source to the GPU having finished it, the queue waited on after
    /// the evaluation's submissions, so the GPU's work is counted and not only its encoding. The
    /// first plan is the drag's first tick, drawn untimed, waiting out its sequences' compile and
    /// the source's upload; every later one is timed, one figure each. The fallback that stops a
    /// tick otherwise.
    pub fn ticks(
        &mut self,
        source: &GpuSource,
        plans: &[(GpuPlan, Option<super::GpuChange>)],
    ) -> Result<Ticked, GpuFallback> {
        let mut slots = self.pipeline.new_surface();
        let timed = self
            .ticks_through(&mut slots, source, plans)
            .and_then(|times| {
                let (plan, _) = plans.last().ok_or(GpuFallback::PipelineFailed)?;
                let slot = slots.gpu.as_ref().ok_or(GpuFallback::PipelineFailed)?;
                let output = slot.output();
                let size = plan.region.map_or((output.width, output.height), |region| {
                    let [x0, y0, x1, y1] = region.rect;
                    (x1 - x0, y1 - y0)
                });
                let [tile] = output.tiles.as_slice() else {
                    return Err(GpuFallback::PipelineFailed);
                };
                let last = self.read(&tile.texture, size)?;
                Ok(Ticked { times, last })
            });
        self.pipeline.release_gpu(&mut slots);
        self.retired();
        timed
    }

    /// [`Self::ticks`] through `slots`, which the caller releases.
    fn ticks_through(
        &mut self,
        slots: &mut State,
        source: &GpuSource,
        plans: &[(GpuPlan, Option<super::GpuChange>)],
    ) -> Result<Vec<std::time::Duration>, GpuFallback> {
        let Some((first, _)) = plans.first() else {
            return Ok(Vec::new());
        };
        let mut waited = GpuFallback::Compiling;
        luxforge_testbase::try_wait_for("a drag's first tick", || {
            self.hand(source);
            match self
                .pipeline
                .evaluate_lit(slots, &self.device, &self.queue, first, None)
            {
                Ok(_) => Some(Ok(())),
                Err(waiting @ (GpuFallback::Compiling | GpuFallback::SourceUploading { .. })) => {
                    waited = waiting;
                    None
                }
                Err(fallback) => Some(Err(fallback)),
            }
        })
        .unwrap_or(Err(waited))?;
        self.finished()?;
        let mut times = Vec::with_capacity(plans.len());
        for (plan, change) in &plans[1..] {
            let started = std::time::Instant::now();
            self.hand(source);
            self.pipeline
                .evaluate_lit(slots, &self.device, &self.queue, plan, *change)?;
            self.finished()?;
            times.push(started.elapsed());
        }
        Ok(times)
    }

    /// Wait until the GPU has finished everything submitted so far.
    fn finished(&self) -> Result<(), GpuFallback> {
        let index = self.queue.submit([]);
        loop {
            match self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index.clone()),
                timeout: None,
            }) {
                Ok(_) => return Ok(()),
                Err(wgpu::PollError::Timeout) => {}
                Err(_) => return Err(GpuFallback::DeviceLost),
            }
        }
    }

    /// The `size` texels at the origin of `texture`, four bytes each, read back: the submission
    /// waited for.
    fn read(&self, texture: &wgpu::Texture, size: (u32, u32)) -> Result<Vec<[u8; 4]>, GpuFallback> {
        Ok(self
            .read_bytes(texture, size, 4)?
            .chunks_exact(4)
            .map(|texel| [texel[0], texel[1], texel[2], texel[3]])
            .collect())
    }

    /// Let go of every light the surface's pipeline keeps, so the next picture at rest computes
    /// its lights again: for a measurement drawing one picture twice.
    pub fn forget_lights(&mut self) {
        self.pipeline.retire_kept_lights();
    }

    /// The lights the surface's pipeline keeps ([`super::light::LightCache`]), `[r, g, b, 1]`
    /// each, read back in the order they were first kept: what a picture at rest's staged sweep
    /// computed from a stage texture, for a test to hold to the CPU's.
    pub fn kept_lights(&self) -> Result<Vec<[f32; 4]>, GpuFallback> {
        let mut lights = Vec::new();
        for texture in self.pipeline.kept_lights.textures() {
            let bytes = self.read_bytes(texture, (1, 1), 16)?;
            lights.push(std::array::from_fn(|channel| {
                f32::from_le_bytes(
                    bytes[channel * 4..channel * 4 + 4]
                        .try_into()
                        .expect("four bytes"),
                )
            }));
        }
        Ok(lights)
    }

    /// `size` texels of `texture` at its origin, `texel` bytes each, read back unpadded.
    fn read_bytes(
        &self,
        texture: &wgpu::Texture,
        size: (u32, u32),
        texel: u32,
    ) -> Result<Vec<u8>, GpuFallback> {
        let (width, height) = size;
        let row = width * texel;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.headless.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luxforge.headless.read"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
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
        // The copy's submission waited for, and its mapping taken up by the poll that completes it,
        // through the test base's one hang-bounded wait: a wait the driver times out on a loaded
        // host is waited again, and only a lost device or a failed mapping ends it.
        luxforge_testbase::try_wait_for("a readback's mapping", || {
            match self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index.clone()),
                timeout: None,
            }) {
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(_) => return Some(Err(GpuFallback::DeviceLost)),
            }
            match receiver.try_recv() {
                Ok(Ok(())) => Some(Ok(())),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(GpuFallback::DeviceLost))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            }
        })
        .map_err(|_| GpuFallback::DeviceLost)??;
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
