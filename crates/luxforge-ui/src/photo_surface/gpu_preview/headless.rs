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
use super::super::{PhotoPipeline, SurfaceSlots};
use super::{GpuFallback, GpuPlan, GpuRest, GpuSource, answered};
use std::sync::{Arc, mpsc};

/// A device of this host's default adapter at `limits`, its queue and the adapter's description,
/// or `None` after printing that `run` was skipped: a run without one drew nothing and is not GPU
/// evidence.
pub fn device(
    run: &str,
    limits: wgpu::Limits,
) -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let Some(Ok(adapter)) =
        answered(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        eprintln!("skipped: no GPU adapter; {run} ran nothing and is not GPU evidence");
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

/// A picture at rest drawn headless: the view's codes, RGBA row by row, and the frames it took,
/// those that waited for a compile or the upload among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestDrawn {
    pub codes: Vec<[u8; 4]>,
    pub frames: u32,
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
    pipeline: PhotoPipeline,
    surface: SurfaceSlots,
}

impl HeadlessSurface {
    /// A surface on `device`, its pipeline drawing to an sRGB target as the desktop's does and
    /// counting into figures of its own.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let pipeline = PhotoPipeline::with_figures(
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
            Some(Ok(()))
        });
        let codes = drawn
            .map_err(|_| GpuFallback::Compiling)
            .and_then(|drawn| drawn)
            .and_then(|()| {
                let output = self
                    .surface
                    .rest_output()
                    .ok_or(GpuFallback::PipelineFailed)?;
                self.read(&output.tiles[0].texture, (output.width, output.height))
            });
        // Whatever stopped it, the rest's slot goes with its charge.
        self.pipeline.release_rest(&mut self.surface);
        Ok(RestDrawn {
            codes: codes?,
            frames,
        })
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
                .evaluate(&mut slots, &self.device, &self.queue, plan, None)
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
        codes
    }

    /// The `size` texels at the origin of `texture`, four bytes each, read back: the submission
    /// waited for.
    fn read(&self, texture: &wgpu::Texture, size: (u32, u32)) -> Result<Vec<[u8; 4]>, GpuFallback> {
        let (width, height) = size;
        let row = width * 4;
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
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|_| GpuFallback::DeviceLost)?;
        match receiver.try_recv() {
            Ok(Ok(())) => {}
            _ => return Err(GpuFallback::DeviceLost),
        }
        let mapped = readback.slice(..).get_mapped_range();
        let mut codes = Vec::with_capacity((width * height) as usize);
        for line in mapped.chunks_exact(padded as usize) {
            codes.extend(
                line[..row as usize]
                    .chunks_exact(4)
                    .map(|texel| [texel[0], texel[1], texel[2], texel[3]]),
            );
        }
        drop(mapped);
        readback.unmap();
        Ok(codes)
    }
}
