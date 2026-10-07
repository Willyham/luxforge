//! The histogram and clipping counts on the GPU: a reduction of rectangles of 8-bit output codes
//! into the counts the core's analysis reducer gives the same codes, integer for integer, and their
//! readback to the CPU without a wait.
//!
//! The counts the inspector shows and `analysis.request` answers wherever the GPU draws the stack
//! ([GPU-first](../../../../../docs/design/gpu-first.md), stage 2): a surface keeps one reduction
//! for its pictures at rest, which counts each tile of the stack at full resolution as it is drawn
//! ([`super::rest`]), and one for a gesture's ticks, which counts the frame each tick drew.
//!
//! # What it counts
//!
//! Each dispatch counts one rectangle of one texture of the output's own `rgba8unorm` format
//! ([`OUTPUT_FORMAT`]), read through that format, never an sRGB-typed view, so each texel is its
//! 8-bit code exactly: the codes the chain's last pass writes. Alpha is never read. A texture that
//! holds the clipping marks is not output codes — a plan whose last step is
//! [`GpuStep::Clipping`](super::GpuStep::Clipping) lays the overlay's colours over its output — so
//! what is reduced must be rendered without them.
//!
//! The counts buffer holds [`COUNT_WORDS`] `u32` words, 3,096 bytes, in the reducer's own split:
//! the red, green and blue channels' 256 bins, then the pixels by the OR of their channels'
//! endpoints and by the AND (shadow only, highlight only, both), the class with no endpoint
//! uncounted. [`Counts`] derives the report's eleven counters from them as the reducer does: a
//! channel's code-0 and code-255 counts are its first and last bins, `any_shadow` is the OR's
//! shadow-only and both classes, `both` the OR's both class, and so on.
//!
//! # Bounds and the budget
//!
//! The reduction holds the counts buffer, a 16-byte rectangle and a table of every value a
//! rectangle can name, 0 to the device's texture limit (32,776 bytes at 8,192 texels); each
//! readback adds a staging copy of the counts, 3,096 bytes, until the GPU has finished with it.
//! Nothing scales with the image. Each is charged to the GPU-preview budget through the stage's own
//! charge before it is created, and a charge that would pass the budget is refused with
//! [`HistogramError::BudgetExceeded`]; each leaves through the photo surface's retirement worker,
//! charged until the GPU is done with it, as the stage's own buffers do.
//!
//! # Determinism
//!
//! Every count is an integer addition: a workgroup counts its texels into bins of its own with
//! `atomicAdd`, then adds each bin that is not zero into the buffer with `atomicAdd`. Addition
//! commutes, so the counts are the same whatever order the workgroups, the tiles or the
//! submissions run in. A `u32` counter rises by at most one a pixel, and a reduction refuses to
//! count past [`MAX_PIXELS`], so none overflows.
//!
//! # A stage in tiles
//!
//! [`HistogramReduction::clear`] zeroes the counts once; [`HistogramReduction::reduce`] then adds
//! one tile, the rectangle of its texture that is its own (past its halo), as often as there are
//! tiles, each in its own texture, so a stage larger than a texture may be — 8,192 texels a side
//! under the toolkit's limits — is counted tile by tile and never held whole. A texel outside the
//! rectangle is never counted. Each tile's rectangle reaches the kernel through the encoder, copied
//! word by word from the table into the rectangle's buffer just before the tile's dispatch, so the
//! commands run in order and any number of tiles may share a submission or each have their own: a
//! queue write would land before every command of its submission, giving all of a submission's
//! tiles the last one's rectangle.
//!
//! # The readback
//!
//! [`HistogramReduction::read_back`] encodes the copy of the counts into a staging buffer into the
//! caller's last encoder, schedules the staging buffer's mapping on that same submission
//! (`map_buffer_on_submit`), submits it and hands the staging buffer to the retirement worker. The
//! worker already blocks on its channel while nothing retires and, while something does, polls the
//! device without waiting every 2 ms (performance rule 8): that poll, or the next frame's own
//! submit, runs the mapping's callback, which decodes the counts, unmaps and wakes the desktop
//! through the surface waker (`set_surface_waker`), whose stream the desktop keeps installed while
//! a photograph is open. The callback wakes it itself, after the counts are stored, because the
//! worker's own wake when it ends the retirement and discharges the staging copy can come first:
//! wgpu fires a mapping's callback before a later work-done callback only when one poll or submit
//! finds both, and a frame's submit on the interface thread and the worker's poll may each find
//! one. No timer, thread or wait is added, and the UI thread only encodes and submits.
//! [`HistogramReadback::poll`] answers nothing while the counts are on their
//! way, then the counts or why there are none: the budget, a failed mapping, counts that do not add
//! up to the pixels reduced since the clear, or a lost device. It never answers counts it did not
//! read for this readback.
//!
//! [`HistogramReduction::new`] compiles the kernel on the calling thread, checked with `naga` first
//! and inside error scopes read without waiting, as the stage compiles its passes; it belongs where
//! nothing waits on it, as the mip pass is built with the pipeline.

use super::{
    GpuFallback, GpuStageState, Held, OUTPUT_FORMAT, RetiredPreview, answered, finish_retirement,
    validate,
};
use crate::{Executor, Retired, Signals};
use std::{
    borrow::Cow,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// One channel's bins: one for each 8-bit code.
pub const BINS: usize = 256;

/// The classes a pixel's channel endpoints put it in, past the one with none, which the report
/// never reads: a channel at code 0 and none at 255, one at 255 and none at 0, and both.
const CLASSES: usize = 3;

/// Where the classes by the OR of a pixel's channels' endpoints start: after the three channels'
/// bins.
const ANY_WORD: usize = 3 * BINS;

/// Where the classes by the AND start. No pixel has all three channels at both ends, so the last of
/// them stays zero; it is counted all the same, as the reducer keeps it, so the two read alike.
const ALL_WORD: usize = ANY_WORD + CLASSES;

/// The counts buffer in `u32` words: the red, green and blue bins, then the classes by the OR and
/// by the AND of the channels' endpoints. It bounds the buffer every tile adds into and each
/// readback's staging copy.
pub const COUNT_WORDS: usize = ALL_WORD + CLASSES;

/// [`COUNT_WORDS`] in bytes, 3,096: what the counts buffer and each staging copy charge the budget.
pub const COUNT_BYTES: u64 = COUNT_WORDS as u64 * 4;

/// The most pixels one reduction counts between two clears: a `u32` counter rises by at most one a
/// pixel, so up to this none can overflow. The largest evaluated frame, 512 MiB of RGBA8
/// (134,217,728 pixels), is a thirty-second of it; a tile that would pass it is refused.
pub const MAX_PIXELS: u64 = u32::MAX as u64;

/// The lanes of a workgroup along each axis, 256 in all.
const LANES: u32 = 16;

/// The texels each lane counts along each axis, a workgroup's width of lanes apart.
const REACH: u32 = 4;

/// The texels a workgroup covers along each axis.
const SPAN: u32 = LANES * REACH;

/// How many times each lane walks the workgroup's bins, to zero them and to add them in.
const SWEEPS: u32 = (COUNT_WORDS as u32).div_ceil(LANES * LANES);

// Each lane's sweeps reach every word of the bins.
const _: () = assert!(SWEEPS * LANES * LANES >= COUNT_WORDS as u32);

/// The rectangle a dispatch counts, four `u32` words: its first texel's column and row, its width
/// and its height.
const RECT_BYTES: u64 = 16;

/// The kernel's entry point.
const ENTRY: &str = "lf_histogram_reduce";

const LABEL: &str = "luxforge.gpu_preview.histogram";

/// A rectangle of a texture's texels: its first texel and its size. A tile's is its interior, the
/// texels past its halo that are its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistogramRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl HistogramRect {
    /// Every texel of `texture`.
    pub fn whole(texture: &wgpu::Texture) -> Self {
        Self {
            x: 0,
            y: 0,
            width: texture.width(),
            height: texture.height(),
        }
    }

    /// The texels it holds.
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// One reduction's counts in the report's names: each channel's 256 bins, the clipping counters
/// derived from the endpoint classes as the reducer derives them, and the pixels counted. The
/// counters mean what the report's do: `r0` the pixels whose red is code 0, `any_shadow` those with
/// any channel at 0, `all_highlight` those with every channel at 255, `both` those with a channel
/// at each end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts {
    pub r: [u64; BINS],
    pub g: [u64; BINS],
    pub b: [u64; BINS],
    pub r0: u64,
    pub g0: u64,
    pub b0: u64,
    pub r255: u64,
    pub g255: u64,
    pub b255: u64,
    pub any_shadow: u64,
    pub any_highlight: u64,
    pub all_shadow: u64,
    pub all_highlight: u64,
    pub both: u64,
    /// The pixels the reduction counted since its clear: every channel's bins add up to it. A
    /// stage's counts are whole only when it is the stage's pixel count.
    pub pixels: u64,
}

impl Counts {
    /// The counts in `bytes`, the counts buffer's little-endian words as read back, of a reduction
    /// that counted `pixels`: refused when a channel's bins do not add up to `pixels`, which a
    /// clear or a tile encoded but never submitted would leave.
    fn read(bytes: &[u8], pixels: u64) -> Result<Self, HistogramError> {
        if bytes.len() as u64 != COUNT_BYTES {
            return Err(HistogramError::ReadbackFailed);
        }
        let word = |index: usize| {
            let at = index * 4;
            u64::from(u32::from_le_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]))
        };
        let channel =
            |first: usize| -> [u64; BINS] { std::array::from_fn(|code| word(first + code)) };
        let (r, g, b) = (channel(0), channel(BINS), channel(2 * BINS));
        for (index, bins) in [&r, &g, &b].into_iter().enumerate() {
            let counted = bins.iter().sum();
            if counted != pixels {
                return Err(HistogramError::Inconsistent {
                    channel: index,
                    counted,
                    pixels,
                });
            }
        }
        let [any_shadow_only, any_highlight_only, both] =
            [0, 1, 2].map(|class| word(ANY_WORD + class));
        let [all_shadow_only, all_highlight_only, all_both] =
            [0, 1, 2].map(|class| word(ALL_WORD + class));
        Ok(Self {
            r0: r[0],
            g0: g[0],
            b0: b[0],
            r255: r[BINS - 1],
            g255: g[BINS - 1],
            b255: b[BINS - 1],
            any_shadow: any_shadow_only + both,
            any_highlight: any_highlight_only + both,
            all_shadow: all_shadow_only + all_both,
            all_highlight: all_highlight_only + all_both,
            both,
            r,
            g,
            b,
            pixels,
        })
    }
}

/// Why a reduction or a readback has no counts. Each is reported, never answered with counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistogramError {
    /// The GPU stage cannot run on the pipeline's device, as its capability check answered, or the
    /// launch refused it.
    Unavailable(GpuStageState),
    /// The device's limits do not hold the kernel: a 16 × 16 workgroup with its bins in workgroup
    /// memory, a dispatch across the widest texture, a uniform, a storage buffer and a texture.
    Unsupported,
    /// The device was lost. Nothing is counted on it again, and a readback it did not finish
    /// carries no counts.
    DeviceLost,
    /// Allocating `requested` bytes would pass the GPU-preview budget.
    BudgetExceeded {
        requested: u64,
        in_use: u64,
        budget: u64,
    },
    /// `naga` refused the kernel or wgpu its pipeline.
    PipelineFailed(String),
    /// The texture does not hold output codes the kernel reads exactly, and why.
    Texture(String),
    /// The rectangle is not within the texture, or names a texel past the device's texture limit.
    Rectangle {
        rect: HistogramRect,
        width: u32,
        height: u32,
    },
    /// Counting the rectangle would bring the pixels counted since the clear to `pixels`, past
    /// [`MAX_PIXELS`].
    PixelLimit { pixels: u64 },
    /// The staging copy could not be mapped for reading.
    ReadbackFailed,
    /// A channel's bins read back count `counted` pixels, not the `pixels` reduced since the clear.
    Inconsistent {
        channel: usize,
        counted: u64,
        pixels: u64,
    },
}

impl std::fmt::Display for HistogramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(state) => {
                write!(f, "the GPU stage is unavailable: {}", state.as_str())
            }
            Self::Unsupported => {
                f.write_str("the device's limits do not hold the histogram kernel")
            }
            Self::DeviceLost => f.write_str("the device was lost"),
            Self::BudgetExceeded {
                requested,
                in_use,
                budget,
            } => write!(
                f,
                "{requested} bytes would pass the GPU-preview budget, {in_use} of {budget} in use"
            ),
            Self::PipelineFailed(error) => write!(f, "the histogram kernel failed: {error}"),
            Self::Texture(why) => write!(f, "the texture cannot be counted: {why}"),
            Self::Rectangle {
                rect,
                width,
                height,
            } => write!(
                f,
                "the rectangle {rect:?} is not within the {width} x {height} texture"
            ),
            Self::PixelLimit { pixels } => write!(
                f,
                "counting {pixels} pixels would pass the reduction's limit of {MAX_PIXELS}"
            ),
            Self::ReadbackFailed => f.write_str("the counts could not be mapped for reading"),
            Self::Inconsistent {
                channel,
                counted,
                pixels,
            } => write!(
                f,
                "channel {channel}'s bins count {counted} pixels, not the {pixels} reduced since \
                 the clear"
            ),
        }
    }
}

impl std::error::Error for HistogramError {}

/// The histogram kernel on one pipeline's device, with its counts buffer: clear it, add tiles, read
/// it back ([module documentation](self)). Its buffers are charged to the GPU-preview budget from
/// its creation until the GPU has finished with them after it is dropped; drop it once its last
/// encoder is submitted.
pub struct HistogramReduction {
    device: wgpu::Device,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// Every value a rectangle can name, word `v` holding `v`, 0 to `limit`: written once, when the
    /// buffer is created, and the source each tile's rectangle is copied from.
    table: wgpu::Buffer,
    /// The largest value the table holds: the device's texture limit.
    limit: u32,
    /// The rectangle the next dispatch counts, copied in from the table just before it.
    rect: wgpu::Buffer,
    counts: wgpu::Buffer,
    /// The pixels counted since the last clear.
    pixels: u64,
    /// The figures the budget is charged in, and the pipeline's retirement worker.
    figures: Arc<Signals>,
    retirement: mpsc::Sender<Retired>,
}

impl HistogramReduction {
    /// The kernel and its buffers on `device`, the device `pipeline` draws with, charged to its
    /// GPU-preview budget before anything is created. Compiles the kernel on the calling thread.
    pub fn new(pipeline: &Executor, device: &wgpu::Device) -> Result<Self, HistogramError> {
        let figures = Arc::clone(&pipeline.figures);
        match figures.preview.stage_state() {
            GpuStageState::Available => {}
            GpuStageState::DeviceLost => return Err(HistogramError::DeviceLost),
            state => return Err(HistogramError::Unavailable(state)),
        }
        let limits = device.limits();
        if !supported(&limits) {
            return Err(HistogramError::Unsupported);
        }
        let limit = limits.max_texture_dimension_2d;
        let table_bytes = table_words(limit) * 4;
        let charged = table_bytes + RECT_BYTES + COUNT_BYTES;
        charge(&figures, charged)?;
        let (compiled, layout) = match kernel(device) {
            Ok(compiled) => compiled,
            Err(error) => {
                figures.preview.discharge(charged);
                return Err(HistogramError::PipelineFailed(error));
            }
        };
        let buffer = |size, usage, mapped_at_creation| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(LABEL),
                size,
                usage,
                mapped_at_creation,
            })
        };
        let table = buffer(table_bytes, wgpu::BufferUsages::COPY_SRC, true);
        {
            let mut words = table.slice(..).get_mapped_range_mut();
            for (value, word) in (0u32..).zip(words.chunks_exact_mut(4)) {
                word.copy_from_slice(&value.to_le_bytes());
            }
        }
        table.unmap();
        let rect = buffer(
            RECT_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let counts = buffer(
            COUNT_BYTES,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            false,
        );
        Ok(Self {
            device: device.clone(),
            pipeline: compiled,
            layout,
            table,
            limit,
            rect,
            counts,
            pixels: 0,
            figures,
            retirement: pipeline.retirement_sender.clone(),
        })
    }

    /// What its buffers charge the GPU-preview budget: the table, the rectangle and the counts. A
    /// readback's staging copy adds [`COUNT_BYTES`] until the GPU has finished with it.
    pub fn charged_bytes(&self) -> u64 {
        self.table.size() + self.rect.size() + self.counts.size()
    }

    /// The pixels counted since the last clear.
    pub fn pixels(&self) -> u64 {
        self.pixels
    }

    /// Zero the counts in `encoder`, ahead of a reduction's first tile.
    pub fn clear(&mut self, encoder: &mut wgpu::CommandEncoder) {
        encoder.clear_buffer(&self.counts, 0, None);
        self.pixels = 0;
    }

    /// Add `rect` of `texture` to the counts in `encoder`, after whatever the encoder holds that
    /// writes the texture: one dispatch, its rectangle copied in from the table just before it. An
    /// empty rectangle counts nothing. Refused, with nothing encoded, for a texture that is not
    /// output codes, a rectangle outside it, a count past [`MAX_PIXELS`] or a lost device.
    pub fn reduce(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        rect: HistogramRect,
    ) -> Result<(), HistogramError> {
        self.usable()?;
        readable(texture)?;
        let (width, height) = (texture.width(), texture.height());
        let within = |start: u32, extent: u32, side: u32| {
            start.checked_add(extent).is_some_and(|end| end <= side)
        };
        if !within(rect.x, rect.width, width)
            || !within(rect.y, rect.height, height)
            || width.max(height) > self.limit
        {
            return Err(HistogramError::Rectangle {
                rect,
                width,
                height,
            });
        }
        if rect.pixels() == 0 {
            return Ok(());
        }
        let pixels = self.pixels + rect.pixels();
        if pixels > MAX_PIXELS {
            return Err(HistogramError::PixelLimit { pixels });
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(LABEL),
            format: Some(OUTPUT_FORMAT),
            dimension: Some(wgpu::TextureViewDimension::D2),
            mip_level_count: Some(1),
            array_layer_count: Some(1),
            ..wgpu::TextureViewDescriptor::default()
        });
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(LABEL),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.rect.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.counts.as_entire_binding(),
                },
            ],
        });
        for (word, value) in (0u64..).zip([rect.x, rect.y, rect.width, rect.height]) {
            encoder.copy_buffer_to_buffer(
                &self.table,
                u64::from(value) * 4,
                &self.rect,
                word * 4,
                4,
            );
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(LABEL),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(rect.width.div_ceil(SPAN), rect.height.div_ceil(SPAN), 1);
        }
        self.pixels = pixels;
        Ok(())
    }

    /// Copy the counts into a staging buffer in `encoder`, map it on the same submission, submit
    /// `encoder` and hand the staging buffer to the retirement worker, whose polling completes the
    /// mapping, whose callback stores the counts and wakes the desktop; answer the handle the counts
    /// arrive through ([module documentation](self#the-readback)). `encoder` is submitted whatever
    /// happens: the staging copy refused by the budget, or a lost device, makes the handle answer
    /// the error at once, and wakes the desktop too.
    #[must_use = "the counts arrive only through the readback"]
    pub fn read_back(
        &mut self,
        queue: &wgpu::Queue,
        mut encoder: wgpu::CommandEncoder,
    ) -> HistogramReadback {
        let outcome = Arc::new(Mutex::new(None));
        let readback = HistogramReadback {
            outcome: Arc::clone(&outcome),
            lost: Arc::clone(&self.figures.preview.lost),
        };
        if let Err(error) = self
            .usable()
            .and_then(|()| charge(&self.figures, COUNT_BYTES))
        {
            queue.submit([encoder.finish()]);
            *outcome.lock().expect("histogram readback lock") = Some(Err(error));
            self.figures.wake();
            return readback;
        }
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(LABEL),
            size: COUNT_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&self.counts, 0, &staging, 0, COUNT_BYTES);
        let mapped = staging.clone();
        let pixels = self.pixels;
        let wake = self.figures.wake_callback;
        encoder.map_buffer_on_submit(&staging, wgpu::MapMode::Read, .., move |result| {
            // On whichever thread polls the device or submits next: a short decode, then unmapped.
            let counts = match result {
                Ok(()) => {
                    let bytes = mapped.slice(..).get_mapped_range();
                    let counts = Counts::read(&bytes, pixels);
                    drop(bytes);
                    mapped.unmap();
                    counts
                }
                Err(_) => Err(HistogramError::ReadbackFailed),
            };
            *outcome.lock().expect("histogram readback lock") = Some(counts);
            // After the counts are stored, so no wake reaches the desktop before them.
            wake();
        });
        queue.submit([encoder.finish()]);
        // After the submission, so the worker's work-done callback is registered after the mapping.
        retire(&self.figures, &self.retirement, staging, COUNT_BYTES);
        readback
    }

    /// Whether the device can still be used.
    fn usable(&self) -> Result<(), HistogramError> {
        if self.figures.preview.lost.load(Ordering::Acquire) {
            Err(HistogramError::DeviceLost)
        } else {
            Ok(())
        }
    }
}

impl Drop for HistogramReduction {
    /// Its buffers retire through the worker, still charged until the GPU has finished with them.
    fn drop(&mut self) {
        for buffer in [&self.table, &self.rect, &self.counts] {
            retire(
                &self.figures,
                &self.retirement,
                buffer.clone(),
                buffer.size(),
            );
        }
    }
}

/// The counts of one [`HistogramReduction::read_back`], once the GPU has finished and they are
/// decoded. Every clone answers the same.
#[derive(Clone, Debug)]
pub struct HistogramReadback {
    outcome: Arc<Mutex<Option<Result<Counts, HistogramError>>>>,
    /// The device's lost flag, which its lost callback sets.
    lost: Arc<AtomicBool>,
}

impl HistogramReadback {
    /// `None` while the counts are on their way; then the counts, or why there are none. A device
    /// lost before they were decoded answers [`HistogramError::DeviceLost`], never earlier counts.
    /// Reads nothing from the GPU and never waits.
    pub fn poll(&self) -> Option<Result<Counts, HistogramError>> {
        let outcome = self
            .outcome
            .lock()
            .expect("histogram readback lock")
            .clone();
        outcome.or_else(|| {
            self.lost
                .load(Ordering::Acquire)
                .then_some(Err(HistogramError::DeviceLost))
        })
    }
}

/// Whether a device's limits hold the kernel: a 16 × 16 workgroup with its bins in workgroup
/// memory, a dispatch across the widest texture the device allows, and a uniform, a storage buffer
/// of the counts and a sampled texture.
fn supported(limits: &wgpu::Limits) -> bool {
    limits.max_compute_invocations_per_workgroup >= LANES * LANES
        && limits.max_compute_workgroup_size_x >= LANES
        && limits.max_compute_workgroup_size_y >= LANES
        && u64::from(limits.max_compute_workgroup_storage_size) >= COUNT_BYTES
        && limits.max_compute_workgroups_per_dimension
            >= limits.max_texture_dimension_2d.div_ceil(SPAN)
        && limits.max_uniform_buffers_per_shader_stage >= 1
        && limits.max_storage_buffers_per_shader_stage >= 1
        && u64::from(limits.max_storage_buffer_binding_size) >= COUNT_BYTES
        && limits.max_sampled_textures_per_shader_stage >= 1
}

/// The words of the value table on a device whose textures are `limit` texels a side: every value
/// from 0 to `limit`, padded to an even count so the table maps whole.
fn table_words(limit: u32) -> u64 {
    (u64::from(limit) + 1).next_multiple_of(2)
}

/// Charge `bytes` to the GPU-preview budget, or say why not.
fn charge(figures: &Signals, bytes: u64) -> Result<(), HistogramError> {
    figures
        .preview
        .charge(bytes)
        .map_err(|refusal| match refusal {
            GpuFallback::BudgetExceeded {
                requested,
                in_use,
                budget,
            } => HistogramError::BudgetExceeded {
                requested,
                in_use,
                budget,
            },
            // The charge refuses with the budget alone.
            _ => HistogramError::BudgetExceeded {
                requested: bytes,
                in_use: figures.preview.in_use(),
                budget: figures.preview.budget(),
            },
        })
}

/// Hand `buffer` to the pipeline's retirement worker, still charged `bytes`, as the stage hands a
/// buffer it outgrew: the worker holds it until the GPU has finished every submission so far,
/// polling the device meanwhile, then drops it, discharges it and wakes the desktop.
fn retire(figures: &Signals, retirement: &mpsc::Sender<Retired>, buffer: wgpu::Buffer, bytes: u64) {
    figures.retirement_pending.fetch_add(1, Ordering::AcqRel);
    if let Err(error) = retirement.send(Retired::Preview(RetiredPreview {
        held: Held::Buffer(buffer),
        bytes,
        scratch: 0,
    })) && let Retired::Preview(retired) = error.0
    {
        // The worker has ended with the device: its resources are gone with it.
        finish_retirement(figures, retired, true);
    }
}

/// Whether `texture` holds output codes the kernel reads exactly: a single-sampled 2D texture of
/// one layer, in the output's own `rgba8unorm` format, that can be bound for reading.
fn readable(texture: &wgpu::Texture) -> Result<(), HistogramError> {
    let why = if texture.format() != OUTPUT_FORMAT {
        format!(
            "its format is {:?}, not the output codes' {OUTPUT_FORMAT:?}",
            texture.format()
        )
    } else if !texture
        .usage()
        .contains(wgpu::TextureUsages::TEXTURE_BINDING)
    {
        "it cannot be bound for reading".to_owned()
    } else if texture.dimension() != wgpu::TextureDimension::D2
        || texture.depth_or_array_layers() != 1
        || texture.sample_count() != 1
    {
        "it is not a single-sampled 2D texture of one layer".to_owned()
    } else {
        return Ok(());
    };
    Err(HistogramError::Texture(why))
}

/// The kernel's text: the figures it names, from the constants above, then `histogram.wgsl`.
fn source() -> String {
    format!(
        "const LF_HISTOGRAM_BINS: u32 = {BINS}u;\n\
         const LF_HISTOGRAM_ANY: u32 = {ANY_WORD}u;\n\
         const LF_HISTOGRAM_ALL: u32 = {ALL_WORD}u;\n\
         const LF_HISTOGRAM_WORDS: u32 = {COUNT_WORDS}u;\n\
         const LF_HISTOGRAM_LANES: u32 = {LANES}u;\n\
         const LF_HISTOGRAM_REACH: u32 = {REACH}u;\n\
         const LF_HISTOGRAM_SWEEPS: u32 = {SWEEPS}u;\n\n{}",
        include_str!("histogram.wgsl")
    )
}

/// The kernel's pipeline and its group's layout: the text checked with `naga` as the stage checks
/// its own, then compiled inside error scopes that are read without waiting.
fn kernel(device: &wgpu::Device) -> Result<(wgpu::ComputePipeline, wgpu::BindGroupLayout), String> {
    let module = validate(&source())?;
    let buffer = |binding, ty, bytes| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(bytes),
        },
        count: None,
    };
    device.push_error_scope(wgpu::ErrorFilter::Internal);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(LABEL),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                // Read with `textureLoad`, which needs no filtering.
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            buffer(1, wgpu::BufferBindingType::Uniform, RECT_BYTES),
            buffer(
                2,
                wgpu::BufferBindingType::Storage { read_only: false },
                COUNT_BYTES,
            ),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(LABEL),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(LABEL),
        source: wgpu::ShaderSource::Naga(Cow::Owned(module)),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(LABEL),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some(ENTRY),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let validation = answered(device.pop_error_scope());
    let internal = answered(device.pop_error_scope());
    match (validation, internal) {
        (Some(None), Some(None)) => Ok((pipeline, layout)),
        (Some(Some(error)), _) | (_, Some(Some(error))) => Err(error.to_string()),
        _ => Err("the kernel's error scopes were not answered without waiting".into()),
    }
}

#[cfg(test)]
#[path = "histogram_tests.rs"]
mod tests;
