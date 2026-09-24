use crate::{
    Error, ErrorKind, Layer, Recipe, SnapshotId, SourceImage,
    mask_field::{MaskField, MaskSampling},
    modules::{
        ColorOperation, ExactGeometry, ModuleRegistry, Parallelism, Processing, Region, Resample,
        SpatialOperation, Stage,
    },
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub mod linear;
pub mod spatial;
pub(crate) use linear::render_linear_proxy_cancellable;
pub use linear::{
    LinearImage, LinearSettings, WhiteBalanceApproximation, render_linear,
    render_linear_cancellable, sample_linear,
};
pub use spatial::SpatialBudget;
use spatial::{
    PRODUCTION_TILE, SpatialPlan, build_reduction, fill_planes, resolve_globals, run_batches,
    run_tile,
};

const PARALLEL_RENDER_PIXELS: u64 = 1_000_000;

/// The detail every cancelled pass carries. The kind is the meaning; nothing about the work itself
/// went wrong, so there is nothing image-specific to say.
pub(crate) const CANCELLED: &str = "superseded by a newer request";

/// A cooperative cancellation token shared between the thread that renders and the one that
/// supersedes it.
///
/// Every rasterizing pass, the resample, the streamed colour pass, the linear row pass and the
/// histogram reducer read it once per row or chunk, so a superseded full-resolution render stops
/// within one chunk of the request instead of competing for the shared Rayon pool with the render
/// that replaced it. A cancelled pass returns [`ErrorKind::Cancelled`] and never a partial frame;
/// scratch reservations are released by their guards on the way out, exactly as on any other early
/// return.
///
/// Both the load and the store are relaxed. The flag is the only thing communicated — no pixels are
/// published through it and no other value depends on the order it becomes visible in — so the one
/// relaxed load per chunk is all the hot loop pays.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token that is never cancelled, for callers with nothing to supersede. It is a fresh token
    /// rather than a shared one, so no caller can cancel another's work through it; the cost is one
    /// `Arc` allocation per render, which is nothing beside the frame that render allocates.
    pub fn never() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(ErrorKind::Cancelled)` when cancelled, for the passes to call per chunk.
    #[inline]
    pub fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::new(ErrorKind::Cancelled, CANCELLED))
        } else {
            Ok(())
        }
    }
}

/// The sRGB transfer function applied backwards, in f64: one encoded channel in `[0, 1]` to linear
/// light. Every table below is built from this one definition.
fn srgb_decode(encoded: f64) -> f64 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// One 8-bit channel code in linear light, at f64 precision: the same transfer function the f32
/// table below is built from, without that table's storage rounding. A module that reasons about
/// colour off the per-pixel path — the neutral picker averages 25 sampled codes and solves a
/// chromaticity from them — decodes through this rather than restating the transfer function.
pub(crate) fn srgb_to_linear_f64(code: u8) -> f64 {
    srgb_decode(f64::from(code) / 255.0)
}

/// The sRGB transfer function over the 256 8-bit channel values: interpolation weights and colour
/// units are applied in linear light, so every channel is decoded through this table first. The
/// entries are computed in f64 and stored as f32, which is the working precision of a colour unit.
static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut table = [0.0; 256];
    for (value, slot) in table.iter_mut().enumerate() {
        *slot = srgb_decode(value as f64 / 255.0) as f32;
    }
    table
});

/// The 255 linear-light thresholds that separate the 256 output codes: `t_k = decode((k − 0.5)/255)`
/// for `k` in `1..=255`, at index `k − 1`. `floor(255·encode(v) + 0.5) = k` exactly when
/// `encode(v)` lies in `[(k − 0.5)/255, (k + 0.5)/255)`, so the code of a clamped value is the
/// number of thresholds at or below it. Computed once in f64, it quantizes the output boundary
/// without a power function per pixel.
static SRGB_CODE_THRESHOLDS: LazyLock<[f64; 255]> = LazyLock::new(|| {
    let mut thresholds = [0.0; 255];
    for (index, slot) in thresholds.iter_mut().enumerate() {
        *slot = srgb_decode((index as f64 + 0.5) / 255.0);
    }
    thresholds
});

/// The sRGB transfer function applied forwards, rounded to the nearest 8-bit value.
fn linear_to_srgb(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// The default aggregate target for transient float scratch: 64 MiB across every active render.
const DEFAULT_SCRATCH_BYTES: u64 = 64 * 1024 * 1024;

/// A process-wide account of the transient float buffers a render streams through. Frames have
/// their own 512 MiB limit; this covers everything that is neither a frame nor the source, so a
/// colour pass never trades a bounded frame for scratch nobody counts. Reservations are taken
/// before the allocation they pay for and released when it is dropped.
///
/// It is a target, not a limit. What keeps scratch inside it is the row chunk, sized so one chunk
/// per pool worker stays well below the target; a chunk that finds the target taken — because more
/// renders overlap than the sizing assumed, or the target was lowered — still runs, and
/// [`Self::peak`] shows the overshoot. Nothing here refuses work.
pub struct ScratchBudget {
    target: AtomicU64,
    used: AtomicU64,
    /// The largest `used` any reservation ever reached, so a process that is idle when it is asked
    /// can still report what the budget actually had to carry. It is only ever raised.
    peak: AtomicU64,
}

static SCRATCH_BUDGET: ScratchBudget = ScratchBudget {
    target: AtomicU64::new(DEFAULT_SCRATCH_BYTES),
    used: AtomicU64::new(0),
    peak: AtomicU64::new(0),
};

impl ScratchBudget {
    /// The one process-wide budget. It is shared state, not a new instance, so this is not the
    /// `Default` trait.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> &'static Self {
        &SCRATCH_BUDGET
    }

    pub fn target(&self) -> u64 {
        self.target.load(Ordering::Relaxed)
    }

    /// Set the target and return the previous one. It changes nothing a reservation does; it is
    /// the figure the high-water mark is read against.
    pub fn set_target(&self, bytes: u64) -> u64 {
        self.target.swap(bytes, Ordering::SeqCst)
    }

    pub fn in_use(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// The high-water mark of [`Self::in_use`] since the process started. A render's scratch is
    /// released as soon as its chunk is done, so `in_use` observed from outside a pass is almost
    /// always zero; this is what makes the budget observable after the fact, including a peak
    /// above the target.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Reserve `bytes`. It never fails. The reservation is released when the returned guard is
    /// dropped, including on an early return from the work it covers.
    fn reserve(&self, bytes: usize) -> Reservation<'_> {
        let bytes = bytes as u64;
        let total = self
            .used
            .fetch_add(bytes, Ordering::SeqCst)
            .saturating_add(bytes);
        // One relaxed maximum beside the reservation that already happened: the counter is only
        // read by diagnostics, so no other value depends on the order it becomes visible in.
        self.peak.fetch_max(total, Ordering::Relaxed);
        Reservation {
            budget: self,
            bytes,
        }
    }
}

struct Reservation<'a> {
    budget: &'a ScratchBudget,
    bytes: u64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// A frame row chunk holds at most this many bytes of float scratch, so the peak is the worker
/// count times this and nothing scales with the image. Rayon runs at most one chunk per worker:
/// 16 workers on the host's M4 Pro reach 16 MiB, comfortably inside the 64 MiB budget, and a
/// 16384-pixel row — the widest side the host accepts — still leaves 5 rows per chunk.
const COLOR_CHUNK_SCRATCH_BYTES: usize = 1024 * 1024;

/// And never more rows than this, so a narrow image does not buffer an arbitrary slice of the
/// frame: 16 rows of a 10000-pixel image is 16 × 10000 × 12 = 1.92 MB of `[f32; 3]`, which is why
/// the byte cap above decides there and the row cap decides for narrow frames.
const COLOR_CHUNK_ROWS: usize = 16;

/// The rows of one streamed colour chunk at this frame width.
fn color_chunk_rows(width: u32) -> usize {
    let row = (width as usize).max(1) * std::mem::size_of::<[f32; 3]>();
    (COLOR_CHUNK_SCRATCH_BYTES / row).clamp(1, COLOR_CHUNK_ROWS)
}

const NON_FINITE_COLOR: &str = "colour processing produced a non-finite value";

/// One maximal run of colour operations in a segment's operation list: every operation between two
/// point replacements, which is the span the host evaluates as one unbroken float pass. Exact
/// geometry is already composed into the segment's single raster pass and commutes with pointwise
/// colour, so it does not break a run; a point replacement does, because a replacement written
/// before a run is processed by it and one written after it is not.
#[derive(Clone, Copy)]
struct ColorRun<'a> {
    /// The index of this run's first colour operation in the segment's operation list.
    start: usize,
    /// The index of this run's last colour operation, inclusive.
    end: usize,
    /// The **whole** segment's operation list, not just this run's span. A masked operation is
    /// handed a coordinate of the frame this run colours, which is the segment's output frame, so it
    /// needs the exact geometry that follows it — inside this run's span and after it — to map that
    /// coordinate back to the stage its own mask was compiled against.
    operations: &'a [Processing],
    /// The segment's output stage: the frame this run is applied to.
    stage: Stage,
}

impl<'a> ColorRun<'a> {
    /// This run's colour operations in evaluation order, each with its index in the segment's
    /// operation list.
    fn colour_operations(self) -> impl Iterator<Item = (usize, &'a ColorOperation)> {
        self.operations[self.start..=self.end]
            .iter()
            .enumerate()
            .filter_map(move |(offset, operation)| match operation {
                Processing::Color(operation) => Some((self.start + offset, operation)),
                _ => None,
            })
    }

    /// Whether any operation of this run is modulated by a mask, which is what decides whether the
    /// pass needs snapshot scratch at all. An unmasked run costs and allocates exactly what it did
    /// before masks existed.
    fn has_mask(self) -> bool {
        self.colour_operations()
            .any(|(_, operation)| operation.mask().is_some())
    }
}

struct ColorRuns<'a> {
    operations: &'a [Processing],
    stage: Stage,
    position: usize,
}

impl<'a> Iterator for ColorRuns<'a> {
    type Item = ColorRun<'a>;

    fn next(&mut self) -> Option<ColorRun<'a>> {
        while self.position < self.operations.len() {
            if !matches!(self.operations[self.position], Processing::Color(_)) {
                self.position += 1;
                continue;
            }
            let start = self.position;
            let mut last = start;
            for (index, operation) in self.operations.iter().enumerate().skip(start) {
                match operation {
                    Processing::Color(_) => last = index,
                    Processing::PointReplace { .. } => break,
                    Processing::ExactGeometry(_)
                    | Processing::Spatial(_)
                    | Processing::Resample(_) => {}
                }
            }
            self.position = last + 1;
            return Some(ColorRun {
                start,
                end: last,
                operations: self.operations,
                stage: self.stage,
            });
        }
        None
    }
}

fn color_runs(segment: &Segment) -> ColorRuns<'_> {
    ColorRuns {
        operations: &segment.operations,
        stage: Stage {
            width: segment.width,
            height: segment.height,
        },
        position: 0,
    }
}

/// One masked operation's mask, placed in the frame the run is applied to.
///
/// A mask is compiled against the stage its layer receives — the content stage, for every layer that
/// may carry one — while a colour run is applied to the frame its segment produces, after the
/// segment's exact geometry has been composed into one pass. The two differ by exactly the exact
/// steps that follow the operation, which is the same suffix a point replacement is mapped through
/// ([performance rule 3](../../docs/engineering/performance-rules.md)). Composing that suffix costs
/// `O(operations)` integer multiplies, allocates nothing, and is exact: `a`, `b`, `c`, `d` are a
/// signed permutation, so `unmap` is the mapping's exact inverse and a mask lands on the same content
/// pixels through a quarter turn, a reflection and an axis-aligned crop.
struct MaskPlacement<'a> {
    mask: &'a MaskField,
    /// From the stage the mask was compiled against to the frame this run colours.
    suffix: ExactGeometry,
    /// [`MaskField::bounds`] mapped into that frame: outside it coverage is exactly zero, so the
    /// blend is the identity there and no unit is evaluated at all.
    bounds: Region,
}

impl<'a> MaskPlacement<'a> {
    fn new(run: &ColorRun<'a>, index: usize, mask: &'a MaskField) -> Self {
        let mut suffix = ExactGeometry::identity(run.stage.width, run.stage.height);
        for operation in run.operations[index + 1..].iter().rev() {
            if let Processing::ExactGeometry(step) = operation {
                suffix = step.then(suffix);
            }
        }
        Self {
            mask,
            bounds: suffix.map_region(mask.bounds()),
            suffix,
        }
    }

    /// The coverage at one frame pixel: the mask's own field at the pixel of its own stage that this
    /// frame pixel came from, for `input` — **the value this operation receives at that pixel**, in
    /// linear float, which is the snapshot the blend is taken against and not the value the units
    /// have produced from it. The rasterizing pass and `render.sample` reach this through the same
    /// call with the same arguments, so a sampled byte equals the rendered byte for a masked
    /// layer by construction, for a value-based component exactly as for a geometric one.
    fn coverage(&self, x: u32, y: u32, input: [f32; 3]) -> f32 {
        let (mask_x, mask_y) = self.suffix.unmap(x, y);
        self.mask.evaluate(mask_x, mask_y, input)
    }

    /// The part of one contiguous row span this mask can reach, as offsets into that span, or `None`
    /// when the span lies entirely outside the bounds rectangle.
    fn span(&self, y: u32, x0: u32, len: usize) -> Option<(usize, usize)> {
        if self.bounds.is_empty() || y < self.bounds.y0 || y >= self.bounds.y1() {
            return None;
        }
        let last = x0.saturating_add(len as u32);
        let from = self.bounds.x0.max(x0);
        let to = self.bounds.x1().min(last);
        if to <= from {
            return None;
        }
        Some(((from - x0) as usize, (to - x0) as usize))
    }
}

/// One 8-bit pixel decoded into linear sRGB.
#[inline]
pub(crate) fn decode_pixel(rgb: [u8; 3]) -> [f32; 3] {
    let table = &*SRGB_TO_LINEAR;
    [
        table[rgb[0] as usize],
        table[rgb[1] as usize],
        table[rgb[2] as usize],
    ]
}

/// The output boundary for one channel already carried in f64: clamp to `[0, 1]`, then take the
/// code whose exact threshold interval holds the value, which equals `floor(255 · encode(v) + 0.5)`.
///
/// A pass that accumulates in f64 — the proxy downscale averages a source rectangle that way —
/// quantizes through this directly, so no f32 rounding is inserted between its arithmetic and the
/// code boundary.
#[inline]
pub(crate) fn quantize_channel(value: f64) -> u8 {
    let thresholds = &*SRGB_CODE_THRESHOLDS;
    let value = value.clamp(0.0, 1.0);
    thresholds.partition_point(|threshold| *threshold <= value) as u8
}

/// The output boundary: clamp to `[0, 1]`, then take the code whose exact threshold interval holds
/// the value, which equals `floor(255 · encode(v) + 0.5)`.
#[inline]
pub(crate) fn quantize_pixel(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|value| quantize_channel(f64::from(value)))
}

/// Apply one run to one contiguous run of already decoded linear pixels of row `y` starting at
/// column `x0`, in the coordinates of the stage its segment produces. Nothing is clamped or
/// quantized between operations or between units, so an inverse pair returns its input exactly and a
/// value outside `[0, 1]` survives to the next unit.
///
/// The run is walked **operation by operation** rather than as one flat stream of units, because a
/// masked operation is blended against its own input and an unmasked one is not. An unmasked
/// operation is applied exactly as before: every unit in order, with the finite check over the whole
/// slice after each unit rather than per pixel inside it, so the hot loop stays branch-free and the
/// run fails as soon as a unit has produced a non-finite value. Flattening the units of consecutive
/// unmasked operations into one stream, which is what this used to do, produces the same arithmetic
/// in the same order.
///
/// `scratch` is the snapshot buffer a masked operation blends against. Its length is free: the units
/// are pointwise, so a masked span is processed in blocks of at most `scratch.len()` pixels and the
/// result does not depend on the block size. The rasterizing pass hands it one row of float scratch
/// reserved from the budget; a point query hands it one pixel on the stack and allocates nothing.
fn apply_units(
    run: &ColorRun<'_>,
    y: u32,
    x0: u32,
    pixels: &mut [[f32; 3]],
    scratch: &mut [[f32; 3]],
) -> Result<(), Error> {
    for (index, operation) in run.colour_operations() {
        match operation.mask() {
            None => apply_operation(operation, y, x0, pixels)?,
            Some(mask) => {
                let placement = MaskPlacement::new(run, index, mask);
                apply_masked_operation(operation, &placement, y, x0, pixels, scratch)?;
            }
        }
    }
    Ok(())
}

/// Every unit of one operation, in order, over the whole slice it is given.
fn apply_operation(
    operation: &ColorOperation,
    y: u32,
    x0: u32,
    pixels: &mut [[f32; 3]],
) -> Result<(), Error> {
    for unit in operation.units() {
        unit.apply_row(y, x0, pixels);
        if !pixels.iter().flatten().all(|channel| channel.is_finite()) {
            return Err(Error::new(ErrorKind::ResourceLimit, NON_FINITE_COLOR));
        }
    }
    Ok(())
}

/// One masked operation over one contiguous row span: `out = (1 − M)·in + M·units(in)` per channel,
/// in linear float, inside the run.
///
/// Three properties are load-bearing and are what the tests assert:
///
/// - **The blend is against the operation's own input**, so an unmasked operation before or after it
///   in the same run is unaffected and nothing is clamped or quantized in between. That is why the
///   input is snapshotted rather than recomputed.
/// - **The endpoints are exact.** `M = 0` leaves `1·in + 0·units(in)`, which is `in`, and `M = 1`
///   leaves `0·in + 1·units(in)`, which is `units(in)` — both bit for bit, which the algebraically
///   equal `in + M·(units(in) − in)` is not. Only the sign of a zero can change, and a signed zero
///   quantizes to the same code.
/// - **Outside the bounds rectangle nothing is evaluated at all**, not merely blended away, so a
///   small mask on a large frame costs the units of its own rectangle and no more.
fn apply_masked_operation(
    operation: &ColorOperation,
    placement: &MaskPlacement<'_>,
    y: u32,
    x0: u32,
    pixels: &mut [[f32; 3]],
    scratch: &mut [[f32; 3]],
) -> Result<(), Error> {
    let Some((from, to)) = placement.span(y, x0, pixels.len()) else {
        return Ok(());
    };
    debug_assert!(!scratch.is_empty(), "a masked run needs snapshot scratch");
    let block = scratch.len().max(1);
    let mut at = from;
    while at < to {
        let end = (at + block).min(to);
        let span = &mut pixels[at..end];
        let (snapshot, _) = scratch.split_at_mut(span.len());
        snapshot.copy_from_slice(span);
        apply_operation(operation, y, x0 + at as u32, span)?;
        for (offset, (output, input)) in span.iter_mut().zip(snapshot.iter()).enumerate() {
            let coverage = placement.coverage(x0 + (at + offset) as u32, y, *input);
            for channel in 0..3 {
                output[channel] = (1.0 - coverage) * input[channel] + coverage * output[channel];
            }
        }
        at = end;
    }
    Ok(())
}

/// One pixel through one colour run: decode, every operation in order, clamp and quantize. The point
/// sampler applies a run with this; the rasterizing pass applies the same three steps to a row of a
/// chunk, calling `apply_row` once per row instead of once per pixel, which changes no arithmetic
/// and gives a position-dependent unit the same coordinates. Both paths share `decode_pixel`,
/// `apply_units` and `quantize_pixel`, so a sample cannot disagree with the byte that was rendered —
/// including the mask coverage, which both reach through the one [`MaskPlacement::coverage`] call
/// inside that shared function.
fn color_pixel(rgb: [u8; 3], run: &ColorRun<'_>, x: u32, y: u32) -> Result<[u8; 3], Error> {
    let mut pixel = [decode_pixel(rgb)];
    let mut scratch = [[0.0f32; 3]; 1];
    apply_units(run, y, x, &mut pixel, &mut scratch)?;
    Ok(quantize_pixel(pixel[0]))
}

/// One streamed colour pass over a frame, in place: bounded row chunks on the shared Rayon pool
/// above the same one-megapixel threshold the other passes use, serial below it. No full-frame
/// float buffer exists at any point; each chunk reserves its own scratch before allocating it.
fn apply_color_run(
    pixels: &mut [u8],
    width: u32,
    rows: std::ops::Range<usize>,
    run: &ColorRun<'_>,
    cancel: &Cancel,
) -> Result<(), Error> {
    if pixels.is_empty() || width == 0 || rows.is_empty() {
        return Ok(());
    }
    let stride = width as usize * 4;
    // Only the band of rows the caller names is coloured; every other row keeps its bytes. The
    // whole frame is the band whenever nothing after this pass reads less than that.
    let pixels = &mut pixels[rows.start * stride..rows.end * stride];
    let height = (rows.end - rows.start) as u32;
    let chunk_rows = color_chunk_rows(width);
    let chunk_bytes = chunk_rows * width as usize * 4;
    // Whether this run needs snapshot scratch at all, decided once for the pass: an unmasked run
    // reserves and allocates exactly what it did before masks existed.
    let masked = run.has_mask();
    // A chunk is a whole number of rows, so the row a pixel belongs to is the band's first row
    // plus the chunk's offset inside it: every unit is handed one row at a time, at the
    // coordinates of the stage this segment produces.
    let process = |chunk_index: usize, chunk: &mut [u8]| -> Result<(), Error> {
        // Before the reservation, so a cancelled pass never takes scratch it will not use.
        cancel.check()?;
        let count = chunk.len() / 4;
        let _reservation =
            ScratchBudget::default().reserve(count * std::mem::size_of::<[f32; 3]>());
        // One row of snapshot scratch for a masked operation's own input, reserved before it
        // allocates and released with the chunk. It is a row and not a chunk because `apply_units`
        // is handed one row at a time; nothing here scales with the frame, and an unmasked run takes
        // none of it.
        let (_snapshot_reservation, mut snapshot) = if masked {
            let pixels = width as usize;
            (
                Some(ScratchBudget::default().reserve(pixels * std::mem::size_of::<[f32; 3]>())),
                vec![[0.0f32; 3]; pixels],
            )
        } else {
            (None, Vec::new())
        };
        let mut unused = [[0.0f32; 3]; 1];
        let scratch: &mut [[f32; 3]] = if masked { &mut snapshot } else { &mut unused };
        let mut linear: Vec<[f32; 3]> = chunk
            .chunks_exact(4)
            .map(|pixel| decode_pixel([pixel[0], pixel[1], pixel[2]]))
            .collect();
        let first_row = (rows.start + chunk_index * chunk_rows) as u32;
        for (offset, row) in linear.chunks_mut(width as usize).enumerate() {
            apply_units(run, first_row + offset as u32, 0, row, scratch)?;
        }
        for (pixel, value) in chunk.chunks_exact_mut(4).zip(&linear) {
            pixel[..3].copy_from_slice(&quantize_pixel(*value));
        }
        Ok(())
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        pixels
            .par_chunks_mut(chunk_bytes)
            .enumerate()
            .try_for_each(|(index, chunk)| process(index, chunk))
    } else {
        pixels
            .chunks_mut(chunk_bytes)
            .enumerate()
            .try_for_each(|(index, chunk)| process(index, chunk))
    }
}

/// One bilinear sample of a frame in linear light, with indices clamped to the frame's edge.
///
/// `fetch` reads one pixel of that frame; the rasterizing path reads a buffer and the point-query
/// path evaluates the previous segment recursively, so both produce identical bytes.
#[inline]
fn bilinear(
    u: f64,
    v: f64,
    width: u32,
    height: u32,
    fetch: impl Fn(u32, u32) -> [u8; 4],
) -> [u8; 4] {
    let table = &*SRGB_TO_LINEAR;
    // The mapped coordinate is a pixel center, so index space starts half a pixel earlier.
    let x = u - 0.5;
    let y = v - 0.5;
    let left = x.floor();
    let top = y.floor();
    let weight_x = x - left;
    let weight_y = y - top;
    let index = |value: f64, limit: u32| -> u32 {
        let last = limit.saturating_sub(1);
        if value <= 0.0 {
            0
        } else if value >= f64::from(last) {
            last
        } else {
            value as u32
        }
    };
    let (left_x, right_x) = (index(left, width), index(left + 1.0, width));
    let (top_y, bottom_y) = (index(top, height), index(top + 1.0, height));
    let corners = [
        (fetch(left_x, top_y), (1.0 - weight_x) * (1.0 - weight_y)),
        (fetch(right_x, top_y), weight_x * (1.0 - weight_y)),
        (fetch(left_x, bottom_y), (1.0 - weight_x) * weight_y),
        (fetch(right_x, bottom_y), weight_x * weight_y),
    ];
    let mut pixel = [0; 4];
    for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
        let linear: f64 = corners
            .iter()
            .map(|(corner, weight)| weight * f64::from(table[corner[channel] as usize]))
            .sum();
        *slot = linear_to_srgb(linear);
    }
    // Alpha has no transfer function; it blends linearly.
    let alpha: f64 = corners
        .iter()
        .map(|(corner, weight)| weight * f64::from(corner[3]))
        .sum();
    pixel[3] = alpha.round().clamp(0.0, 255.0) as u8;
    pixel
}

/// The input pixel one continuous input coordinate falls in, clamped to the frame exactly as the
/// sampler clamps its own indices. A resample maps an output pixel center between input pixels, and
/// this is the corner the bilinear blend weights most, so it is the pixel that output pixel shows.
#[inline]
fn nearest_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    let index = value.floor();
    if index <= 0.0 {
        0
    } else if index >= f64::from(last) {
        last
    } else {
        index as u32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub source_fingerprint: String,
    pub snapshot_id: SnapshotId,
}

impl Raster {
    pub(crate) fn expected_len(width: u32, height: u32) -> Result<usize, Error> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::new(ErrorKind::ResourceLimit, "image dimensions overflow"))?;
        if pixels > 512 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "evaluated image exceeds 512 MiB",
            ));
        }
        usize::try_from(pixels).map_err(|_| {
            Error::new(
                ErrorKind::ResourceLimit,
                "image allocation is not addressable",
            )
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        self.rgba
            .get(offset..offset + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// Composition and mapping of exact geometry belong to the host; modules only declare one step.
impl ExactGeometry {
    pub(crate) fn identity(width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: width,
            output_height: height,
        }
    }

    /// Compose `self` followed by `next`.
    pub(crate) fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.b * self.c,
            b: next.a * self.b + next.b * self.d,
            c: next.c * self.a + next.d * self.c,
            d: next.c * self.b + next.d * self.d,
            tx: next.a * self.tx + next.b * self.ty + next.tx,
            ty: next.c * self.tx + next.d * self.ty + next.ty,
            output_width: next.output_width,
            output_height: next.output_height,
        }
    }

    /// A pure translation that copies one rectangle of its input stage: what a crop with no
    /// straightening declares. Exact, and composable with neighbouring transforms into one pass.
    pub fn crop(x: i64, y: i64, width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: -x,
            ty: -y,
            output_width: width,
            output_height: height,
        }
    }

    /// Where an input-stage point lands, or `None` when it falls outside the output stage: a point
    /// replacement cropped away later is simply not visible.
    fn map(self, x: u32, y: u32) -> Option<(u32, u32)> {
        let out_x = self.a * i64::from(x) + self.b * i64::from(y) + self.tx;
        let out_y = self.c * i64::from(x) + self.d * i64::from(y) + self.ty;
        let inside = out_x >= 0
            && out_x < i64::from(self.output_width)
            && out_y >= 0
            && out_y < i64::from(self.output_height);
        inside.then_some((out_x as u32, out_y as u32))
    }

    /// Whether every output pixel of this mapping reads a pixel that exists in the given input
    /// stage. A module declares its own exact step, so the host checks it before any pass reads a
    /// frame through it: the image of the output rectangle is a rectangle, so its corners decide.
    pub(crate) fn reads_inside(self, input_width: u32, input_height: u32) -> bool {
        if self.output_width == 0 || self.output_height == 0 {
            return false;
        }
        let far_x = i64::from(self.output_width) - 1;
        let far_y = i64::from(self.output_height) - 1;
        [(0, 0), (far_x, 0), (0, far_y), (far_x, far_y)]
            .into_iter()
            .all(|(x, y)| {
                let translated_x = x - self.tx;
                let translated_y = y - self.ty;
                let input_x = self.a * translated_x + self.c * translated_y;
                let input_y = self.b * translated_x + self.d * translated_y;
                (0..i64::from(input_width)).contains(&input_x)
                    && (0..i64::from(input_height)).contains(&input_y)
            })
    }

    /// Where an input-stage pixel rectangle lands in the output stage, clipped to it.
    ///
    /// Closed form and exact: `a`, `b`, `c`, `d` are a signed permutation, so the image of a
    /// rectangle is a rectangle and its two opposite corners decide it. A rectangle that maps
    /// entirely outside the output stage comes back empty, which is what lets a masked run skip a
    /// whole frame whose mask was cropped away.
    fn map_region(self, region: Region) -> Region {
        let empty = Region {
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
        };
        if region.is_empty() || self.output_width == 0 || self.output_height == 0 {
            return empty;
        }
        let corner = |x: u32, y: u32| -> (i64, i64) {
            (
                self.a * i64::from(x) + self.b * i64::from(y) + self.tx,
                self.c * i64::from(x) + self.d * i64::from(y) + self.ty,
            )
        };
        let (first_x, first_y) = corner(region.x0, region.y0);
        let (last_x, last_y) = corner(region.x1() - 1, region.y1() - 1);
        let x0 = first_x.min(last_x).clamp(0, i64::from(self.output_width)) as u32;
        let x1 = (first_x.max(last_x) + 1).clamp(0, i64::from(self.output_width)) as u32;
        let y0 = first_y.min(last_y).clamp(0, i64::from(self.output_height)) as u32;
        let y1 = (first_y.max(last_y) + 1).clamp(0, i64::from(self.output_height)) as u32;
        if x1 <= x0 || y1 <= y0 {
            return empty;
        }
        Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    fn unmap(self, x: u32, y: u32) -> (u32, u32) {
        let translated_x = i64::from(x) - self.tx;
        let translated_y = i64::from(y) - self.ty;
        let input_x = self.a * translated_x + self.c * translated_y;
        let input_y = self.b * translated_x + self.d * translated_y;
        debug_assert!(input_x >= 0 && input_y >= 0);
        (input_x as u32, input_y as u32)
    }

    fn is_identity(self, input_width: u32, input_height: u32) -> bool {
        self.output_width == input_width
            && self.output_height == input_height
            && (self.a, self.b, self.c, self.d, self.tx, self.ty) == (1, 0, 0, 1, 0, 0)
    }
}

/// Mapping the output of one resample back into its input frame is the host's too.
impl Resample {
    /// The continuous input coordinate one output pixel center samples.
    fn input_at(self, x: u32, y: u32) -> (f64, f64) {
        let [m0, m1, m2, m3, m4, m5] = self.inverse;
        let center_x = f64::from(x) + 0.5;
        let center_y = f64::from(y) + 0.5;
        (
            m0 * center_x + m1 * center_y + m2,
            m3 * center_x + m4 * center_y + m5,
        )
    }
}

/// The rows of a resample's input frame that its output can read, with a margin, so the pointwise
/// colour before a crop is applied only where the crop looks. The mapping is affine, so the region
/// its output rectangle reads is the convex hull of the four mapped corners, and the bilinear
/// sample at each of them reads at most one neighbouring pixel in each direction, which the margin
/// covers with a pixel to spare. Everything outside the band is discarded by the resample, so
/// leaving it uncoloured changes no output byte.
fn rows_read_by(resample: Resample, input_height: u32) -> std::ops::Range<usize> {
    if resample.output_width == 0 || resample.output_height == 0 || input_height == 0 {
        return 0..0;
    }
    let (last_x, last_y) = (resample.output_width - 1, resample.output_height - 1);
    let mut top = f64::INFINITY;
    let mut bottom = f64::NEG_INFINITY;
    for (x, y) in [(0, 0), (last_x, 0), (0, last_y), (last_x, last_y)] {
        let (_, v) = resample.input_at(x, y);
        if !v.is_finite() {
            return 0..input_height as usize;
        }
        top = top.min(v);
        bottom = bottom.max(v);
    }
    let start = (top - 0.5).floor() - 2.0;
    let end = (bottom - 0.5).ceil() + 3.0;
    let start = start.max(0.0).min(f64::from(input_height)) as usize;
    let end = end.max(0.0).min(f64::from(input_height)) as usize;
    start..end.max(start)
}

/// One exact pass over one input frame: every output pixel copies exactly one input pixel.
fn copy_transformed(
    input: &[u8],
    input_width: u32,
    geometry: ExactGeometry,
    cancel: &Cancel,
) -> Result<Vec<u8>, Error> {
    let width = geometry.output_width;
    let height = geometry.output_height;
    cancel.check()?;
    let mut output = vec![0; Raster::expected_len(width, height)?];
    let row_bytes = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "image row is not addressable"))?;
    // One relaxed load per output row, ahead of that row's copies; the arithmetic below is
    // untouched.
    let copy_row = |out_y: usize, row: &mut [u8]| -> Result<(), Error> {
        cancel.check()?;
        for out_x in 0..width {
            let (input_x, input_y) = geometry.unmap(out_x, out_y as u32);
            let from =
                ((u64::from(input_y) * u64::from(input_width) + u64::from(input_x)) * 4) as usize;
            let to = out_x as usize * 4;
            row[to..to + 4].copy_from_slice(&input[from..from + 4]);
        }
        Ok(())
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| copy_row(out_y, row))?;
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| copy_row(out_y, row))?;
    }
    Ok(output)
}

/// One interpolating pass: the resample reads the frame it was given and writes the next one.
fn resample_frame(
    input: &[u8],
    input_width: u32,
    input_height: u32,
    resample: Resample,
    cancel: &Cancel,
) -> Result<Vec<u8>, Error> {
    let width = resample.output_width;
    let height = resample.output_height;
    cancel.check()?;
    let mut output = vec![0; Raster::expected_len(width, height)?];
    let row_bytes = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "image row is not addressable"))?;
    let fetch = |x: u32, y: u32| -> [u8; 4] {
        let offset = ((u64::from(y) * u64::from(input_width) + u64::from(x)) * 4) as usize;
        let pixel = &input[offset..offset + 4];
        [pixel[0], pixel[1], pixel[2], pixel[3]]
    };
    // One relaxed load per output row, ahead of that row's samples; the point sampler and the
    // bilinear blend are untouched.
    let sample_row = |out_y: usize, row: &mut [u8]| -> Result<(), Error> {
        cancel.check()?;
        for out_x in 0..width {
            let (u, v) = resample.input_at(out_x, out_y as u32);
            let pixel = bilinear(u, v, input_width, input_height, fetch);
            let to = out_x as usize * 4;
            row[to..to + 4].copy_from_slice(&pixel);
        }
        Ok(())
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| sample_row(out_y, row))?;
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| sample_row(out_y, row))?;
    }
    Ok(output)
}

/// What produces one segment's input frame, and therefore what separates it from the segment
/// before it. Both kinds are stage boundaries: the frame before them is finished, they read it and
/// write the next one.
pub(crate) enum Entry {
    /// An interpolating boundary that also changes the stage.
    Resample(Resample),
    /// A neighbourhood boundary at the same dimensions as the stage it receives. `prefix_hash` is
    /// the SHA-256 of the canonical JSON of the layers before this one, which together with the
    /// source and the stage identifies what a global estimate was prepared from.
    Spatial {
        operation: SpatialOperation,
        prefix_hash: String,
    },
}

impl Entry {
    /// The resample this entry is, if it is one. Point queries and the linear path walk a resample
    /// by its inverse mapping; a spatial entry maps its input pixel to itself.
    pub(crate) fn resample(&self) -> Option<Resample> {
        match self {
            Self::Resample(resample) => Some(*resample),
            Self::Spatial { .. } => None,
        }
    }
}

/// One neighbourhood pass over a finished byte frame: the operation reads that frame through the
/// existing sRGB table, runs its unit chain over stage-aligned tiles and quantizes each tile into a
/// new frame of the same size with the existing exact-threshold quantizer. Alpha is copied from the
/// input; no full-frame float buffer exists at any point, only one tile's working set per tile in
/// flight, charged to the spatial budget before each batch of tiles allocates.
#[allow(clippy::too_many_arguments)]
fn spatial_frame(
    input: &[u8],
    stage: Stage,
    operation: &SpatialOperation,
    prefix_hash: &str,
    fingerprint: &str,
    cancel: &Cancel,
    tile_size: u32,
) -> Result<Vec<u8>, Error> {
    let plan = SpatialPlan::new(operation, stage, tile_size)?;
    let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
        let offset = ((u64::from(y) * u64::from(stage.width) + u64::from(x)) * 4) as usize;
        Ok(decode_pixel([
            input[offset],
            input[offset + 1],
            input[offset + 2],
        ]))
    };
    let globals = resolve_globals(operation, stage, fingerprint, prefix_hash, || {
        build_reduction(stage, read)
    })?;
    let mut output = vec![0; Raster::expected_len(stage.width, stage.height)?];
    run_batches(
        &plan,
        cancel,
        |tile, parallelism| -> Result<Vec<u8>, Error> {
            let (region, values) = run_tile(
                &plan,
                operation,
                &globals,
                tile,
                parallelism,
                |region, planes| fill_planes(region, planes, parallelism, read),
            )?;
            let mut bytes = vec![0; (tile.pixels() * 3) as usize];
            let row = |(row, bytes): (usize, &mut [u8])| {
                let y = tile.y0 + row as u32;
                for (column, x) in (tile.x0..tile.x1()).enumerate() {
                    let rgb = quantize_pixel(spatial::plane_pixel(region, &values, x, y));
                    bytes[column * 3..column * 3 + 3].copy_from_slice(&rgb);
                }
            };
            let row_bytes = tile.width as usize * 3;
            match parallelism {
                Parallelism::Pool => bytes.par_chunks_mut(row_bytes).enumerate().for_each(row),
                Parallelism::Serial => bytes.chunks_mut(row_bytes).enumerate().for_each(row),
            }
            Ok(bytes)
        },
        |tile, bytes| -> Result<(), Error> {
            for (row, y) in (tile.y0..tile.y1()).enumerate() {
                for (column, x) in (tile.x0..tile.x1()).enumerate() {
                    let to = ((u64::from(y) * u64::from(stage.width) + u64::from(x)) * 4) as usize;
                    let from = (row * tile.width as usize + column) * 3;
                    output[to..to + 3].copy_from_slice(&bytes[from..from + 3]);
                    output[to + 3] = input[to + 3];
                }
            }
            Ok(())
        },
    )?;
    Ok(output)
}

/// One rasterizing pass: the exact operations that share an input frame, their composed geometry and
/// the stage they produce. `entry` is what produces this segment's input frame, so consecutive
/// segments are separated by exactly one resample or one spatial operation, and the first segment
/// reads the source.
pub(crate) struct Segment {
    pub(crate) entry: Option<Entry>,
    pub(crate) operations: Vec<Processing>,
    pub(crate) geometry: ExactGeometry,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) has_pixels: bool,
    pub(crate) has_color: bool,
}

impl Segment {
    pub(crate) fn new(entry: Option<Entry>, width: u32, height: u32) -> Self {
        Self {
            entry,
            operations: Vec::new(),
            geometry: ExactGeometry::identity(width, height),
            width,
            height,
            has_pixels: false,
            has_color: false,
        }
    }

    /// Whether this pass writes anything into its frame. An identity pass that does not shares the
    /// source allocation instead of copying it.
    fn writes_pixels(&self) -> bool {
        self.has_pixels || self.has_color
    }
}

/// One recipe compiled by the registry: the ordered rasterizing passes and the resamples between
/// them. A recipe without a resample is one segment, which is the M1 and M2 behavior unchanged.
pub(crate) struct Compiled {
    pub(crate) segments: Vec<Segment>,
}

impl Compiled {
    fn last(&self) -> &Segment {
        self.segments
            .last()
            .expect("a compiled recipe always has one segment")
    }

    pub(crate) fn stage(&self) -> Stage {
        Stage {
            width: self.last().width,
            height: self.last().height,
        }
    }

    /// Whether any mask in this compilation had the thin-feature rule applied to it: it draws a
    /// feature narrower than two pixels of the stage it was compiled against, so it is evaluated
    /// with a 2 x 2 supersample per pixel and the frame it produces is approximate.
    ///
    /// Read from the compilation the render itself uses rather than recomputed from the recipe, so
    /// what is reported and what is drawn cannot disagree. Cost is `O(layers)` and reads no pixels.
    pub(crate) fn supersampled_masks(&self) -> bool {
        self.segments.iter().any(|segment| {
            segment.operations.iter().any(|operation| match operation {
                Processing::Color(colour) => colour
                    .mask()
                    .is_some_and(super::mask_field::MaskField::supersampled),
                Processing::Spatial(spatial) => spatial
                    .mask()
                    .is_some_and(super::mask_field::MaskField::supersampled),
                _ => false,
            })
        })
    }

    /// Whether answering one pixel of this compilation evaluates a spatial segment.
    ///
    /// A spatial point query is the declared exception to [performance rule
    /// 4](../../docs/engineering/performance-rules.md#rules): it evaluates one stage-aligned tile
    /// plus the operation's halo, and nothing caches that tile, so a caller that asks per display
    /// cell pays it per cell. The coverage overlay reads this to refuse rather than to pay it.
    /// `O(segments)` and reads no pixels.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment.entry, Some(Entry::Spatial { .. })))
    }
}

/// Where one segment-output pixel comes from: the input-frame pixel it reads and the replacement
/// that wins there, with that replacement's position in the operation list. The position decides
/// which colour runs still reach the pixel: a replacement overwrites everything the runs before it
/// produced, and only the runs after it process the replaced value.
struct Resolved {
    replacement: Option<(usize, [u8; 3])>,
    input_x: u32,
    input_y: u32,
}

impl Segment {
    fn resolve(&self, x: u32, y: u32) -> Option<Resolved> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (input_x, input_y) = self.geometry.unmap(x, y);
        let mut suffix = ExactGeometry::identity(self.width, self.height);
        for (index, operation) in self.operations.iter().enumerate().rev() {
            match operation {
                Processing::ExactGeometry(step) => suffix = step.then(suffix),
                Processing::PointReplace {
                    x: pixel_x,
                    y: pixel_y,
                    rgb,
                } if suffix.map(*pixel_x, *pixel_y) == Some((x, y)) => {
                    return Some(Resolved {
                        replacement: Some((index, *rgb)),
                        input_x,
                        input_y,
                    });
                }
                // A point replacement mapping outside this stage was cropped away.
                Processing::PointReplace { .. } => {}
                // Colour is applied to the resolved value, not to the coordinate walk, and a
                // resample or spatial operation is the next segment's entry, never one of its
                // operations.
                Processing::Color(_) | Processing::Spatial(_) | Processing::Resample(_) => {}
            }
        }
        Some(Resolved {
            replacement: None,
            input_x,
            input_y,
        })
    }
}

/// Where each of one segment's point replacements lands in its output frame, in stack order, with
/// its position in the operation list. One backward walk accumulates the suffix geometry that
/// carries each replacement; a replacement a later crop discards is simply absent. The result is
/// bounded by the layer count and reads no pixels.
fn mapped_replacements(segment: &Segment) -> Vec<(usize, u32, u32, [u8; 3])> {
    let mut suffix = ExactGeometry::identity(segment.width, segment.height);
    let mut mapped = Vec::new();
    for (index, operation) in segment.operations.iter().enumerate().rev() {
        match operation {
            Processing::ExactGeometry(step) => suffix = step.then(suffix),
            Processing::PointReplace {
                x: pixel_x,
                y: pixel_y,
                rgb,
            } => {
                if let Some((x, y)) = suffix.map(*pixel_x, *pixel_y) {
                    mapped.push((index, x, y, *rgb));
                }
            }
            Processing::Color(_) | Processing::Spatial(_) | Processing::Resample(_) => {}
        }
    }
    mapped.reverse();
    mapped
}

/// Apply one segment's operation list to its rasterized frame as ordered phases: the point
/// replacements before a colour run, then that run streamed over the frame, then the replacements
/// after it. Writing the replacements in stack order lets a later one win at the same coordinate,
/// exactly as a later layer does.
fn apply_operations(
    pixels: &mut [u8],
    segment: &Segment,
    rows: std::ops::Range<usize>,
    cancel: &Cancel,
) -> Result<(), Error> {
    if !segment.writes_pixels() {
        return Ok(());
    }
    let replacements = mapped_replacements(segment);
    let mut next = 0;
    let write = |next: &mut usize, before: usize, pixels: &mut [u8]| {
        while let Some((index, x, y, rgb)) = replacements.get(*next).copied() {
            if index >= before {
                break;
            }
            let offset = ((u64::from(y) * u64::from(segment.width) + u64::from(x)) * 4) as usize;
            pixels[offset..offset + 3].copy_from_slice(&rgb);
            *next += 1;
        }
    };
    for run in color_runs(segment) {
        write(&mut next, run.start, pixels);
        apply_color_run(pixels, segment.width, rows.clone(), &run, cancel)?;
    }
    write(&mut next, usize::MAX, pixels);
    Ok(())
}

fn check_source(source: &SourceImage) -> Result<(), Error> {
    if source.rgba.len() != Raster::expected_len(source.width, source.height)? {
        return Err(Error::new(
            ErrorKind::Validation,
            "source pixel buffer has the wrong length",
        ));
    }
    Ok(())
}

fn source_pixel(source: &SourceImage, x: u32, y: u32) -> [u8; 4] {
    let offset = ((u64::from(y) * u64::from(source.width) + u64::from(x)) * 4) as usize;
    let pixel = &source.rgba[offset..offset + 4];
    [pixel[0], pixel[1], pixel[2], pixel[3]]
}

/// One evaluated pixel of a recipe's output stage, with that stage's dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub width: u32,
    pub height: u32,
    /// `None` when the coordinate lies outside the output stage.
    pub rgba: Option<[u8; 4]>,
}

/// One located pixel of a recipe's content stage: the source after EXIF orientation, which is the
/// stage the first layer receives and the stage a pixel-stage edit addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPoint {
    pub content_x: u32,
    pub content_y: u32,
    /// The content stage's dimensions.
    pub width: u32,
    pub height: u32,
}

/// One stage's dimensions, as a mapping result names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSize {
    pub width: u32,
    pub height: u32,
}

/// The whole geometry tail of one recipe as a single affine map, both ways, between the content
/// stage and the output stage.
///
/// [`ContentPoint`] answers one pixel at a time, which is what a pick needs. A gesture that must
/// follow the pointer cannot pay that call per move ([performance rule
/// 12](../../docs/engineering/performance-rules.md#rules)), and it does not have to: the tail is
/// exact integer transforms plus at most one crop resample, so the map is affine and one matrix
/// answers every position a gesture will ask about.
///
/// **Coordinates are continuous and pixel-center based, the convention [`Resample`] already fixes:**
/// pixel index `n` has its center at `n + 0.5`, so a coordinate `c` lies in pixel `c.floor()` and
/// the content stage spans `0.0..width` by `0.0..height`. The
/// [crop spec](../../docs/specs/single-image.md#sampling) states the same thing about the crop's own
/// sampling, and [`Resample::inverse`] maps output pixel centers to exactly these input
/// coordinates, so nothing here introduces a second convention.
///
/// Both matrices are `[m0, m1, m2, m3, m4, m5]`, again the coefficient order of
/// [`Resample::inverse`]: `x' = m0·x + m1·y + m2` and `y' = m3·x + m4·y + m5`. `forward` maps a
/// content coordinate to an output coordinate and `inverse` is its exact inverse; a coordinate that
/// lands outside the output stage was cropped away, which the caller sees from `output` and this
/// type does not hide by clamping.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageTransform {
    pub content: StageSize,
    pub output: StageSize,
    pub forward: [f64; 6],
    pub inverse: [f64; 6],
}

/// One affine map in the continuous, pixel-center coordinates [`StageTransform`] documents, in the
/// coefficient order [`Resample::inverse`] fixes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine([f64; 6]);

impl Affine {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    /// The continuous form of one exact step. [`ExactGeometry`] maps pixel *indices*; this is the
    /// same mapping over pixel *centers*, so the half-pixel of the convention is conjugated in on
    /// both sides: `out = A·(in − ½) + t + ½`. An exact step is a signed permutation with an integer
    /// translation, so every coefficient stays exact in `f64` and the center of an input pixel lands
    /// exactly on the center of an output pixel.
    fn from_exact(step: ExactGeometry) -> Self {
        let (a, b, c, d) = (step.a as f64, step.b as f64, step.c as f64, step.d as f64);
        Self([
            a,
            b,
            step.tx as f64 + 0.5 - 0.5 * (a + b),
            c,
            d,
            step.ty as f64 + 0.5 - 0.5 * (c + d),
        ])
    }

    /// `self` followed by `next`. Composing the tail this way is what keeps the cost `O(layers)`:
    /// one matrix multiply per layer, and no walk per point afterwards.
    fn then(self, next: Self) -> Self {
        let [a0, a1, a2, a3, a4, a5] = self.0;
        let [b0, b1, b2, b3, b4, b5] = next.0;
        Self([
            b0 * a0 + b1 * a3,
            b0 * a1 + b1 * a4,
            b0 * a2 + b1 * a5 + b2,
            b3 * a0 + b4 * a3,
            b3 * a1 + b4 * a4,
            b3 * a2 + b4 * a5 + b5,
        ])
    }

    /// The exact inverse map, or a refusal. A step that does not invert collapses its stage onto a
    /// line or a point, so there is no mapping to report and reporting the identity instead would
    /// put a gesture's pointer somewhere the recipe never puts it. The compiler already refuses a
    /// resample whose mapping is not finite or whose output stage is empty; this is the remaining
    /// degenerate case, refused in the same voice.
    fn invert(self) -> Result<Self, Error> {
        let [m0, m1, m2, m3, m4, m5] = self.0;
        let determinant = m0 * m4 - m1 * m3;
        let inverted = Self([
            m4 / determinant,
            -m1 / determinant,
            (m1 * m5 - m4 * m2) / determinant,
            -m3 / determinant,
            m0 / determinant,
            (m3 * m2 - m0 * m5) / determinant,
        ]);
        if determinant == 0.0 || !inverted.0.iter().all(|value| value.is_finite()) {
            return Err(Error::new(
                ErrorKind::Validation,
                "a geometry layer declares a mapping that cannot be inverted",
            ));
        }
        Ok(inverted)
    }
}

/// One compiled recipe bound to its source: the stage it produces and point queries that never
/// allocate a frame. Compiling once serves any number of sampled pixels.
pub(crate) struct Evaluation<'a> {
    source: &'a SourceImage,
    compiled: Compiled,
    /// The output tile a spatial entry is evaluated in. It is [`PRODUCTION_TILE`] everywhere but
    /// in the tests that prove the result does not depend on it.
    tile: u32,
}

impl<'a> Evaluation<'a> {
    pub(crate) fn new(
        registry: &ModuleRegistry,
        source: &'a SourceImage,
        recipe: &Recipe,
    ) -> Result<Self, Error> {
        check_source(source)?;
        Ok(Self {
            source,
            compiled: registry.compile(source.width, source.height, recipe)?,
            tile: PRODUCTION_TILE,
        })
    }

    #[cfg(test)]
    /// The same evaluation with another spatial tile size. A spatial unit's value at a pixel
    /// depends on that pixel's neighbourhood only, so this changes nothing but the schedule.
    pub(crate) fn with_tile(mut self, tile: u32) -> Self {
        self.tile = tile;
        self
    }

    /// One evaluation over an ordered prefix of a stack, for planning a layer against the stage it
    /// will be inserted at. The recipe format is the caller's to check; compiling the prefix costs
    /// `O(layers)` and allocates no frame, so sampling that stage still rasterizes nothing.
    pub(crate) fn over_layers(
        registry: &ModuleRegistry,
        source: &'a SourceImage,
        layers: &[Layer],
        masks: &[crate::Mask],
        strokes: &crate::path::StrokeTable,
    ) -> Result<Self, Error> {
        check_source(source)?;
        Ok(Self {
            source,
            compiled: registry.compile_layers(
                source.width,
                source.height,
                layers,
                masks,
                strokes,
            )?,
            tile: PRODUCTION_TILE,
        })
    }

    pub(crate) fn stage(&self) -> Stage {
        self.compiled.stage()
    }

    /// `None` when the coordinate lies outside the output stage.
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.pixel_in(self.compiled.segments.len() - 1, x, y)
    }

    /// One pixel of one segment's output stage. A resample is evaluated recursively as the bilinear
    /// blend of four pixels of the previous segment, so a point query costs `O(layers · 4^resamples)`
    /// and never allocates a frame; a stack holds at most one crop layer.
    ///
    /// A spatial entry is the one exception to "a point query never rasterizes", declared in the
    /// [performance rules](../../docs/engineering/performance-rules.md): see [`Self::spatial_pixel`].
    ///
    /// The colour phases are the rasterizing pass's, applied to this one pixel: the replacement that
    /// wins here ends the runs before it, and every run after it is decoded, evaluated and quantized
    /// in turn, so the sampled byte is the byte the frame holds.
    fn pixel_in(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        let segment = &self.compiled.segments[index];
        let Some(resolved) = segment.resolve(x, y) else {
            return Ok(None);
        };
        let mut rgba = match &segment.entry {
            None => source_pixel(self.source, resolved.input_x, resolved.input_y),
            Some(Entry::Spatial {
                operation,
                prefix_hash,
            }) => self.spatial_pixel(
                index,
                operation,
                prefix_hash,
                resolved.input_x,
                resolved.input_y,
            )?,
            Some(Entry::Resample(resample)) => {
                let resample = *resample;
                let previous = &self.compiled.segments[index - 1];
                let (u, v) = resample.input_at(resolved.input_x, resolved.input_y);
                // `bilinear` fetches four pixels and cannot itself fail, so a failure from the
                // previous segment is carried out of the closure and reported here.
                let failure: Cell<Option<Error>> = Cell::new(None);
                let blended = bilinear(u, v, previous.width, previous.height, |x, y| {
                    match self.pixel_in(index - 1, x, y) {
                        Ok(pixel) => pixel.expect("clamped indices stay inside the previous stage"),
                        Err(error) => {
                            failure.set(Some(error));
                            [0; 4]
                        }
                    }
                });
                if let Some(error) = failure.take() {
                    return Err(error);
                }
                blended
            }
        };
        if let Some((_, rgb)) = resolved.replacement {
            rgba[..3].copy_from_slice(&rgb);
        }
        if segment.has_color {
            let after = resolved.replacement.map_or(0, |(index, _)| index + 1);
            let mut rgb = [rgba[0], rgba[1], rgba[2]];
            for run in color_runs(segment).filter(|run| run.start >= after) {
                rgb = color_pixel(rgb, &run, x, y)?;
            }
            rgba[..3].copy_from_slice(&rgb);
        }
        Ok(Some(rgba))
    }

    /// One pixel of the frame a spatial entry produces.
    ///
    /// The value at a pixel depends on a bounded neighbourhood of it, so there is no way to answer
    /// this in `O(layers)`: the sample evaluates the stage-aligned tile that contains the pixel,
    /// reading that tile plus the operation's summed halo through the compiled prefix, with exactly
    /// the tile function the render uses. The sampled byte is therefore the byte a render of that
    /// tile produces, by construction rather than by agreement. Its cost is
    /// `O((tile + halo)² × layers)`, plus one bounded reduction of the stage when a unit's global
    /// estimate is not already cached, and it allocates one tile working set from the spatial
    /// budget and no frame. This is the declared exception to performance rule 4.
    fn spatial_pixel(
        &self,
        index: usize,
        operation: &SpatialOperation,
        prefix_hash: &str,
        x: u32,
        y: u32,
    ) -> Result<[u8; 4], Error> {
        let previous = &self.compiled.segments[index - 1];
        let stage = Stage {
            width: previous.width,
            height: previous.height,
        };
        let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
            let pixel = self.pixel_in(index - 1, x, y)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::Render,
                    "a spatial read was outside its input stage",
                )
            })?;
            Ok(decode_pixel([pixel[0], pixel[1], pixel[2]]))
        };
        let plan = SpatialPlan::new(operation, stage, self.tile)?;
        let globals = resolve_globals(
            operation,
            stage,
            &self.source.fingerprint,
            prefix_hash,
            || build_reduction(stage, read),
        )?;
        let tile = plan.tile_containing(x, y);
        let _reservation = spatial::reserve_one(&plan);
        // Serial on the calling thread: on the pool a sample would queue behind a render.
        let (region, values) = run_tile(
            &plan,
            operation,
            &globals,
            tile,
            Parallelism::Serial,
            |region, planes| fill_planes(region, planes, Parallelism::Serial, read),
        )?;
        let rgb = quantize_pixel(spatial::plane_pixel(region, &values, x, y));
        // Alpha is never touched by a unit; it is the input frame's, exactly as the render copies
        // it.
        let alpha = self
            .pixel_in(index - 1, x, y)?
            .map_or(255, |pixel| pixel[3]);
        Ok([rgb[0], rgb[1], rgb[2], alpha])
    }

    /// The content-stage pixel one output-stage pixel shows. `None` when the coordinate lies outside
    /// the output stage.
    pub(crate) fn locate(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        self.locate_in(self.compiled.segments.len() - 1, x, y)
    }

    /// Walk one segment backwards, the same walk `pixel_in` makes to fetch a color: the composed
    /// exact geometry unmaps to the segment's input frame by its integer inverse, and a resample
    /// takes the nearest pixel of the previous stage to the input coordinate its output pixel center
    /// samples. Cost is linear in the segment count and no frame is allocated.
    fn locate_in(&self, index: usize, x: u32, y: u32) -> Option<(u32, u32)> {
        let segment = &self.compiled.segments[index];
        if x >= segment.width || y >= segment.height {
            return None;
        }
        let (input_x, input_y) = segment.geometry.unmap(x, y);
        let Some(entry) = &segment.entry else {
            // The first segment reads the source, which is the content stage.
            return Some((input_x, input_y));
        };
        let Some(resample) = entry.resample() else {
            // A spatial entry keeps the stage and every coordinate in it: the pixel a spatial
            // output shows is the pixel of its input at the same place.
            return self.locate_in(index - 1, input_x, input_y);
        };
        let previous = &self.compiled.segments[index - 1];
        let (u, v) = resample.input_at(input_x, input_y);
        self.locate_in(
            index - 1,
            nearest_index(u, previous.width),
            nearest_index(v, previous.height),
        )
    }
}

/// The input of one layer of a recipe, as a point query over the stage that layer receives: one
/// compiled prefix answering any number of pixels in linear light.
///
/// **This is where a value-based mask component's pixel comes from.** The colour-constrained brush's
/// seed and `mask.sample-input` read exactly this — the prefix before the mask's first bound layer,
/// through `StageContext::sample_before` — one pixel at a time. The coverage overlay asks the same
/// question once per display cell, so the prefix is compiled *once* here and the per-cell cost is the
/// point query alone: `O(layers)`, no frame allocated ([performance rule
/// 4](../../docs/engineering/performance-rules.md#rules)).
///
/// The two arms are the two source interpretations and they answer in the same domain the render's
/// masked primitives blend in. The byte path's prefix ends at a quantized boundary, exactly as the
/// brush's stored seed and `mask.sample-input` do, so the overlay and the seed read one value; the
/// linear path never quantizes at all.
pub(crate) enum LayerInput<'a> {
    Byte(Evaluation<'a>),
    Linear(linear::LinearEvaluation<'a>),
}

impl LayerInput<'_> {
    /// The stage the layer receives, which is the stage a mask bound to it is compiled against.
    pub(crate) fn stage(&self) -> Stage {
        match self {
            Self::Byte(evaluation) => evaluation.stage(),
            Self::Linear(evaluation) => {
                let (width, height) = evaluation.stage();
                Stage { width, height }
            }
        }
    }

    /// One pixel of the layer's input stage, in linear light, or `None` outside that stage.
    pub(crate) fn linear(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        match self {
            Self::Byte(evaluation) => Ok(evaluation.pixel(x, y)?.map(|rgba| {
                let linear = decode_pixel([rgba[0], rgba[1], rgba[2]]);
                [
                    f64::from(linear[0]),
                    f64::from(linear[1]),
                    f64::from(linear[2]),
                ]
            })),
            Self::Linear(evaluation) => evaluation.pixel(x, y),
        }
    }
}

impl crate::analysis::MaskInputPixel for LayerInput<'_> {
    fn linear(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        LayerInput::linear(self, x, y)
    }
}

/// Evaluate one output pixel without rasterizing; cost is linear in the layer count and, for a
/// stack with a resample, in the four samples each resample blends.
pub fn sample(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<Sample, Error> {
    let evaluation = Evaluation::new(registry, source, recipe)?;
    let stage = evaluation.stage();
    Ok(Sample {
        width: stage.width,
        height: stage.height,
        rgba: evaluation.pixel(x, y)?,
    })
}

/// The centres of the cells of a `side` × `side` grid over a `width` × `height` stage, row by row
/// from the top-left. Along an axis of `extent` pixels the centre of cell `i` is
/// `floor((2i + 1) · extent / (2 · side))`, which lies inside every non-empty stage.
pub(crate) fn grid_centres(side: u32, width: u32, height: u32) -> Vec<(u32, u32)> {
    let centre = |index: u32, extent: u32| {
        ((2 * u64::from(index) + 1) * u64::from(extent) / (2 * u64::from(side.max(1)))) as u32
    };
    (0..side)
        .flat_map(|row| (0..side).map(move |column| (column, row)))
        .map(|(column, row)| (centre(column, width), centre(row, height)))
        .collect()
}

/// Point samples of a recipe's output stage at the centres of a `side` × `side` grid, row by row
/// from the top-left. One compiled evaluation answers every point, so the cost is
/// `O(side² × layers)` — a point through a spatial layer evaluates its tile, as any sample does —
/// and no frame is allocated; each sample is the byte the render holds there. `checkpoint` is asked
/// before each point, so a caller can stop between them.
pub(crate) fn sample_grid(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    side: u32,
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<Vec<[u8; 4]>, Error> {
    let evaluation = Evaluation::new(registry, source, recipe)?;
    let stage = evaluation.stage();
    grid_centres(side, stage.width, stage.height)
        .into_iter()
        .map(|(x, y)| {
            checkpoint()?;
            evaluation.pixel(x, y)?.ok_or_else(|| {
                Error::new(ErrorKind::Internal, "a grid centre lies outside the stage")
            })
        })
        .collect()
}

/// Map one pixel of a recipe's output stage back to the content-stage pixel it shows: the source
/// after EXIF orientation, the stage the first layer receives. A point outside the output stage is a
/// validation error naming that stage. Cost is linear in the layer count and no frame is allocated,
/// so the canvas pick and the API query share one implementation.
/// Locate through compiled geometry using dimensions only; source pixels are never needed.
pub(crate) fn locate_dimensions(
    registry: &ModuleRegistry,
    width: u32,
    height: u32,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    fn walk(compiled: &Compiled, index: usize, x: u32, y: u32) -> Option<(u32, u32)> {
        let segment = &compiled.segments[index];
        if x >= segment.width || y >= segment.height {
            return None;
        }
        let (input_x, input_y) = segment.geometry.unmap(x, y);
        let Some(entry) = &segment.entry else {
            return Some((input_x, input_y));
        };
        let Some(resample) = entry.resample() else {
            return walk(compiled, index - 1, input_x, input_y);
        };
        let previous = &compiled.segments[index - 1];
        let (u, v) = resample.input_at(input_x, input_y);
        walk(
            compiled,
            index - 1,
            nearest_index(u, previous.width),
            nearest_index(v, previous.height),
        )
    }
    let compiled = registry.compile(width, height, recipe)?;
    let stage = compiled.stage();
    let (content_x, content_y) =
        walk(&compiled, compiled.segments.len() - 1, x, y).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "point ({x}, {y}) is outside the {}x{} rendered image",
                    stage.width, stage.height
                ),
            )
        })?;
    Ok(ContentPoint {
        content_x,
        content_y,
        width,
        height,
    })
}

pub fn locate(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    let evaluation = Evaluation::new(registry, source, recipe)?;
    let stage = evaluation.stage();
    let (content_x, content_y) = evaluation.locate(x, y).ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "point ({x}, {y}) is outside the {}x{} rendered image",
                stage.width, stage.height
            ),
        )
    })?;
    Ok(ContentPoint {
        content_x,
        content_y,
        width: source.width,
        height: source.height,
    })
}

/// The output stage one recipe produces over this source: the dimensions an analysis job, a
/// preview or an export of it will have. Cost is linear in the layer count — it compiles the stack
/// and allocates only the per-segment operation lists — and it reads no pixels and rasterizes
/// nothing, so the catalog owner may call it while building a job.
pub fn extents(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
) -> Result<(u32, u32), Error> {
    check_source(source)?;
    let stage = registry
        .compile(source.width, source.height, recipe)?
        .stage();
    Ok((stage.width, stage.height))
}

/// The whole geometry tail of one recipe as one affine map between the content stage and the output
/// stage, in both directions.
///
/// This is `locate` in closed form. `locate` walks a point back through the compiled segments, which
/// is right for a pick and wrong for a gesture that follows the pointer, so the composition happens
/// once here and the caller maps positions itself. Every step of the tail composes: an exact step is
/// an integer signed permutation with an integer translation, a spatial boundary keeps every
/// coordinate of the stage it receives, and the one crop resample declares its output-to-input
/// mapping, which inverts. Nothing else can appear in the tail.
///
/// The dimensions are the whole input, not a source: no pixel is read on any path here, so there is
/// none to pass. Cost is one matrix multiply per layer on top of compiling the stack, and like
/// [`extents`] it allocates no frame, which is what lets the catalog owner answer it
/// ([rules 4 and 5](../../docs/engineering/performance-rules.md#rules)).
///
/// A stack the compiler refuses has no output stage to map, and its reason is returned unchanged —
/// an unavailable effect is `Incompatible`, a stack the host cannot evaluate is `Validation`. There
/// is no identity fallback on any path.
pub fn stage_transform(
    registry: &ModuleRegistry,
    width: u32,
    height: u32,
    recipe: &Recipe,
) -> Result<StageTransform, Error> {
    let compiled = registry.compile(width, height, recipe)?;
    let mut forward = Affine::IDENTITY;
    for segment in &compiled.segments {
        // A resample declares the map from its output back to its input, which is the direction a
        // sampler reads; the forward direction is that map inverted. A spatial boundary is at the
        // dimensions of the stage it receives and moves no coordinate, so it contributes nothing.
        if let Some(resample) = segment.entry.as_ref().and_then(Entry::resample) {
            forward = forward.then(Affine(resample.inverse).invert()?);
        }
        forward = forward.then(Affine::from_exact(segment.geometry));
    }
    let inverse = forward.invert()?;
    let stage = compiled.stage();
    Ok(StageTransform {
        content: StageSize { width, height },
        output: StageSize {
            width: stage.width,
            height: stage.height,
        },
        forward: forward.0,
        inverse: inverse.0,
    })
}

pub fn render(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
) -> Result<Raster, Error> {
    render_cancellable(registry, source, snapshot_id, recipe, &Cancel::never())
}

/// [`render`] under a [`Cancel`] token the passes read once per row or chunk and once per batch of
/// spatial tiles. With a token that is never cancelled this is byte for byte [`render`]; it is the
/// same code, and [`render`] is one call to it. A cancelled render releases every reservation it
/// held.
pub fn render_cancellable(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    cancel: &Cancel,
) -> Result<Raster, Error> {
    render_tiled(
        registry,
        source,
        snapshot_id,
        recipe,
        cancel,
        PRODUCTION_TILE,
    )
}

/// [`render_cancellable`] against a **proxy** source: the same code and the same colour arithmetic,
/// with the proxy phase's thin-feature rule applied to the masks in the stack.
///
/// The recipe is resolution independent and a mask's geometry is normalized, so this is the exact
/// recipe at proxy size and its frame is byte for byte [`render_cancellable`] over the same
/// downscaled source — *unless* a mask draws a feature narrower than two pixels of that smaller
/// stage, where [`MaskSampling::ThinFeature`] supersamples the mask field 2 x 2 and the caller
/// reports the frame approximate. Nothing about the effect is supersampled, and no exact render
/// ever takes this path.
pub(crate) fn render_proxy_cancellable(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    cancel: &Cancel,
) -> Result<Raster, Error> {
    render_sampled(
        registry,
        source,
        snapshot_id,
        recipe,
        cancel,
        PRODUCTION_TILE,
        MaskSampling::ThinFeature,
    )
}

/// [`render_cancellable`] with the spatial tile size as a parameter. Production always passes
/// [`PRODUCTION_TILE`]; the tests pass other sizes to prove a rendered frame does not depend on it.
pub(crate) fn render_tiled(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    cancel: &Cancel,
    tile: u32,
) -> Result<Raster, Error> {
    render_sampled(
        registry,
        source,
        snapshot_id,
        recipe,
        cancel,
        tile,
        MaskSampling::Point,
    )
}

/// [`render_tiled`] with the mask sampling as a parameter as well. The only caller that passes
/// anything but [`MaskSampling::Point`] is the proxy phase.
#[allow(clippy::too_many_arguments)]
fn render_sampled(
    registry: &ModuleRegistry,
    source: &SourceImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    cancel: &Cancel,
    tile: u32,
    sampling: MaskSampling,
) -> Result<Raster, Error> {
    // A token already cancelled when the call arrives costs no frame at all.
    cancel.check()?;
    check_source(source)?;
    let compiled = registry.compile_sampled(source.width, source.height, recipe, sampling)?;
    let first = &compiled.segments[0];
    let mut width = first.width;
    let mut height = first.height;
    // An identity pass with nothing to write shares the source allocation instead of copying it.
    let mut frame = if first.geometry.is_identity(source.width, source.height) {
        first.writes_pixels().then(|| source.rgba.as_ref().to_vec())
    } else {
        Some(copy_transformed(
            source.rgba.as_ref(),
            source.width,
            first.geometry,
            cancel,
        )?)
    };
    // A segment followed by a resample colours only the rows that resample reads; the last one
    // colours its whole frame.
    let band = |index: usize, height: u32| -> std::ops::Range<usize> {
        match compiled
            .segments
            .get(index + 1)
            .and_then(|next| next.entry.as_ref())
        {
            Some(Entry::Resample(resample)) => rows_read_by(*resample, height),
            // A spatial boundary reads every row of the frame before it, plus a halo.
            Some(Entry::Spatial { .. }) | None => 0..height as usize,
        }
    };
    if let Some(pixels) = frame.as_mut() {
        apply_operations(pixels, first, band(0, height), cancel)?;
    }

    for (index, segment) in compiled.segments.iter().enumerate().skip(1) {
        let entry = segment
            .entry
            .as_ref()
            .expect("every segment after the first enters through a stage boundary");
        let previous = frame.take();
        let input = previous.as_deref().unwrap_or(source.rgba.as_ref());
        let mut next = match entry {
            Entry::Resample(resample) => {
                let frame = resample_frame(input, width, height, *resample, cancel)?;
                width = resample.output_width;
                height = resample.output_height;
                frame
            }
            Entry::Spatial {
                operation,
                prefix_hash,
            } => spatial_frame(
                input,
                Stage { width, height },
                operation,
                prefix_hash,
                &source.fingerprint,
                cancel,
                tile,
            )?,
        };
        // The frame the boundary read is released before the next pass, so two frames is the peak.
        drop(previous);
        if !segment.geometry.is_identity(width, height) {
            let transformed = copy_transformed(&next, width, segment.geometry, cancel)?;
            next = transformed;
            width = segment.width;
            height = segment.height;
        }
        apply_operations(&mut next, segment, band(index, height), cancel)?;
        frame = Some(next);
    }

    Ok(Raster {
        width,
        height,
        rgba: frame.map_or_else(|| source.rgba.clone(), Into::into),
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, EFFECT_FORMAT, Layer, LayerId, MAX_COLOR_UNITS, ORIENTATION_EFFECT, Orientation,
        PIXEL_EFFECT, PixelReplace, PointwiseColor, Recipe, Snapshot, Transform,
        modules::{
            ActionInput, ActionPlan, Availability, BoxRect, CropPayload, CropStage,
            EffectDescriptor, EffectStage, ModuleDescriptor, StageContext, ToolModule,
        },
    };
    use serde_json::{Map, Value, json};

    /// The pixel value a geometric component is handed and ignores (proposal P12 of
    /// `docs/design/range-study.md`): the masks here hold gradients and their coverage is a
    /// function of position alone, so the value is arbitrary and the same at every call.
    const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];
    const ANY_PIXEL_F32: [f32; 3] = [0.25, 0.5, 0.75];

    fn registry() -> ModuleRegistry {
        ModuleRegistry::builtin()
    }

    /// The colour pass before a crop covers only the rows the crop reads, so the rendered frame
    /// must still agree with the point sampler at every output pixel, and the band must be a
    /// strict subset of the stage for a crop that discards rows.
    #[test]
    fn colour_before_a_crop_is_applied_only_where_the_crop_reads_and_stays_exact() {
        // This colours real chunks, so it takes the guard the scratch-budget tests serialize on.
        let _scratch = scratch_guard();
        let registry = registry();
        let source = source(240, 320);
        let stage = CropStage {
            width: 320,
            height: 240,
            angle: 7.0,
        };
        let (box_width, box_height) = stage.bounding_box();
        // A wide, short crop near the centre, so whole rows above and below it are never read.
        let rect = BoxRect {
            x: box_width * 0.1,
            y: box_height * 0.35,
            width: box_width * 0.8,
            height: box_height * 0.3,
        };
        let layers = vec![
            turn(Transform::RotateRight),
            Layer {
                id: LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 0.7, "contrast": 30.0, "vibrance": 40.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer::crop(rect.normalized(&stage)),
        ];
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        };
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        let Some(Entry::Resample(resample)) = compiled.segments[1].entry else {
            panic!("a rotated crop resamples");
        };
        let band = rows_read_by(resample, compiled.segments[0].height);
        assert!(
            band.start > 0 && band.end < compiled.segments[0].height as usize,
            "the band {band:?} should exclude rows of the {} high stage",
            compiled.segments[0].height
        );
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(
                    raster.pixel(x, y),
                    sampled.rgba,
                    "pixel ({x}, {y}) differs from the sampler"
                );
            }
        }
    }
    fn source(width: u32, height: u32) -> SourceImage {
        let mut rgba = Vec::new();
        for i in 0..width * height {
            rgba.extend([i as u8, (i + 20) as u8, (i + 40) as u8, 255]);
        }
        SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:test".into(),
            orientation: 1,
        }
    }
    fn red(raster: &Raster) -> Vec<u8> {
        raster.rgba.chunks_exact(4).map(|p| p[0]).collect()
    }
    /// One single-action orientation layer: what a transform commits when the stack does not end
    /// in an orientation layer. A sequence of these is the stepwise form every proof below uses.
    pub(crate) fn turn(transform: Transform) -> Layer {
        Layer::orientation(Orientation::of(transform))
    }

    /// The elementary steps one orientation payload means: the mirror, then the quarter turns. The
    /// stepwise references apply these one at a time, independently of the composed mapping.
    fn steps(payload: &Value) -> Vec<Transform> {
        let orientation: Orientation = serde_json::from_value(payload.clone()).unwrap();
        let mut steps = Vec::new();
        if orientation.mirror {
            steps.push(Transform::MirrorHorizontal);
        }
        for _ in 0..orientation.turns {
            steps.push(Transform::RotateRight);
        }
        steps
    }

    fn rendered(source: &SourceImage, layers: Vec<Layer>) -> Raster {
        render(
            &registry(),
            source,
            SnapshotId::new(),
            &Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
                masks: Vec::new(),
                ..Recipe::default()
            },
        )
        .unwrap()
    }

    fn reference(source: &SourceImage, layers: &[Layer]) -> (u32, u32, Vec<u8>) {
        let mut width = source.width;
        let mut height = source.height;
        let mut rgba = source.rgba.as_ref().to_vec();
        for layer in layers {
            match layer.effect_id.as_str() {
                PIXEL_EFFECT => {
                    let pixel: PixelReplace =
                        serde_json::from_value(layer.payload.clone()).unwrap();
                    let offset = ((pixel.y * width + pixel.x) * 4) as usize;
                    rgba[offset..offset + 3].copy_from_slice(&pixel.rgb);
                }
                ORIENTATION_EFFECT => {
                    for transform in steps(&layer.payload) {
                        let (next_width, next_height) = match transform {
                            Transform::RotateLeft | Transform::RotateRight => (height, width),
                            Transform::MirrorHorizontal | Transform::FlipVertical => {
                                (width, height)
                            }
                        };
                        let mut next = vec![0; (next_width * next_height * 4) as usize];
                        for y in 0..height {
                            for x in 0..width {
                                let (next_x, next_y) = match transform {
                                    Transform::RotateRight => (height - 1 - y, x),
                                    Transform::RotateLeft => (y, width - 1 - x),
                                    Transform::MirrorHorizontal => (width - 1 - x, y),
                                    Transform::FlipVertical => (x, height - 1 - y),
                                };
                                let from = ((y * width + x) * 4) as usize;
                                let to = ((next_y * next_width + next_x) * 4) as usize;
                                next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                            }
                        }
                        width = next_width;
                        height = next_height;
                        rgba = next;
                    }
                }
                TEST_CROP_EFFECT => {
                    let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                    assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                    let (x, y) = (
                        (crop.x * f64::from(width)).round() as u32,
                        (crop.y * f64::from(height)).round() as u32,
                    );
                    let next_width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                    let next_height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                    let mut next = vec![0; (next_width * next_height * 4) as usize];
                    for row in 0..next_height {
                        for column in 0..next_width {
                            let from = (((row + y) * width + column + x) * 4) as usize;
                            let to = ((row * next_width + column) * 4) as usize;
                            next[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                        }
                    }
                    width = next_width;
                    height = next_height;
                    rgba = next;
                }
                other => panic!("unexpected test effect {other}"),
            }
        }
        (width, height, rgba)
    }

    /// Where one pixel of a stage lands after the exact layers that follow it, evaluated one layer
    /// at a time and independently of the renderer: the direction `locate` walks backwards. `None`
    /// when a crop discards it. Pixel layers move nothing, so they are skipped.
    fn forward(width: u32, height: u32, layers: &[Layer], x: u32, y: u32) -> Option<(u32, u32)> {
        let (mut width, mut height, mut x, mut y) = (width, height, x, y);
        for layer in layers {
            match layer.effect_id.as_str() {
                PIXEL_EFFECT => {}
                ORIENTATION_EFFECT => {
                    for transform in steps(&layer.payload) {
                        (x, y) = match transform {
                            Transform::RotateRight => (height - 1 - y, x),
                            Transform::RotateLeft => (y, width - 1 - x),
                            Transform::MirrorHorizontal => (width - 1 - x, y),
                            Transform::FlipVertical => (x, height - 1 - y),
                        };
                        (width, height) = match transform {
                            Transform::RotateLeft | Transform::RotateRight => (height, width),
                            Transform::MirrorHorizontal | Transform::FlipVertical => {
                                (width, height)
                            }
                        };
                    }
                }
                TEST_CROP_EFFECT => {
                    let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                    assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                    let (origin_x, origin_y) = (
                        (crop.x * f64::from(width)).round() as u32,
                        (crop.y * f64::from(height)).round() as u32,
                    );
                    width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                    height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                    if x < origin_x
                        || y < origin_y
                        || x >= origin_x + width
                        || y >= origin_y + height
                    {
                        return None;
                    }
                    (x, y) = (x - origin_x, y - origin_y);
                }
                other => panic!("unexpected test effect {other}"),
            }
        }
        Some((x, y))
    }

    /// The pixel index a continuous index-space position is nearest to, clamped to the frame.
    fn nearest(position: f64, limit: u32) -> u32 {
        position.round().clamp(0.0, f64::from(limit - 1)) as u32
    }

    /// A test-only module that compiles crop payloads and plain scaling into the host's primitives.
    /// The real crop module is a separate deliverable; this one exists so the host's resample
    /// segmentation can be tested without it.
    struct GeometryTestModule(ModuleDescriptor);

    const TEST_CROP_EFFECT: &str = "test.crop";
    const TEST_SCALE_EFFECT: &str = "test.scale";
    const TEST_OFFSET_EFFECT: &str = "test.offset";
    /// A resample that is finite and has a non-empty output stage, so compilation accepts it, and
    /// whose mapping collapses its stage onto a line. No delivered module can declare one; it exists
    /// so the host's refusal to invent a mapping for it can be proved.
    const TEST_DEGENERATE_EFFECT: &str = "test.degenerate";

    impl GeometryTestModule {
        fn shared() -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: "test.geometry".into(),
                title: "Test geometry".into(),
                hint: None,
                effects: [
                    TEST_CROP_EFFECT,
                    TEST_SCALE_EFFECT,
                    TEST_OFFSET_EFFECT,
                    TEST_DEGENERATE_EFFECT,
                ]
                .into_iter()
                .map(|id| EffectDescriptor {
                    id: id.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                })
                .collect(),
                actions: Vec::new(),
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            }))
        }
    }

    impl ToolModule for GeometryTestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Err(Error::new(ErrorKind::Internal, "no actions"))
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("test geometry {effect_id}"))
        }
        fn compile(
            &self,
            effect_id: &str,
            _: u32,
            payload: &Value,
            stage: Stage,
        ) -> Result<Processing, Error> {
            if effect_id == TEST_OFFSET_EFFECT {
                // A raw translation with a smaller output, including mappings the host must reject.
                return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                    payload["x"].as_i64().expect("test offset"),
                    payload["y"].as_i64().expect("test offset"),
                    payload["width"].as_u64().expect("test offset") as u32,
                    payload["height"].as_u64().expect("test offset") as u32,
                )));
            }
            if effect_id == TEST_DEGENERATE_EFFECT {
                // Both axes read the same line of the input stage: finite, non-empty, and not a
                // mapping anything can be projected back through.
                return Ok(Processing::Resample(Resample {
                    inverse: [1.0, 1.0, 0.0, 1.0, 1.0, 0.0],
                    output_width: stage.width,
                    output_height: stage.height,
                }));
            }
            if effect_id == TEST_SCALE_EFFECT {
                let scale = payload["scale"].as_f64().expect("test scale");
                return Ok(Processing::Resample(Resample {
                    inverse: [1.0 / scale, 0.0, 0.0, 0.0, 1.0 / scale, 0.0],
                    output_width: (f64::from(stage.width) * scale) as u32,
                    output_height: (f64::from(stage.height) * scale) as u32,
                }));
            }
            let crop: CropPayload = serde_json::from_value(payload.clone())
                .map_err(|error| Error::new(ErrorKind::Validation, error.to_string()))?;
            let crop_stage = CropStage {
                width: stage.width,
                height: stage.height,
                angle: crop.angle,
            };
            let rect = crop.output_rect(&crop_stage)?;
            if crop.angle == 0.0 {
                return Ok(Processing::ExactGeometry(ExactGeometry::crop(
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                )));
            }
            Ok(Processing::Resample(Resample {
                inverse: crop_stage.inverse_map((rect.x as f64, rect.y as f64)),
                output_width: rect.width,
                output_height: rect.height,
            }))
        }
    }

    /// The payload a crop draft would commit: the requested fraction of the rotated box, fitted onto
    /// the source and normalized, so every case in these tests is a rectangle the contract accepts.
    pub(crate) fn fitted_crop(width: u32, height: u32, angle: f64, rect: [f64; 4]) -> CropPayload {
        let stage = CropStage {
            width,
            height,
            angle,
        };
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: rect[0] * box_width,
            y: rect[1] * box_height,
            width: rect[2] * box_width,
            height: rect[3] * box_height,
        });
        fitted.normalized(&stage)
    }

    pub(crate) fn crop_layer(crop: CropPayload) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_CROP_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: serde_json::to_value(crop).unwrap(),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn offset_layer(x: i64, y: i64, width: u32, height: u32) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_OFFSET_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"x": x, "y": y, "width": width, "height": height}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn degenerate_layer() -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_DEGENERATE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn scale_layer(scale: f64) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_SCALE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"scale": scale}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    pub(crate) fn geometry_registry() -> ModuleRegistry {
        let mut registry = ModuleRegistry::builtin();
        registry.register(GeometryTestModule::shared()).unwrap();
        registry
    }

    /// An asymmetric gradient with a varying alpha, so a wrong axis, a wrong weight or a dropped
    /// alpha channel all show up.
    pub(crate) fn gradient(width: u32, height: u32) -> SourceImage {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend([
                    (x * 251 / width.max(1)) as u8,
                    (y * 241 / height.max(1)) as u8,
                    ((x * 7 + y * 3) % 256) as u8,
                    (200 + (x + y) % 56) as u8,
                ]);
            }
        }
        SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:gradient".into(),
            orientation: 1,
        }
    }

    /// An independent f64 evaluation of the crop contract: the rotated box, the rounded output
    /// rectangle and one bilinear sample in linear light. It shares no code with the renderer.
    pub(crate) struct CropReference {
        box_width: f64,
        box_height: f64,
        cos: f64,
        sin: f64,
        origin: (f64, f64),
        width: u32,
        height: u32,
    }

    impl CropReference {
        pub(crate) fn new(source: &SourceImage, crop: CropPayload) -> Self {
            let rect = [crop.x, crop.y, crop.width, crop.height];
            let radians = crop.angle * std::f64::consts::PI / 180.0;
            let (cos, sin) = (radians.cos(), radians.sin());
            let width = f64::from(source.width);
            let height = f64::from(source.height);
            let box_width = width * cos.abs() + height * sin.abs();
            let box_height = width * sin.abs() + height * cos.abs();
            Self {
                box_width,
                box_height,
                cos,
                sin,
                origin: (
                    (rect[0] * box_width).round(),
                    (rect[1] * box_height).round(),
                ),
                width: (rect[2] * box_width).round().max(1.0) as u32,
                height: (rect[3] * box_height).round().max(1.0) as u32,
            }
        }

        /// The continuous source position, in index space, that one output pixel center samples.
        fn position(&self, source: &SourceImage, i: u32, j: u32) -> (f64, f64) {
            let x = self.origin.0 + f64::from(i) + 0.5 - self.box_width / 2.0;
            let y = self.origin.1 + f64::from(j) + 0.5 - self.box_height / 2.0;
            (
                self.cos * x + self.sin * y + f64::from(source.width) / 2.0 - 0.5,
                -self.sin * x + self.cos * y + f64::from(source.height) / 2.0 - 0.5,
            )
        }

        pub(crate) fn pixel(&self, source: &SourceImage, i: u32, j: u32) -> [u8; 4] {
            let decode = |value: u8| -> f64 {
                let encoded = f64::from(value) / 255.0;
                if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                }
            };
            let encode = |linear: f64| -> u8 {
                let linear = linear.clamp(0.0, 1.0);
                let encoded = if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                (encoded * 255.0).round() as u8
            };
            let (u, v) = self.position(source, i, j);
            let (left, top) = (u.floor(), v.floor());
            let (fraction_x, fraction_y) = (u - left, v - top);
            let at = |x: f64, y: f64| -> [u8; 4] {
                let x = (x.max(0.0) as u32).min(source.width - 1);
                let y = (y.max(0.0) as u32).min(source.height - 1);
                let offset = ((y * source.width + x) * 4) as usize;
                [
                    source.rgba[offset],
                    source.rgba[offset + 1],
                    source.rgba[offset + 2],
                    source.rgba[offset + 3],
                ]
            };
            let samples = [
                (at(left, top), (1.0 - fraction_x) * (1.0 - fraction_y)),
                (at(left + 1.0, top), fraction_x * (1.0 - fraction_y)),
                (at(left, top + 1.0), (1.0 - fraction_x) * fraction_y),
                (at(left + 1.0, top + 1.0), fraction_x * fraction_y),
            ];
            let mut pixel = [0; 4];
            for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
                *slot = encode(
                    samples
                        .iter()
                        .map(|(sample, weight)| weight * decode(sample[channel]))
                        .sum(),
                );
            }
            pixel[3] = samples
                .iter()
                .map(|(sample, weight)| weight * f64::from(sample[3]))
                .sum::<f64>()
                .round() as u8;
            pixel
        }
    }

    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn measure_resample_on_photo_sized_frames() {
        let registry = geometry_registry();
        for (width, height) in [(6000_u32, 4000_u32), (9504, 6336)] {
            let source = gradient(width, height);
            for (label, layers) in [
                ("exact rotate", vec![turn(Transform::RotateRight)]),
                (
                    "crop 0 deg",
                    vec![crop_layer(fitted_crop(
                        width,
                        height,
                        0.0,
                        [0.1, 0.1, 0.8, 0.8],
                    ))],
                ),
                (
                    "crop 10 deg",
                    vec![crop_layer(fitted_crop(
                        width,
                        height,
                        10.0,
                        [0.1, 0.1, 0.8, 0.8],
                    ))],
                ),
                (
                    "crop 10 deg then rotate",
                    vec![
                        crop_layer(fitted_crop(width, height, 10.0, [0.1, 0.1, 0.8, 0.8])),
                        turn(Transform::RotateRight),
                    ],
                ),
            ] {
                let recipe = Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers,
                    masks: Vec::new(),
                    ..Recipe::default()
                };
                let mut best = f64::INFINITY;
                let mut raster = None;
                for _ in 0..5 {
                    let start = std::time::Instant::now();
                    raster = Some(render(&registry, &source, SnapshotId::new(), &recipe).unwrap());
                    best = best.min(start.elapsed().as_secs_f64() * 1000.0);
                }
                let raster = raster.unwrap();
                println!(
                    "{width}x{height} {label}: {best:.1} ms -> {}x{}",
                    raster.width, raster.height
                );
            }
        }
    }

    #[test]
    fn rotated_crops_match_an_independent_reference_sampler() {
        let registry = geometry_registry();
        // The last input is over a megapixel on both sides of the resample, so the parallel row path
        // runs; every pixel of every case is compared against the reference.
        for (width, height, angle, rect) in [
            (64_u32, 48_u32, 7.5_f64, [0.12, 0.1, 0.7, 0.75]),
            (64, 48, -30.0, [0.25, 0.2, 0.5, 0.55]),
            (64, 48, 45.0, [0.3, 0.3, 0.4, 0.4]),
            (1500, 1100, 10.0, [0.1, 0.1, 0.8, 0.8]),
        ] {
            let source = gradient(width, height);
            let crop = fitted_crop(width, height, angle, rect);
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: vec![crop_layer(crop)],
                masks: Vec::new(),
                ..Recipe::default()
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            let reference = CropReference::new(&source, crop);
            let case = format!("{width}x{height} at {angle}");
            assert_eq!(
                (raster.width, raster.height),
                (reference.width, reference.height),
                "{case}: output dimensions"
            );
            assert!(raster.width > 1 && raster.height > 1, "{case}");
            let mut worst = 0_i32;
            for j in 0..raster.height {
                for i in 0..raster.width {
                    let expected = reference.pixel(&source, i, j);
                    let actual = raster.pixel(i, j).expect("inside the output stage");
                    for channel in 0..4 {
                        let difference = i32::from(actual[channel]) - i32::from(expected[channel]);
                        worst = worst.max(difference.abs());
                        assert!(
                            difference.abs() <= 1,
                            "{case}: pixel ({i}, {j}) channel {channel}: {actual:?} against {expected:?}"
                        );
                    }
                }
            }
            assert!(
                u64::from(raster.width) * u64::from(raster.height) > PARALLEL_RENDER_PIXELS
                    || width == 64,
                "{case}: {}x{} does not reach the parallel row path",
                raster.width,
                raster.height
            );
            println!("{case}: worst channel difference against the f64 reference is {worst}");
        }
    }

    #[test]
    fn angle_zero_crops_are_exact_copies_composed_into_one_pass() {
        let registry = geometry_registry();
        let source = source(7, 5);
        let crops = [
            CropPayload::NEUTRAL,
            CropPayload {
                angle: 0.0,
                x: 2.0 / 7.0,
                y: 1.0 / 5.0,
                width: 4.0 / 7.0,
                height: 3.0 / 5.0,
            },
            CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 4.0 / 5.0,
                width: 1.0,
                height: 1.0 / 5.0,
            },
        ];
        for crop in crops {
            for stack in [
                vec![],
                vec![Transform::RotateRight],
                vec![Transform::MirrorHorizontal],
            ] {
                for after in [
                    vec![],
                    vec![Transform::RotateLeft],
                    vec![Transform::FlipVertical],
                ] {
                    let mut layers: Vec<Layer> = stack.iter().copied().map(turn).collect();
                    layers.push(crop_layer(crop));
                    layers.extend(after.iter().copied().map(turn));
                    let recipe = Recipe {
                        format: crate::RECIPE_FORMAT,
                        layers: layers.clone(),
                        masks: Vec::new(),
                        ..Recipe::default()
                    };
                    let compiled = registry
                        .compile(source.width, source.height, &recipe)
                        .unwrap();
                    assert_eq!(
                        compiled.segments.len(),
                        1,
                        "an exact crop never starts a new rasterizing pass"
                    );
                    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                    let expected = reference(&source, &layers);
                    assert_eq!(
                        (raster.width, raster.height),
                        (expected.0, expected.1),
                        "{layers:?}"
                    );
                    assert_eq!(
                        raster.rgba.as_ref(),
                        expected.2,
                        "{crop:?} {stack:?} {after:?}"
                    );
                }
            }
        }
        // A neutral crop of the whole stage still shares the source allocation.
        let shared = render(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe {
                format: crate::RECIPE_FORMAT,
                layers: vec![crop_layer(CropPayload::NEUTRAL)],
                masks: Vec::new(),
                ..Recipe::default()
            },
        )
        .unwrap();
        assert!(Arc::ptr_eq(&shared.rgba, &source.rgba));
    }

    #[test]
    fn locating_maps_exact_geometry_back_to_the_content_pixel() {
        let registry = geometry_registry();
        let source = source(7, 5);
        let crops = [
            CropPayload::NEUTRAL,
            CropPayload {
                angle: 0.0,
                x: 2.0 / 7.0,
                y: 1.0 / 5.0,
                width: 4.0 / 7.0,
                height: 3.0 / 5.0,
            },
            CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 4.0 / 5.0,
                width: 1.0,
                height: 1.0 / 5.0,
            },
        ];
        for crop in crops {
            for before in [
                vec![],
                vec![Transform::RotateRight],
                vec![Transform::MirrorHorizontal],
                vec![Transform::RotateLeft, Transform::FlipVertical],
            ] {
                for after in [
                    vec![],
                    vec![Transform::RotateLeft],
                    vec![Transform::FlipVertical],
                    vec![Transform::MirrorHorizontal, Transform::RotateRight],
                ] {
                    let mut layers: Vec<Layer> = before.iter().copied().map(turn).collect();
                    layers.push(crop_layer(crop));
                    layers.extend(after.iter().copied().map(turn));
                    let recipe = Recipe {
                        format: crate::RECIPE_FORMAT,
                        layers: layers.clone(),
                        masks: Vec::new(),
                        ..Recipe::default()
                    };
                    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                    // Every output pixel names a content pixel that the stepwise forward map puts
                    // back where it was found, and the rendered bytes are that content pixel's.
                    for y in 0..raster.height {
                        for x in 0..raster.width {
                            let located = locate(&registry, &source, &recipe, x, y).unwrap();
                            let content = (located.content_x, located.content_y);
                            assert_eq!(
                                (located.width, located.height),
                                (source.width, source.height),
                                "{layers:?}"
                            );
                            assert_eq!(
                                forward(source.width, source.height, &layers, content.0, content.1),
                                Some((x, y)),
                                "({x}, {y}) of {layers:?}"
                            );
                            assert_eq!(
                                raster.pixel(x, y),
                                Some(source_pixel(&source, content.0, content.1)),
                                "({x}, {y}) of {layers:?}"
                            );
                        }
                    }
                    // And every content pixel the stack keeps is located from where it lands.
                    for y in 0..source.height {
                        for x in 0..source.width {
                            let Some((out_x, out_y)) =
                                forward(source.width, source.height, &layers, x, y)
                            else {
                                continue;
                            };
                            let located =
                                locate(&registry, &source, &recipe, out_x, out_y).unwrap();
                            assert_eq!(
                                (located.content_x, located.content_y),
                                (x, y),
                                "({x}, {y}) of {layers:?}"
                            );
                        }
                    }
                    // A point outside the output stage is refused, not clamped.
                    for (x, y) in [(raster.width, 0), (0, raster.height)] {
                        let error = locate(&registry, &source, &recipe, x, y)
                            .expect_err(&format!("({x}, {y}) is outside {layers:?}"));
                        assert_eq!(error.kind, ErrorKind::Validation);
                        assert!(
                            error
                                .detail
                                .contains(&format!("{}x{}", raster.width, raster.height)),
                            "{error}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_rotated_crop_locates_the_nearest_pixel_its_sampler_read() {
        let registry = geometry_registry();
        for (width, height, angle, rect, after) in [
            (40_u32, 24_u32, 12.0_f64, [0.2, 0.15, 0.6, 0.65], vec![]),
            (
                28,
                36,
                -30.0,
                [0.25, 0.2, 0.5, 0.55],
                vec![Transform::RotateRight],
            ),
            (
                32,
                24,
                7.5,
                [0.3, 0.25, 0.45, 0.5],
                vec![Transform::MirrorHorizontal, Transform::FlipVertical],
            ),
        ] {
            let source = gradient(width, height);
            let crop = fitted_crop(width, height, angle, rect);
            let tail: Vec<Layer> = after.iter().copied().map(turn).collect();
            // A pixel layer before the crop moves nothing; the walk back is geometry only.
            let mut layers = vec![Layer::pixel(2, 3, [250, 1, 2]), crop_layer(crop)];
            layers.extend(tail.iter().cloned());
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: layers.clone(),
                masks: Vec::new(),
                ..Recipe::default()
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            let reference = CropReference::new(&source, crop);
            let case = format!("{width}x{height} at {angle}");
            assert_eq!(
                u64::from(raster.width) * u64::from(raster.height),
                u64::from(reference.width) * u64::from(reference.height),
                "{case}: output pixel count"
            );
            for j in 0..reference.height {
                for i in 0..reference.width {
                    let (x, y) = forward(reference.width, reference.height, &tail, i, j)
                        .expect("exact transforms keep every pixel");
                    let located = locate(&registry, &source, &recipe, x, y).unwrap();
                    let (u, v) = reference.position(&source, i, j);
                    assert_eq!(
                        (located.content_x, located.content_y),
                        (nearest(u, width), nearest(v, height)),
                        "{case}: ({i}, {j}) of the crop's output"
                    );
                    assert_eq!((located.width, located.height), (width, height), "{case}");
                }
            }
        }
    }

    /// The 64-bit LCG the randomized geometry points draw from. Fixed seeds, so a disagreement is
    /// the same disagreement on every host and run.
    struct Lcg(u64);

    impl Lcg {
        fn next_unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
    }

    /// One affine map applied to a continuous coordinate, written out here rather than shared with
    /// the composition under test.
    fn at(matrix: [f64; 6], x: f64, y: f64) -> (f64, f64) {
        (
            matrix[0] * x + matrix[1] * y + matrix[2],
            matrix[3] * x + matrix[4] * y + matrix[5],
        )
    }

    /// The stage a tail's transforms hand the crop after them.
    fn turned_stage(width: u32, height: u32, transforms: &[Transform]) -> (u32, u32) {
        let (mut width, mut height) = (width, height);
        for transform in transforms {
            if matches!(transform, Transform::RotateLeft | Transform::RotateRight) {
                (width, height) = (height, width);
            }
        }
        (width, height)
    }

    /// Every geometry tail the delivered modules produce: nothing, each of the eight orientations, a
    /// crop at zero degrees, a straightened crop, and crops with transforms on both sides. A pixel,
    /// a colour and a spatial layer ride along in one case each, because they move no coordinate and
    /// the composition must ignore them while still walking the segments they open.
    ///
    /// The flag is whether the tail resamples, which is the only thing that costs the mapping its
    /// exactness: a straightened crop, and nothing else.
    fn geometry_tails(width: u32, height: u32) -> Vec<(String, Vec<Layer>, bool)> {
        let mut tails = vec![("no transform".to_owned(), Vec::new(), false)];
        for turns in 0..4u8 {
            for mirror in [false, true] {
                tails.push((
                    format!("orientation mirror={mirror} turns={turns}"),
                    vec![Layer::orientation(Orientation { mirror, turns })],
                    false,
                ));
            }
        }
        let crops: [(f64, [f64; 4]); 3] = [
            (0.0, [0.15, 0.2, 0.6, 0.55]),
            (11.0, [0.2, 0.15, 0.6, 0.65]),
            (-37.5, [0.25, 0.2, 0.5, 0.5]),
        ];
        let arrangements: [(&[Transform], &[Transform]); 4] = [
            (&[], &[]),
            (&[Transform::RotateRight], &[]),
            (&[], &[Transform::MirrorHorizontal, Transform::RotateLeft]),
            (
                &[Transform::MirrorHorizontal, Transform::RotateLeft],
                &[Transform::FlipVertical],
            ),
        ];
        for (angle, rect) in crops {
            for (before, after) in arrangements {
                let (crop_width, crop_height) = turned_stage(width, height, before);
                let mut layers: Vec<Layer> = before.iter().copied().map(turn).collect();
                layers.push(Layer::crop(fitted_crop(
                    crop_width,
                    crop_height,
                    angle,
                    rect,
                )));
                layers.extend(after.iter().copied().map(turn));
                tails.push((
                    format!("crop at {angle} with {before:?} before and {after:?} after"),
                    layers,
                    angle != 0.0,
                ));
            }
        }
        // A pixel edit, a colour layer and a spatial layer before a straightened crop: three more
        // operations and one more segment, and not one of them moves a coordinate.
        tails.push((
            "pixel, colour and spatial layers before a straightened crop".to_owned(),
            vec![
                Layer::pixel(3, 4, [250, 1, 2]),
                Layer {
                    id: LayerId::new(),
                    effect_id: crate::BASIC_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"exposure": 0.4, "contrast": 20.0}),
                    mask: None,
                    artifacts: Vec::new(),
                },
                Layer {
                    id: LayerId::new(),
                    effect_id: crate::PRESENCE_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"texture": 35.0}),
                    mask: None,
                    artifacts: Vec::new(),
                },
                Layer::crop(fitted_crop(width, height, 6.0, [0.2, 0.2, 0.55, 0.55])),
            ],
            true,
        ));
        tails
    }

    /// `stage_transform` is `locate` in closed form, so the two must name the same content pixel.
    ///
    /// The strong direction is `inverse`: rounding the continuous content coordinate it gives for an
    /// output pixel center to the pixel that contains it must be, exactly, the pixel `locate` walks
    /// to. That is not the same arithmetic — `locate` rounds at the resample and then applies the
    /// exact steps before it as integers, while this composes everything continuously and rounds
    /// once at the end — so the agreement is a real check on the half-pixel convention and on the
    /// composition order, not a re-run of the walk.
    ///
    /// The two orders can disagree in exactly one place: a position that lands *on* a content pixel
    /// boundary, where `nearest_index`'s floor takes the lower index in the resample's own frame and
    /// a reflection after it turns that into the higher index in the content stage. The seeded points
    /// below never land on one, and this asserts that rather than allowing a pixel of slack.
    #[test]
    fn the_stage_transform_and_locate_name_the_same_content_pixel() {
        let registry = registry();
        let (width, height) = (40, 28);
        let source = gradient(width, height);
        for (case, layers, _) in geometry_tails(width, height) {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
                masks: Vec::new(),
                ..Recipe::default()
            };
            let transform = stage_transform(&registry, width, height, &recipe).unwrap();
            let (stage_width, stage_height) = extents(&registry, &source, &recipe).unwrap();
            assert_eq!(
                (transform.content.width, transform.content.height),
                (width, height),
                "{case}: the content stage"
            );
            assert_eq!(
                (transform.output.width, transform.output.height),
                (stage_width, stage_height),
                "{case}: the output stage"
            );
            let mut rng = Lcg(0x5EED_0007);
            for _ in 0..400 {
                let x = (rng.next_unit() * f64::from(stage_width)).floor() as u32;
                let y = (rng.next_unit() * f64::from(stage_height)).floor() as u32;
                let (x, y) = (x.min(stage_width - 1), y.min(stage_height - 1));
                let (u, v) = at(transform.inverse, f64::from(x) + 0.5, f64::from(y) + 0.5);
                assert!(
                    u.fract() != 0.0 && v.fract() != 0.0,
                    "{case}: ({x}, {y}) maps onto a content pixel boundary at ({u}, {v}), \
                     where the two rounding orders are allowed to differ"
                );
                let located = locate(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(
                    (located.content_x, located.content_y),
                    (nearest_index(u, width), nearest_index(v, height)),
                    "{case}: ({x}, {y}) maps to ({u}, {v})"
                );
                assert_eq!(
                    (located.width, located.height),
                    (width, height),
                    "{case}: the content stage"
                );
            }
        }
    }

    /// The acceptance direction: project a content point through `forward` and locate the rendered
    /// pixel it lands in.
    ///
    /// For every exact tail this is exact. `forward` is then a signed permutation of continuous
    /// coordinates, so it carries the pixel containing the content point onto the pixel containing
    /// its image, and `locate` walks straight back to it.
    ///
    /// A straightened crop cannot be exact in the index domain, and the reason is arithmetic rather
    /// than approximate: taking the output *pixel* the projection lands in discards up to half a
    /// pixel in each axis, and `inverse` carries that back into the content stage scaled by its own
    /// coefficients, `½(|m0| + |m1|)` in x and `½(|m3| + |m4|)` in y — about 0.71 content pixels for
    /// the rotation a crop applies. A displacement that large can cross one content pixel boundary
    /// and no more, so the located pixel is within one index, and the continuous displacement is
    /// asserted against that derived budget rather than against a chosen number.
    #[test]
    fn projecting_a_content_point_forward_locates_the_pixel_it_came_from() {
        let registry = registry();
        let (width, height) = (40, 28);
        let source = gradient(width, height);
        for (case, layers, resamples) in geometry_tails(width, height) {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
                masks: Vec::new(),
                ..Recipe::default()
            };
            let transform = stage_transform(&registry, width, height, &recipe).unwrap();
            let budget_x = 0.5 * (transform.inverse[0].abs() + transform.inverse[1].abs());
            let budget_y = 0.5 * (transform.inverse[3].abs() + transform.inverse[4].abs());
            let mut rng = Lcg(0x5EED_0070);
            let mut located_points = 0;
            for _ in 0..1000 {
                let px = rng.next_unit() * f64::from(width);
                let py = rng.next_unit() * f64::from(height);
                let (qx, qy) = at(transform.forward, px, py);
                // A content point the crop discarded has no rendered pixel to locate.
                if qx < 0.0
                    || qy < 0.0
                    || qx >= f64::from(transform.output.width)
                    || qy >= f64::from(transform.output.height)
                {
                    continue;
                }
                located_points += 1;
                let (x, y) = (qx.floor() as u32, qy.floor() as u32);
                let located = locate(&registry, &source, &recipe, x, y).unwrap();
                let (content_x, content_y) = (px.floor() as u32, py.floor() as u32);
                if !resamples {
                    assert_eq!(
                        (located.content_x, located.content_y),
                        (content_x, content_y),
                        "{case}: ({px}, {py}) projects to ({qx}, {qy})"
                    );
                    continue;
                }
                // The centre of the pixel the projection landed in, carried back: within the budget
                // of the content point it started from, and therefore within one content pixel.
                let (u, v) = at(transform.inverse, f64::from(x) + 0.5, f64::from(y) + 0.5);
                assert!(
                    (u - px).abs() <= budget_x + 1e-9 && (v - py).abs() <= budget_y + 1e-9,
                    "{case}: ({px}, {py}) came back as ({u}, {v}), outside \
                     ({budget_x}, {budget_y})"
                );
                assert!(
                    located.content_x.abs_diff(content_x) <= 1
                        && located.content_y.abs_diff(content_y) <= 1,
                    "{case}: ({px}, {py}) located ({}, {}) instead of ({content_x}, {content_y})",
                    located.content_x,
                    located.content_y,
                );
            }
            // The narrowest tail here, a half-frame crop at −37.5°, keeps about a fifth of the
            // content stage; this only proves no case tested nothing.
            assert!(
                located_points > 100,
                "{case}: only {located_points} of the points landed in the output stage"
            );
        }
    }

    /// `forward` and `inverse` are one mapping stated twice. The tolerance is `f64` round-off over a
    /// handful of multiplies at coordinates of a few thousand — the last bits of the mantissa, some
    /// three orders of magnitude under the 1e-9 asserted here — and nothing else: every exact tail
    /// composes integers and half-integers, and the crop's rotation is the only inexact step.
    #[test]
    fn the_stage_transform_and_its_inverse_are_mutual_inverses() {
        let registry = registry();
        let (width, height) = (40, 28);
        for (case, layers, _) in geometry_tails(width, height) {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
                masks: Vec::new(),
                ..Recipe::default()
            };
            let transform = stage_transform(&registry, width, height, &recipe).unwrap();
            let mut rng = Lcg(0x5EED_0700);
            for _ in 0..400 {
                let px = rng.next_unit() * f64::from(width);
                let py = rng.next_unit() * f64::from(height);
                let (qx, qy) = at(transform.forward, px, py);
                let (rx, ry) = at(transform.inverse, qx, qy);
                assert!(
                    (rx - px).abs() < 1e-9 && (ry - py).abs() < 1e-9,
                    "{case}: ({px}, {py}) round-tripped to ({rx}, {ry})"
                );
                let ox = rng.next_unit() * f64::from(transform.output.width);
                let oy = rng.next_unit() * f64::from(transform.output.height);
                let (cx, cy) = at(transform.inverse, ox, oy);
                let (bx, by) = at(transform.forward, cx, cy);
                assert!(
                    (bx - ox).abs() < 1e-9 && (by - oy).abs() < 1e-9,
                    "{case}: output ({ox}, {oy}) round-tripped to ({bx}, {by})"
                );
            }
        }
    }

    /// A stack with no output stage has no mapping, and says so in the voice the neighbouring
    /// methods use. An identity matrix would put a gesture's pointer where the recipe never puts it,
    /// so no path here returns one.
    #[test]
    fn a_stack_without_a_renderable_output_stage_has_no_transform() {
        let registry = geometry_registry();
        let (width, height) = (32, 24);

        // An effect no module provides: `extents` and `locate` report it this way too.
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: LayerId::new(),
                effect_id: "test.absent".into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = stage_transform(&registry, width, height, &recipe)
            .expect_err("an unavailable effect has no output stage");
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(error.detail.contains("unavailable effect"), "{error}");

        // A finite, non-empty resample that still collapses its stage onto a line. The compiler's
        // own checks pass it, so this is the one degenerate mapping that reaches the composition.
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![degenerate_layer()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = stage_transform(&registry, width, height, &recipe)
            .expect_err("a collapsed stage has no mapping");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("cannot be inverted"), "{error}");
    }

    #[test]
    fn a_translation_with_a_smaller_output_composes_and_is_bounds_checked() {
        let registry = geometry_registry();
        let source = source(7, 5);
        // Two translations and a quarter turn compose into one mapping over the source.
        let layers = vec![
            offset_layer(1, 1, 5, 4),
            turn(Transform::RotateRight),
            offset_layer(1, 2, 2, 3),
        ];
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: layers.clone(),
            masks: Vec::new(),
            ..Recipe::default()
        };
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        assert_eq!(compiled.segments.len(), 1);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert_eq!((raster.width, raster.height), (2, 3));
        // The same stack applied one step at a time, through the composed mapping's own definition.
        let mut expected = Vec::new();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let (turned_x, turned_y) = (x + 1, y + 2);
                let (offset_x, offset_y) = (turned_y, 4 - 1 - turned_x);
                expected.extend(source_pixel(&source, offset_x + 1, offset_y + 1));
            }
        }
        assert_eq!(raster.rgba.as_ref(), expected);
        for (x, y) in [(0, 0), (raster.width - 1, raster.height - 1)] {
            assert_eq!(
                sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                raster.pixel(x, y)
            );
        }
        // A mapping that would read outside its input frame is refused, not rasterized.
        for layers in [
            vec![offset_layer(3, 0, 5, 5)],
            vec![offset_layer(-1, 0, 4, 4)],
            vec![offset_layer(0, 0, 8, 5)],
            vec![offset_layer(1, 1, 5, 4), offset_layer(1, 0, 5, 4)],
        ] {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: layers.clone(),
                masks: Vec::new(),
                ..Recipe::default()
            };
            let error = render(&registry, &source, SnapshotId::new(), &recipe)
                .expect_err(&format!("{layers:?} reads outside the stage"));
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains("reads outside"), "{error}");
            assert_eq!(
                sample(&registry, &source, &recipe, 0, 0).unwrap_err().kind,
                ErrorKind::Validation
            );
        }
    }

    #[test]
    fn samples_match_rendered_pixels_through_a_resample() {
        let registry = geometry_registry();
        let source = gradient(40, 24);
        for layers in [
            vec![crop_layer(fitted_crop(
                40,
                24,
                12.0,
                [0.2, 0.15, 0.6, 0.65],
            ))],
            vec![
                Layer::pixel(3, 4, [250, 1, 2]),
                crop_layer(fitted_crop(40, 24, -20.0, [0.25, 0.25, 0.5, 0.5])),
                Layer::pixel(1, 1, [3, 251, 4]),
                turn(Transform::RotateRight),
                Layer::pixel(0, 2, [5, 6, 252]),
            ],
            // Two resamples: a point query blends four recursively evaluated blends.
            vec![
                turn(Transform::MirrorHorizontal),
                crop_layer(fitted_crop(40, 24, 45.0, [0.3, 0.3, 0.4, 0.4])),
                scale_layer(1.5),
                Layer::pixel(0, 0, [7, 8, 253]),
            ],
            vec![scale_layer(2.0), Layer::pixel(5, 5, [254, 9, 10])],
        ] {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: layers.clone(),
                masks: Vec::new(),
                ..Recipe::default()
            };
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            for y in 0..raster.height {
                for x in 0..raster.width {
                    let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                    assert_eq!(
                        (sampled.width, sampled.height),
                        (raster.width, raster.height),
                        "{layers:?}"
                    );
                    assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y}) of {layers:?}");
                }
            }
            assert_eq!(
                sample(&registry, &source, &recipe, raster.width, 0)
                    .unwrap()
                    .rgba,
                None
            );
        }
    }

    #[test]
    fn a_point_replacement_cropped_away_simply_disappears() {
        let registry = geometry_registry();
        let source = source(8, 6);
        let inside = Layer::pixel(4, 3, [250, 1, 2]);
        let outside = Layer::pixel(0, 0, [3, 251, 4]);
        for crop in [
            crop_layer(CropPayload {
                angle: 0.0,
                x: 0.25,
                y: 1.0 / 3.0,
                width: 0.5,
                height: 0.5,
            }),
            crop_layer(fitted_crop(8, 6, 15.0, [0.25, 0.25, 0.5, 0.5])),
        ] {
            let with_outside = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: vec![inside.clone(), outside.clone(), crop.clone()],
                masks: Vec::new(),
                ..Recipe::default()
            };
            let without = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: vec![inside.clone(), crop.clone()],
                masks: Vec::new(),
                ..Recipe::default()
            };
            let rendered = render(&registry, &source, SnapshotId::new(), &with_outside).unwrap();
            let expected = render(&registry, &source, SnapshotId::new(), &without).unwrap();
            assert_eq!(rendered.rgba, expected.rgba, "{crop:?}");
            for y in 0..rendered.height {
                for x in 0..rendered.width {
                    assert_eq!(
                        sample(&registry, &source, &with_outside, x, y)
                            .unwrap()
                            .rgba,
                        rendered.pixel(x, y),
                        "({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn the_frame_limit_applies_to_a_resample_and_point_queries_still_answer() {
        let registry = geometry_registry();
        let source = source(3, 2);
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![scale_layer(10_000.0)],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = render(&registry, &source, SnapshotId::new(), &recipe)
            .expect_err("30000x20000 is over the frame limit");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(error.detail.contains("512 MiB"), "{error}");
        // The same stack answers a point query, because no frame is allocated for one pixel.
        let sampled = sample(&registry, &source, &recipe, 15_000, 10_000).unwrap();
        assert_eq!((sampled.width, sampled.height), (30_000, 20_000));
        assert!(sampled.rgba.is_some());
        // A resample must declare a stage the host can address at all.
        let empty = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![scale_layer(0.0)],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = sample(&registry, &source, &empty, 0, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
    }

    #[test]
    fn pixel_layers_are_ordered_exact_and_source_is_immutable() {
        let source = source(3, 2);
        let original = source.clone();
        let a = Layer::pixel(1, 0, [200, 201, 202]);
        let first = rendered(&source, vec![a.clone()]);
        let second = rendered(&source, vec![a, Layer::pixel(1, 0, [9, 8, 7])]);
        assert_eq!(first.pixel(1, 0), Some([200, 201, 202, 255]));
        assert_eq!(second.pixel(1, 0), Some([9, 8, 7, 255]));
        assert_eq!(first.pixel(0, 0), Some([0, 20, 40, 255]));
        assert_eq!(source, original);
        assert!(rendered(&source, vec![]).pixel(1, 0) != second.pixel(1, 0));
    }

    #[test]
    fn exact_transform_coordinate_tables_for_asymmetric_input() {
        let source = source(3, 2);
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::RotateRight)])),
            vec![3, 0, 4, 1, 5, 2]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::RotateLeft)])),
            vec![2, 5, 1, 4, 0, 3]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::MirrorHorizontal)])),
            vec![2, 1, 0, 5, 4, 3]
        );
        assert_eq!(
            red(&rendered(&source, vec![turn(Transform::FlipVertical)])),
            vec![3, 4, 5, 0, 1, 2]
        );
    }

    #[test]
    fn transform_identities_and_operation_order_hold() {
        let source = source(5, 3);
        for (transform, count) in [
            (Transform::RotateRight, 4),
            (Transform::RotateLeft, 4),
            (Transform::MirrorHorizontal, 2),
            (Transform::FlipVertical, 2),
        ] {
            let layers = (0..count).map(|_| turn(transform)).collect();
            assert_eq!(rendered(&source, layers).rgba, source.rgba);
            // The same actions composed into one layer reach the neutral orientation, whose
            // identity mapping shares the source buffer instead of copying it.
            let mut composed = Orientation::NEUTRAL;
            for _ in 0..count {
                composed = composed.then(transform);
            }
            assert_eq!(composed, Orientation::NEUTRAL, "{transform:?}");
            let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
            assert_eq!(collapsed.rgba, source.rgba);
            assert!(Arc::ptr_eq(&collapsed.rgba, &source.rgba));
        }
        let before = rendered(
            &source,
            vec![
                Layer::pixel(0, 0, [250, 0, 0]),
                turn(Transform::RotateRight),
            ],
        );
        assert_eq!(before.pixel(2, 0), Some([250, 0, 0, 255]));
        let after = rendered(
            &source,
            vec![
                turn(Transform::RotateRight),
                Layer::pixel(0, 0, [250, 0, 0]),
            ],
        );
        assert_eq!(after.pixel(0, 0), Some([250, 0, 0, 255]));
        assert_ne!(before.rgba, after.rgba);
    }

    #[test]
    fn compiled_recipes_match_stepwise_evaluation_for_interleaved_operations() {
        let source = source(5, 3);
        let transforms = [
            Transform::RotateLeft,
            Transform::RotateRight,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ];
        for first in transforms {
            for second in transforms {
                for third in transforms {
                    let layers = vec![
                        Layer::pixel(1, 1, [201, 1, 2]),
                        turn(first),
                        Layer::pixel(0, 0, [3, 202, 4]),
                        turn(second),
                        Layer::pixel(1, 1, [5, 6, 203]),
                        turn(third),
                        Layer::pixel(0, 0, [204, 8, 9]),
                    ];
                    let expected = reference(&source, &layers);
                    let actual = rendered(&source, layers);
                    assert_eq!((actual.width, actual.height), (expected.0, expected.1));
                    assert_eq!(actual.rgba.as_ref(), expected.2);
                }
            }
        }
    }

    /// One orientation layer holding the composed state renders exactly what the same actions
    /// render as separate single-action layers, and both match the stepwise reference. Every one
    /// of the eight orientations against every action, and every sequence of three actions, on a
    /// non-square source so a wrongly composed quarter turn changes the dimensions.
    #[test]
    fn a_composed_orientation_renders_what_its_separate_action_layers_render() {
        let source = source(5, 3);
        let transforms = [
            Transform::RotateLeft,
            Transform::RotateRight,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ];
        let orientations = [false, true]
            .into_iter()
            .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }));
        for state in orientations {
            for transform in transforms {
                let separate = vec![Layer::orientation(state), turn(transform)];
                let expected = reference(&source, &separate);
                let collapsed = rendered(&source, vec![Layer::orientation(state.then(transform))]);
                let stepwise = rendered(&source, separate);
                for raster in [&collapsed, &stepwise] {
                    assert_eq!(
                        (raster.width, raster.height),
                        (expected.0, expected.1),
                        "{state:?} then {transform:?}"
                    );
                    assert_eq!(
                        raster.rgba.as_ref(),
                        expected.2,
                        "{state:?} then {transform:?}"
                    );
                }
            }
        }
        for first in transforms {
            for second in transforms {
                for third in transforms {
                    let separate = vec![turn(first), turn(second), turn(third)];
                    let composed = Orientation::of(first).then(second).then(third);
                    let expected = reference(&source, &separate);
                    let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
                    assert_eq!(
                        (collapsed.width, collapsed.height),
                        (expected.0, expected.1),
                        "{first:?} {second:?} {third:?}"
                    );
                    assert_eq!(
                        collapsed.rgba.as_ref(),
                        expected.2,
                        "{first:?} {second:?} {third:?}"
                    );
                    assert_eq!(rendered(&source, separate).rgba, collapsed.rgba);
                }
            }
        }
    }

    #[test]
    fn samples_match_rendered_pixels_and_keep_source_alpha() {
        let registry = registry();
        let mut source = source(5, 3);
        let rgba: Vec<u8> = source
            .rgba
            .iter()
            .enumerate()
            .map(|(i, v)| if i % 4 == 3 { (i / 4) as u8 + 100 } else { *v })
            .collect();
        source.rgba = rgba.into();
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![
                Layer::pixel(1, 1, [201, 1, 2]),
                turn(Transform::RotateLeft),
                Layer::pixel(0, 0, [3, 202, 4]),
                turn(Transform::MirrorHorizontal),
                Layer::pixel(0, 0, [204, 8, 9]),
                Layer::pixel(0, 0, [205, 10, 11]),
            ],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(
                    (sampled.width, sampled.height),
                    (raster.width, raster.height)
                );
                assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y})");
            }
        }
        assert_eq!(
            sample(&registry, &source, &recipe, raster.width, 0)
                .unwrap()
                .rgba,
            None
        );
        assert_eq!(
            sample(&registry, &source, &recipe, 0, raster.height)
                .unwrap()
                .rgba,
            None
        );
        let invalid = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::pixel(9, 9, [0, 0, 0])],
            masks: Vec::new(),
            ..Recipe::default()
        };
        assert!(sample(&registry, &source, &invalid, 0, 0).is_err());
    }

    #[test]
    fn invalid_coordinates_and_buffers_fail_without_panicking() {
        let registry = registry();
        let source = source(3, 2);
        let snapshot = Snapshot::original(AssetId::new());
        assert!(
            render(
                &registry,
                &source,
                snapshot.id.clone(),
                &Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers: vec![Layer::pixel(3, 0, [0, 0, 0])],
                    masks: Vec::new(),
                    ..Recipe::default()
                }
            )
            .is_err()
        );
        let malformed = SourceImage {
            rgba: vec![0].into(),
            ..source
        };
        assert!(render(&registry, &malformed, snapshot.id, &Recipe::default()).is_err());
    }

    // ---------------------------------------------------------------------------------------
    // The pointwise colour stage.
    // ---------------------------------------------------------------------------------------

    /// The scratch budget is process-wide, so the test that lowers its target and every test that
    /// reserves from it hold this lock instead of racing.
    static SCRATCH_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch_guard() -> std::sync::MutexGuard<'static, ()> {
        SCRATCH_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A test-only colour unit: multiply linear light by `2^EV`. The coefficient is computed in f64
    /// and applied in f32, which is the working precision the contract declares.
    #[derive(Debug)]
    struct Exposure {
        ev: f64,
        gain: f32,
    }

    impl Exposure {
        fn new(ev: f64) -> Self {
            Self {
                ev,
                gain: ev.exp2() as f32,
            }
        }
    }

    impl PointwiseColor for Exposure {
        fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
            for pixel in rgb {
                for channel in pixel {
                    *channel *= self.gain;
                }
            }
        }
        fn is_finite(&self) -> bool {
            self.ev.is_finite() && self.gain.is_finite()
        }
        fn describe(&self) -> String {
            format!("exposure {:+} EV", self.ev)
        }
    }

    /// A unit whose coefficients are finite but whose result is not: two of them in one operation
    /// overflow f32 for any non-zero channel, which is the render-time failure the contract names.
    #[derive(Debug)]
    struct Overflow;

    impl PointwiseColor for Overflow {
        fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
            for pixel in rgb {
                for channel in pixel {
                    *channel *= f32::MAX;
                }
            }
        }
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "overflow".into()
        }
    }

    /// A test-only colour unit whose result depends on where its pixel is: it adds `x / width` to
    /// red and `y / height` to green of the stage its layer receives. A unit like this agrees
    /// between a rendered raster and a point sample only when both paths hand it the same
    /// coordinates, which is exactly what the positional contract promises a finish-stage effect.
    #[derive(Debug)]
    pub(crate) struct Positional {
        width: f32,
        height: f32,
    }

    impl PointwiseColor for Positional {
        fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
            for (offset, pixel) in rgb.iter_mut().enumerate() {
                pixel[0] += (x0 + offset as u32) as f32 / self.width;
                pixel[1] += y as f32 / self.height;
            }
        }
        fn is_finite(&self) -> bool {
            self.width.is_finite() && self.height.is_finite()
        }
        fn describe(&self) -> String {
            format!("positional {}x{}", self.width, self.height)
        }
    }

    pub(crate) const TEST_COLOR_EFFECT: &str = "test.colour";

    /// A test-only module with one colour effect. The Basic module is a separate deliverable; this
    /// one exists so the host's colour stage can be tested without it. Its effect declares itself
    /// maskable, so a masked layer of it compiles exactly as a delivered maskable effect's does.
    struct ColorTestModule {
        descriptor: ModuleDescriptor,
        /// Handed to the `counting` unit when a payload asks for one, so a test can count the pixels
        /// a masked operation actually evaluated.
        counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
    }

    impl ColorTestModule {
        fn with_counter(
            counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
        ) -> Arc<dyn ToolModule> {
            Arc::new(Self {
                descriptor: ModuleDescriptor {
                    id: "test.colour".into(),
                    title: "Test colour".into(),
                    hint: None,
                    effects: vec![EffectDescriptor {
                        id: TEST_COLOR_EFFECT.into(),
                        format: EFFECT_FORMAT,
                        stage: EffectStage::Color,
                        order: 0,
                        maskable: true,
                        artifacts: false,
                    }],
                    actions: Vec::new(),
                    queries: Vec::new(),
                    controls: Vec::new(),
                    reset: None,
                    canvas: None,
                    developer: false,
                    collapsed: false,
                    layout: crate::ModuleLayout::Stacked,
                    availability: Availability::Available,
                    ..ModuleDescriptor::default()
                },
                counter,
            })
        }

        fn shared() -> Arc<dyn ToolModule> {
            Self::with_counter(None)
        }
    }

    impl ToolModule for ColorTestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.descriptor
        }
        fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Err(Error::new(ErrorKind::Internal, "no actions"))
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
            Ok(format!("test colour {payload}"))
        }
        fn compile(
            &self,
            _: &str,
            _: u32,
            payload: &Value,
            stage: Stage,
        ) -> Result<Processing, Error> {
            let mut units: Vec<Arc<dyn PointwiseColor>> = Vec::new();
            for ev in payload["exposure"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
            {
                units.push(Arc::new(Exposure::new(ev.as_f64().expect("an EV number"))));
            }
            if payload["infinite"] == json!(true) {
                units.push(Arc::new(Exposure::new(f64::INFINITY)));
            }
            for _ in 0..payload["overflow"].as_u64().unwrap_or(0) {
                units.push(Arc::new(Overflow));
            }
            if payload["positional"] == json!(true) {
                units.push(Arc::new(Positional {
                    width: stage.width as f32,
                    height: stage.height as f32,
                }));
            }
            if payload["counting"] == json!(true)
                && let Some(counter) = &self.counter
            {
                units.push(Arc::new(Counting(counter.clone())));
            }
            Ok(Processing::Color(ColorOperation::new(units)))
        }
    }

    fn colour_layer(payload: Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_COLOR_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    /// One colour layer of exposure units in order.
    fn exposure_layer(evs: &[f64]) -> Layer {
        colour_layer(json!({ "exposure": evs }))
    }

    /// One colour layer whose unit depends on its pixel position.
    pub(crate) fn positional_layer() -> Layer {
        colour_layer(json!({ "positional": true }))
    }

    pub(crate) fn colour_registry() -> ModuleRegistry {
        let mut registry = geometry_registry();
        registry.register(ColorTestModule::shared()).unwrap();
        registry
    }

    /// A colour registry whose test module counts the pixels its `counting` unit was handed.
    fn counting_registry(counter: Arc<std::sync::atomic::AtomicUsize>) -> ModuleRegistry {
        let mut registry = geometry_registry();
        registry
            .register(ColorTestModule::with_counter(Some(counter)))
            .unwrap();
        registry
    }

    /// One rendered frame used as the source of another render: the reference for "the mask travelled
    /// with the picture" turns an already masked frame with the delivered exact pass.
    fn rendered_from(raster: &Raster, layers: Vec<Layer>) -> Raster {
        let source = SourceImage {
            width: raster.width,
            height: raster.height,
            rgba: raster.rgba.clone(),
            fingerprint: "sha256:rendered-again".into(),
            orientation: 1,
        };
        render(
            &colour_registry(),
            &source,
            SnapshotId::new(),
            &colour_recipe(layers),
        )
        .unwrap()
    }

    fn colour_recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    /// The sRGB transfer function forwards in f64, written from the contract and used only by the
    /// references below; the renderer's own encoder rounds to a byte and is not consulted here.
    fn encode_reference(linear: f64) -> f64 {
        if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    }

    fn decode_reference(encoded: f64) -> f64 {
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }

    /// An independent stepwise f64 evaluation of the colour contract: decode the byte, multiply by
    /// `2^EV` once per unit in order, clamp, encode and round with `floor(255·e + 0.5)`. It shares
    /// no code with the renderer and returns the clamped linear value as well as the code, so a
    /// disagreement can be tested against the threshold it sits on.
    fn colour_reference(byte: u8, evs: &[f64]) -> (u8, f64) {
        let mut linear = decode_reference(f64::from(byte) / 255.0);
        for ev in evs {
            linear *= 2.0_f64.powf(*ev);
        }
        let clamped = linear.clamp(0.0, 1.0);
        let code = (255.0 * encode_reference(clamped) + 0.5).floor();
        (code as u8, clamped)
    }

    /// The tolerance the design freezes for float colour: an exact code everywhere except within
    /// `1e-6 + 1e-6·|value|` of the threshold between two codes, where one code of difference is
    /// permitted because the production path decodes and multiplies in f32.
    fn assert_code_within_tolerance(actual: u8, expected: u8, linear: f64, case: &str) {
        if actual == expected {
            return;
        }
        let difference = i32::from(actual) - i32::from(expected);
        assert!(difference.abs() <= 1, "{case}: {actual} against {expected}");
        let crossed = u32::from(actual.max(expected));
        let threshold = decode_reference((f64::from(crossed) - 0.5) / 255.0);
        let tolerance = 1e-6 + 1e-6 * threshold.abs();
        assert!(
            (linear - threshold).abs() <= tolerance,
            "{case}: {actual} against {expected} is not within {tolerance} of the threshold {threshold} ({linear})"
        );
    }

    /// Every grey, once per byte, so a wrong table entry cannot hide behind a neighbour.
    fn greys() -> SourceImage {
        let mut rgba = Vec::with_capacity(256 * 4);
        for value in 0..=255_u8 {
            rgba.extend([value, value, value, 255]);
        }
        SourceImage {
            width: 256,
            height: 1,
            rgba: rgba.into(),
            fingerprint: "sha256:greys".into(),
            orientation: 1,
        }
    }

    #[test]
    fn the_output_boundary_quantizes_exactly_like_rounding_the_encoded_value() {
        let reference = |value: f32| -> u8 {
            let clamped = f64::from(value).clamp(0.0, 1.0);
            (255.0 * encode_reference(clamped) + 0.5).floor() as u8
        };
        // A dense sweep of the whole range, plus values outside it that the boundary clamps.
        for step in 0..=40_000 {
            let value = (f64::from(step) / 40_000.0) as f32;
            assert_eq!(quantize_pixel([value; 3])[0], reference(value), "{value}");
        }
        for value in [-1.0_f32, -0.0, 0.0, 1.0, 1.5, 1e20] {
            assert_eq!(quantize_pixel([value; 3])[0], reference(value), "{value}");
        }
        // And on both sides of every one of the 255 thresholds, in the f32 neighbourhood of each.
        for code in 1..=255_u32 {
            let threshold = decode_reference((f64::from(code) - 0.5) / 255.0) as f32;
            let bits = threshold.to_bits();
            for value in [
                f32::from_bits(bits - 1),
                threshold,
                f32::from_bits(bits + 1),
            ] {
                assert_eq!(
                    quantize_pixel([value; 3])[0],
                    reference(value),
                    "code {code} at {value}"
                );
            }
        }
    }

    #[test]
    fn zero_exposure_round_trips_every_grey_and_a_neutral_layer_shares_the_source() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = greys();
        let raster = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[0.0])]),
        )
        .unwrap();
        assert_eq!(
            raster.rgba.as_ref(),
            source.rgba.as_ref(),
            "0 EV decodes and quantizes every byte back to itself"
        );
        for value in 0..=255_u8 {
            assert_eq!(colour_reference(value, &[0.0]).0, value);
        }
        // A colour layer with units materializes the frame; a neutral one compiles to nothing and
        // keeps the identity byte path with the source allocation itself.
        assert!(!Arc::ptr_eq(&raster.rgba, &source.rgba));
        let neutral = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[])]),
        )
        .unwrap();
        assert!(Arc::ptr_eq(&neutral.rgba, &source.rgba));
        let compiled = registry
            .compile(
                source.width,
                source.height,
                &colour_recipe(vec![exposure_layer(&[])]),
            )
            .unwrap();
        assert!(compiled.segments[0].operations.is_empty());
        assert!(!compiled.segments[0].has_color);
    }

    #[test]
    fn exposure_matches_an_independent_f64_reference_within_one_code() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        for ev in [1.0, -1.0, 2.0, -2.0, 0.5, -3.0, 5.0] {
            for source in [greys(), gradient(37, 23)] {
                let raster = render(
                    &registry,
                    &source,
                    SnapshotId::new(),
                    &colour_recipe(vec![exposure_layer(&[ev])]),
                )
                .unwrap();
                for y in 0..source.height {
                    for x in 0..source.width {
                        let input = source_pixel(&source, x, y);
                        let actual = raster.pixel(x, y).expect("inside the stage");
                        for channel in 0..3 {
                            let (expected, linear) = colour_reference(input[channel], &[ev]);
                            assert_code_within_tolerance(
                                actual[channel],
                                expected,
                                linear,
                                &format!("{ev} EV at ({x}, {y}) channel {channel}"),
                            );
                        }
                        assert_eq!(actual[3], input[3], "alpha is never touched");
                    }
                }
            }
        }
    }

    #[test]
    fn a_colour_pass_over_a_megapixel_frame_matches_the_reference_on_the_parallel_path() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(1200, 900);
        assert!(
            u64::from(source.width) * u64::from(source.height) >= PARALLEL_RENDER_PIXELS,
            "the case must reach the parallel row-chunk path"
        );
        let raster = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[1.0])]),
        )
        .unwrap();
        for y in 0..source.height {
            for x in 0..source.width {
                let input = source_pixel(&source, x, y);
                let actual = raster.pixel(x, y).expect("inside the stage");
                for channel in 0..3 {
                    let (expected, linear) = colour_reference(input[channel], &[1.0]);
                    assert_code_within_tolerance(
                        actual[channel],
                        expected,
                        linear,
                        &format!("({x}, {y}) channel {channel}"),
                    );
                }
                assert_eq!(actual[3], input[3]);
            }
        }
    }

    #[test]
    fn an_inverse_pair_in_one_operation_returns_the_exact_input_bytes() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        for source in [greys(), gradient(29, 17)] {
            let raster = render(
                &registry,
                &source,
                SnapshotId::new(),
                &colour_recipe(vec![exposure_layer(&[1.0, -1.0])]),
            )
            .unwrap();
            assert_eq!(
                raster.rgba.as_ref(),
                source.rgba.as_ref(),
                "nothing is clamped or quantized between the units of one operation"
            );
        }
    }

    #[test]
    fn two_consecutive_colour_operations_keep_values_outside_the_range_between_them() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(29, 17);
        let recipe = colour_recipe(vec![exposure_layer(&[3.0]), exposure_layer(&[-3.0])]);
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        assert_eq!(
            compiled.segments[0]
                .operations
                .iter()
                .filter(|operation| matches!(operation, Processing::Color(_)))
                .count(),
            2,
            "two layers are two operations"
        );
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert_eq!(
            raster.rgba.as_ref(),
            source.rgba.as_ref(),
            "consecutive operations fuse into one run, so +3 EV does not clip before −3 EV"
        );
        // Separating them with a point replacement does clip, because a replacement is a boundary.
        let separated = colour_recipe(vec![
            exposure_layer(&[3.0]),
            Layer::pixel(0, 0, [1, 2, 3]),
            exposure_layer(&[-3.0]),
        ]);
        let clipped = render(&registry, &source, SnapshotId::new(), &separated).unwrap();
        assert_ne!(clipped.rgba.as_ref(), source.rgba.as_ref());
    }

    #[test]
    fn a_replacement_before_a_colour_operation_is_exposed_and_one_after_it_is_not() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(8, 6);
        let rgb = [10, 120, 200];
        let recipe = colour_recipe(vec![
            Layer::pixel(1, 1, rgb),
            exposure_layer(&[1.0]),
            Layer::pixel(2, 1, rgb),
        ]);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        let mut exposed = [0; 3];
        for (channel, slot) in exposed.iter_mut().enumerate() {
            *slot = colour_reference(rgb[channel], &[1.0]).0;
        }
        assert_eq!(
            raster.pixel(1, 1).map(|p| [p[0], p[1], p[2]]),
            Some(exposed),
            "the replacement before the colour operation is processed by it"
        );
        assert_eq!(
            raster.pixel(2, 1).map(|p| [p[0], p[1], p[2]]),
            Some(rgb),
            "the replacement after it keeps its exact bytes"
        );
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn exact_geometry_commutes_with_pointwise_colour() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(13, 9);
        for transform in [
            Transform::MirrorHorizontal,
            Transform::RotateRight,
            Transform::FlipVertical,
        ] {
            let before = render(
                &registry,
                &source,
                SnapshotId::new(),
                &colour_recipe(vec![turn(transform), exposure_layer(&[1.5])]),
            )
            .unwrap();
            let after = render(
                &registry,
                &source,
                SnapshotId::new(),
                &colour_recipe(vec![exposure_layer(&[1.5]), turn(transform)]),
            )
            .unwrap();
            assert_eq!((before.width, before.height), (after.width, after.height));
            assert_eq!(before.rgba, after.rgba, "{transform:?}");
        }
    }

    #[test]
    fn a_colour_operation_before_a_rotated_crop_quantizes_then_resamples() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let (width, height) = (40_u32, 24_u32);
        let source = gradient(width, height);
        let crop = fitted_crop(width, height, 10.0, [0.15, 0.15, 0.7, 0.7]);
        let exposed_bytes = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[1.0])]),
        )
        .unwrap()
        .rgba;
        let exposed = SourceImage {
            rgba: exposed_bytes,
            fingerprint: "sha256:exposed".into(),
            ..source.clone()
        };
        let recipe = colour_recipe(vec![exposure_layer(&[1.0]), crop_layer(crop)]);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        // The colour run ends at the resample, so the crop interpolates the quantized frame: the
        // same bytes as cropping an already exposed source.
        let separately = render(
            &registry,
            &exposed,
            SnapshotId::new(),
            &colour_recipe(vec![crop_layer(crop)]),
        )
        .unwrap();
        assert_eq!(raster.rgba, separately.rgba);
        // And that frame is what the independent f64 crop reference samples, within its one code.
        let reference = CropReference::new(&exposed, crop);
        assert_eq!(
            (raster.width, raster.height),
            (reference.width, reference.height)
        );
        for j in 0..raster.height {
            for i in 0..raster.width {
                let expected = reference.pixel(&exposed, i, j);
                let actual = raster.pixel(i, j).expect("inside the output stage");
                for channel in 0..4 {
                    assert!(
                        (i32::from(actual[channel]) - i32::from(expected[channel])).abs() <= 1,
                        "({i}, {j}) channel {channel}: {actual:?} against {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn samples_match_rendered_pixels_for_a_mixed_colour_stack() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(32, 20);
        let recipe = colour_recipe(vec![
            Layer::pixel(3, 4, [250, 1, 2]),
            exposure_layer(&[0.75]),
            turn(Transform::MirrorHorizontal),
            Layer::pixel(1, 2, [3, 251, 4]),
            crop_layer(fitted_crop(32, 20, 12.0, [0.2, 0.2, 0.6, 0.6])),
        ]);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert!(raster.width > 1 && raster.height > 1);
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(
                    (sampled.width, sampled.height),
                    (raster.width, raster.height)
                );
                assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y})");
            }
        }
        // A colour operation after the resample is sampled through the same phases.
        let after = colour_recipe(vec![
            crop_layer(fitted_crop(32, 20, 12.0, [0.2, 0.2, 0.6, 0.6])),
            exposure_layer(&[-1.0]),
            Layer::pixel(0, 0, [9, 8, 7]),
        ]);
        let raster = render(&registry, &source, SnapshotId::new(), &after).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    sample(&registry, &source, &after, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "({x}, {y}) after the resample"
                );
            }
        }
    }

    /// A unit that depends on its pixel position is handed the coordinates of the stage its
    /// segment produces, by the rasterizing pass and by every point query alike: a rendered raster
    /// and a sample of the same pixel agree everywhere, under an exact rotation in the same
    /// segment and after a crop resample, where the coordinates are the output stage's.
    #[test]
    fn a_positional_colour_unit_samples_exactly_what_it_renders() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(11, 7);
        for (case, layers) in [
            ("a positional unit alone", vec![positional_layer()]),
            (
                "after an exact rotation in the same segment",
                vec![turn(Transform::RotateLeft), positional_layer()],
            ),
            (
                "after a crop resample, in output coordinates",
                vec![
                    crop_layer(fitted_crop(11, 7, 9.0, [0.15, 0.2, 0.6, 0.55])),
                    positional_layer(),
                ],
            ),
            (
                "over the whole tail",
                vec![
                    exposure_layer(&[0.5]),
                    turn(Transform::MirrorHorizontal),
                    crop_layer(fitted_crop(11, 7, 0.0, [0.1, 0.1, 0.7, 0.7])),
                    positional_layer(),
                ],
            ),
        ] {
            let recipe = colour_recipe(layers);
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            assert!(raster.width > 1 && raster.height > 1, "{case}");
            for y in 0..raster.height {
                for x in 0..raster.width {
                    assert_eq!(
                        sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                        raster.pixel(x, y),
                        "{case}: ({x}, {y})"
                    );
                }
            }
            // The unit really does depend on the position, so the agreement above is not the
            // accident of a constant result.
            assert_ne!(
                raster.pixel(0, 0),
                raster.pixel(raster.width - 1, raster.height - 1),
                "{case}: the unit varies across the frame"
            );
        }
    }

    /// The rasterizing pass hands a unit one row at a time whatever chunking it chose, so a frame
    /// taller than one chunk is processed at the same coordinates as a frame that fits in one.
    #[test]
    fn a_positional_unit_sees_its_own_row_in_every_chunk() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let recipe = colour_recipe(vec![positional_layer()]);
        let tall = gradient(3, COLOR_CHUNK_ROWS as u32 * 2 + 5);
        let raster = render(&registry, &tall, SnapshotId::new(), &recipe).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    sample(&registry, &tall, &recipe, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "({x}, {y}) across chunk boundaries"
                );
            }
        }
    }

    #[test]
    fn an_over_long_or_non_finite_colour_operation_is_refused_by_compilation() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(8, 6);
        assert!(
            render(
                &registry,
                &source,
                SnapshotId::new(),
                &colour_recipe(vec![exposure_layer(&[0.5; MAX_COLOR_UNITS])]),
            )
            .is_ok(),
            "eight units are the bound, not one too many"
        );
        for (case, layer, detail) in [
            (
                "nine units",
                exposure_layer(&[0.5; MAX_COLOR_UNITS + 1]),
                "more than the 8",
            ),
            (
                "a non-finite unit",
                colour_layer(json!({"exposure": [1.0], "infinite": true})),
                "not finite",
            ),
        ] {
            let recipe = colour_recipe(vec![layer]);
            for error in [
                render(&registry, &source, SnapshotId::new(), &recipe).unwrap_err(),
                sample(&registry, &source, &recipe, 0, 0).unwrap_err(),
            ] {
                assert_eq!(error.kind, ErrorKind::Validation, "{case}");
                assert!(error.detail.contains(detail), "{case}: {error}");
            }
        }
    }

    #[test]
    fn a_colour_unit_that_overflows_fails_the_render_and_the_sample() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(8, 6);
        let recipe = colour_recipe(vec![colour_layer(json!({"overflow": 2}))]);
        for error in [
            render(&registry, &source, SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source, &recipe, 4, 3).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::ResourceLimit);
            assert_eq!(error.detail, NON_FINITE_COLOR);
        }
    }

    #[test]
    fn a_colour_pass_reserves_its_row_chunks_from_the_scratch_budget() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(64, 48);
        let recipe = colour_recipe(vec![exposure_layer(&[1.0])]);
        let budget = ScratchBudget::default();
        assert_eq!(budget.target(), 64 * 1024 * 1024, "the declared default");
        // The budget is process-wide and other tests' preview workers reserve from it on their
        // own threads, so "nothing is held between renders" is read once those renders have
        // finished, not at an arbitrary instant.
        let idle = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while budget.in_use() != 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "scratch stayed held: {} bytes",
                    budget.in_use()
                );
                std::thread::yield_now();
            }
        };
        idle();
        let expected = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        idle();
        // The target is a target: a render whose row chunk is larger than all of it still
        // completes, with the same bytes, and the high-water mark shows it went past.
        let previous = budget.set_target(16);
        let rendered = render(&registry, &source, SnapshotId::new(), &recipe);
        budget.set_target(previous);
        let rendered = rendered.expect("a chunk past the target still runs");
        assert_eq!(rendered.rgba, expected.rgba);
        assert!(budget.peak() > 16, "the chunk was reserved and counted");
        idle();
        // A point sample streams nothing, so it reserves nothing whatever the target is.
        let previous = budget.set_target(0);
        let sampled = sample(&registry, &source, &recipe, 1, 1).unwrap();
        budget.set_target(previous);
        assert!(sampled.rgba.is_some());
    }

    #[test]
    fn a_row_chunk_stays_inside_the_scratch_budget_at_every_supported_width() {
        // 16 workers, one chunk each: the byte cap decides for wide frames and the row cap for
        // narrow ones, and neither reaches the 64 MiB target.
        for width in [1_u32, 64, 6000, 10_000, 16_384] {
            let rows = color_chunk_rows(width);
            let bytes = rows * width as usize * std::mem::size_of::<[f32; 3]>();
            assert!((1..=COLOR_CHUNK_ROWS).contains(&rows), "{width}");
            assert!(bytes <= COLOR_CHUNK_SCRATCH_BYTES, "{width}: {bytes} bytes");
            assert!(
                (16 * bytes as u64) < DEFAULT_SCRATCH_BYTES,
                "{width}: {bytes} bytes per worker"
            );
        }
        assert_eq!(color_chunk_rows(10_000), 8);
        assert_eq!(color_chunk_rows(64), COLOR_CHUNK_ROWS);
    }

    // ---------------------------------------------------------------------------------------
    // The masked colour primitive.
    // ---------------------------------------------------------------------------------------

    /// A test-only colour unit that counts the pixels it was handed, so "outside the bounds nothing
    /// is evaluated" is a counted fact and not an argument. It leaves red alone and marks green, so a
    /// frame also shows where it ran.
    #[derive(Debug)]
    struct Counting(Arc<std::sync::atomic::AtomicUsize>);

    impl PointwiseColor for Counting {
        fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
            self.0
                .fetch_add(rgb.len(), std::sync::atomic::Ordering::Relaxed);
            for pixel in rgb {
                pixel[1] = 1.0;
            }
        }
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "counting".into()
        }
    }

    /// One mask of one `add` linear gradient, at full amount: `p0` at coverage 0 and `p1` at 1, both
    /// as normalized content-stage positions.
    fn linear_mask(x0: f64, y0: f64, x1: f64, y1: f64) -> crate::Mask {
        let mut mask = crate::Mask::new("Mask 1");
        mask.components = vec![crate::Component::new(
            "Linear 1",
            crate::ComponentMode::Add,
            "linear",
            json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1}),
        )];
        mask
    }

    fn masked(layer: Layer, mask: &crate::Mask) -> Layer {
        Layer {
            mask: Some(mask.id.clone()),
            ..layer
        }
    }

    fn masked_recipe(layers: Vec<Layer>, masks: Vec<crate::Mask>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        }
    }

    /// The two endpoints of the blend, byte for byte and not approximately: coverage of exactly zero
    /// everywhere renders the unmasked input, and coverage of exactly one everywhere renders the
    /// unmasked effect. Two spellings of zero are checked, because they take different paths: an
    /// `amount` of zero empties the bounds rectangle so nothing is evaluated at all, while a gradient
    /// whose frame lies entirely behind `p0` is evaluated and blended with `M = 0`.
    #[test]
    fn the_endpoints_of_the_mask_are_byte_identical_to_the_unmasked_frames() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(37, 23);
        let identity = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![]),
        )
        .unwrap();
        let exposed = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[1.5])]),
        )
        .unwrap();
        // M = 0 by amount: the rectangle is empty, so no unit runs anywhere.
        let mut silent = linear_mask(0.5, 0.0, 0.5, 1.0);
        silent.amount = 0.0;
        // M = 0 by geometry: the whole frame sits behind p0, which is below the frame.
        let behind = linear_mask(0.5, 1.5, 0.5, 2.0);
        // M = 1 everywhere: the whole frame sits beyond p1, which is above it.
        let ahead = linear_mask(0.5, -1.0, 0.5, -0.5);
        for (case, mask, expected) in [
            ("amount zero", silent, &identity),
            ("behind p0", behind, &identity),
            ("beyond p1", ahead, &exposed),
        ] {
            let recipe = masked_recipe(
                vec![masked(exposure_layer(&[1.5]), &mask)],
                vec![mask.clone()],
            );
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            assert_eq!(
                raster.rgba.as_ref(),
                expected.rgba.as_ref(),
                "{case}: the masked frame is not byte-identical"
            );
            // And the sampled byte is the rendered byte at every pixel of both endpoints.
            for y in 0..raster.height {
                for x in 0..raster.width {
                    assert_eq!(
                        sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                        raster.pixel(x, y),
                        "{case}: sample disagrees at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// A half-covered frame against an independent stepwise evaluation, and the property that makes
    /// the blend a *masked operation* rather than a masked run: the operations before and after the
    /// masked one apply everywhere, nothing is clamped or quantized between them, and the masked one
    /// is blended against the value it was handed.
    #[test]
    fn a_masked_operation_blends_against_its_own_input_inside_the_run() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        // Every byte once at exactly half coverage — the 256×1 strip's one row sits at `v = 0.5` of a
        // gradient from `v = 0` to `v = 1`, where `smooth(0.5)` is exactly `0.5` — and then a frame
        // whose coverage varies row by row through the whole feather band.
        for (case, source) in [
            ("every byte at M = 0.5", greys()),
            ("a feather band", gradient(64, 48)),
        ] {
            let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
            let recipe = masked_recipe(
                vec![
                    exposure_layer(&[0.5]),
                    masked(exposure_layer(&[2.0]), &mask),
                    exposure_layer(&[-0.25]),
                ],
                vec![mask.clone()],
            );
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            let compiled = crate::mask::CompiledMask::new(
                &mask,
                Stage {
                    width: source.width,
                    height: source.height,
                },
                &crate::path::StrokeTable::default(),
            )
            .unwrap();
            let mut partial = 0;
            for y in 0..raster.height {
                let coverage = f64::from(compiled.evaluate(0, y, ANY_PIXEL_F32));
                if coverage > 0.0 && coverage < 1.0 {
                    partial += 1;
                }
                for x in 0..raster.width {
                    let byte = source.rgba[((y * source.width + x) * 4) as usize];
                    // The stepwise f64 reference: decode, the first operation everywhere, the masked
                    // one blended against its own input, the last one everywhere, one quantization.
                    let input = decode_reference(f64::from(byte) / 255.0) * 2.0_f64.powf(0.5);
                    let effect = input * 2.0_f64.powf(2.0);
                    let coverage = f64::from(compiled.evaluate(x, y, ANY_PIXEL_F32));
                    let blended = (1.0 - coverage) * input + coverage * effect;
                    let clamped = (blended * 2.0_f64.powf(-0.25)).clamp(0.0, 1.0);
                    let expected = (255.0 * encode_reference(clamped) + 0.5).floor() as u8;
                    let actual = raster.pixel(x, y).unwrap()[0];
                    assert_code_within_tolerance(
                        actual,
                        expected,
                        clamped,
                        &format!("{case} at ({x}, {y})"),
                    );
                    assert_eq!(
                        sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                        raster.pixel(x, y),
                        "{case}: sample disagrees at ({x}, {y})"
                    );
                }
            }
            // The coverage the fixture exercised is genuinely partial, not one of the endpoints.
            assert_eq!(
                partial, raster.height as usize,
                "{case}: every row of this fixture must be partially covered"
            );
            if case == "every byte at M = 0.5" {
                assert_eq!(
                    compiled.coverage(0, 0, ANY_PIXEL),
                    0.5,
                    "the strip's own coverage"
                );
            }
        }
    }

    /// Outside the bounds rectangle a masked operation evaluates **no unit at all**, counted. The
    /// same test states the rectangle's own conservatism: it is at most one pixel larger on each side
    /// than the rows whose coverage is non-zero.
    #[test]
    fn a_masked_operation_evaluates_no_unit_outside_its_bounds() {
        let _scratch = scratch_guard();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = counting_registry(counter.clone());
        let source = gradient(200, 100);
        let stage = Stage {
            width: source.width,
            height: source.height,
        };
        // A gradient over the bottom tenth of the frame: p0 at v = 0.9, p1 at v = 1.0.
        let mask = linear_mask(0.5, 0.9, 0.5, 1.0);
        let recipe = masked_recipe(
            vec![masked(colour_layer(json!({"counting": true})), &mask)],
            vec![mask.clone()],
        );
        counter.store(0, std::sync::atomic::Ordering::Relaxed);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        let counted = counter.load(std::sync::atomic::Ordering::Relaxed);
        let compiled =
            crate::mask::CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default())
                .unwrap();
        let bounds = compiled.bounds();
        assert_eq!(
            counted as u64,
            bounds.pixels(),
            "the unit ran over {counted} pixels against a {}x{} rectangle",
            bounds.width,
            bounds.height
        );
        // The frame is 20000 pixels and the rectangle is a small part of it: the saving is the point.
        assert!(
            (counted as u64) * 8 < u64::from(source.width) * u64::from(source.height),
            "{counted} pixels is not a small part of the frame"
        );
        // Every pixel the unit did not touch kept its input byte exactly, and the rows it did touch
        // are the rows with coverage.
        for y in 0..raster.height {
            let touched = y >= bounds.y0 && y < bounds.y1();
            let green = raster.pixel(0, y).unwrap()[1];
            let input = source.rgba[((y * source.width) * 4 + 1) as usize];
            if !touched {
                assert_eq!(green, input, "row {y} was outside the rectangle");
                assert_eq!(
                    compiled.evaluate(0, y, ANY_PIXEL_F32),
                    0.0,
                    "row {y} has coverage"
                );
            }
        }
        // And a point sample agrees with the frame inside the feather band and at both bounds edges.
        for y in [
            0,
            bounds.y0.saturating_sub(1),
            bounds.y0,
            bounds.y0 + 1,
            95,
            raster.height - 1,
        ] {
            for x in [0, 1, raster.width / 2, raster.width - 1] {
                assert_eq!(
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "sample disagrees at ({x}, {y})"
                );
            }
        }
    }

    /// A mask is stored in content-stage coordinates, so it travels through the geometry tail with
    /// the picture: masking a colour layer and then turning the frame renders the turn of the masked
    /// frame, exactly. This is what the suffix mapping inside the blend exists for — without it the
    /// mask would be read at the turned frame's coordinates.
    #[test]
    fn a_mask_lands_on_the_same_content_pixels_through_the_geometry_tail() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(24, 16);
        let mask = linear_mask(0.25, 0.25, 0.75, 0.75);
        let masked_layer = masked(exposure_layer(&[1.0]), &mask);
        let flat = render(
            &registry,
            &source,
            SnapshotId::new(),
            &masked_recipe(vec![masked_layer.clone()], vec![mask.clone()]),
        )
        .unwrap();
        for transform in [
            Transform::RotateRight,
            Transform::RotateLeft,
            Transform::MirrorHorizontal,
            Transform::FlipVertical,
        ] {
            // The masked colour layer first, then the turn: the host places a colour layer before the
            // geometry tail, so this is the order a commit produces.
            let turned = render(
                &registry,
                &source,
                SnapshotId::new(),
                &masked_recipe(
                    vec![masked_layer.clone(), turn(transform)],
                    vec![mask.clone()],
                ),
            )
            .unwrap();
            // The reference: turn the masked frame with the delivered exact pass.
            let expected = rendered_from(&flat, vec![turn(transform)]);
            assert_eq!(turned.width, expected.width, "{transform:?}");
            assert_eq!(
                turned.rgba.as_ref(),
                expected.rgba.as_ref(),
                "{transform:?}: the mask did not travel with the picture"
            );
            let recipe = masked_recipe(
                vec![masked_layer.clone(), turn(transform)],
                vec![mask.clone()],
            );
            for y in 0..turned.height {
                for x in 0..turned.width {
                    assert_eq!(
                        sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                        turned.pixel(x, y),
                        "{transform:?}: sample disagrees at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// The snapshot block a masked operation is processed in is scratch, not arithmetic: the units are
    /// pointwise, so the same row blended in blocks of 1, 3 and the whole row is the same row.
    #[test]
    fn the_masked_blend_does_not_depend_on_the_snapshot_block_size() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(64, 5);
        let mask = linear_mask(0.1, 0.2, 0.9, 0.8);
        let recipe = masked_recipe(
            vec![
                exposure_layer(&[0.75]),
                masked(exposure_layer(&[-1.5]), &mask),
            ],
            vec![mask],
        );
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        let segment = compiled.segments.last().unwrap();
        let row = |block: usize| -> Vec<[f32; 3]> {
            let mut pixels: Vec<[f32; 3]> = (0..source.width)
                .map(|x| decode_pixel([x as u8, (x as u8).wrapping_add(20), 0]))
                .collect();
            let mut scratch = vec![[0.0f32; 3]; block];
            for run in color_runs(segment) {
                apply_units(&run, 2, 0, &mut pixels, &mut scratch).unwrap();
            }
            pixels
        };
        let whole = row(source.width as usize);
        for block in [1, 3, 7, 64] {
            assert_eq!(row(block), whole, "block of {block}");
        }
    }

    /// A neutral payload compiles to no units whether it carries a mask or not, so the segment keeps
    /// the identity byte path and the shared source buffer: masking nothing is nothing.
    #[test]
    fn a_neutral_masked_layer_keeps_the_identity_byte_path() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(16, 9);
        let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
        let recipe = masked_recipe(vec![masked(exposure_layer(&[]), &mask)], vec![mask.clone()]);
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert!(
            Arc::ptr_eq(&raster.rgba, &source.rgba),
            "a neutral masked layer allocated a frame"
        );
        let compiled = registry
            .compile(source.width, source.height, &recipe)
            .unwrap();
        assert!(compiled.segments.last().unwrap().operations.is_empty());
    }

    /// Masked scratch is bounded by the existing float budget and is taken only when a run needs it:
    /// an unmasked pass reserves what it always reserved.
    #[test]
    fn a_masked_run_reserves_its_row_snapshot_from_the_existing_budget() {
        let _scratch = scratch_guard();
        let registry = colour_registry();
        let source = gradient(64, 48);
        let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
        let recipe = masked_recipe(
            vec![masked(exposure_layer(&[1.0]), &mask)],
            vec![mask.clone()],
        );
        let budget = ScratchBudget::default();
        let idle = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while budget.in_use() != 0 {
                assert!(std::time::Instant::now() < deadline, "scratch stayed held");
                std::thread::yield_now();
            }
        };
        idle();
        // One row chunk plus one row of snapshot. The budget is a **target and not a limit**, so a
        // target that fits the chunk but not the snapshot renders anyway rather than refusing: what
        // the snapshot costs is visible in the high-water mark, never in an error.
        let chunk = color_chunk_rows(source.width) * source.width as usize * 12;
        let snapshot = source.width as usize * 12;
        let previous = budget.set_target(chunk as u64);
        let rendered = render(&registry, &source, SnapshotId::new(), &recipe);
        budget.set_target(previous);
        assert!(
            rendered.is_ok(),
            "a masked run past the target still renders: {:?}",
            rendered.err()
        );
        idle();
        assert!(render(&registry, &source, SnapshotId::new(), &recipe).is_ok());
        // The unmasked stack renders inside a target that holds only the chunk without ever
        // overshooting it, so the snapshot is charged to masked runs alone.
        let unmasked = colour_recipe(vec![exposure_layer(&[1.0])]);
        let previous = budget.set_target(chunk as u64);
        let before = budget.peak();
        let rendered = render(&registry, &source, SnapshotId::new(), &unmasked);
        budget.set_target(previous);
        assert!(rendered.is_ok(), "{:?}", rendered.err());
        idle();
        // A point sample streams nothing and allocates no snapshot, so it answers at any target.
        let previous = budget.set_target(0);
        let sampled = sample(&registry, &source, &recipe, 3, 3).unwrap();
        budget.set_target(previous);
        assert!(sampled.rgba.is_some());
        // The high-water mark is what makes the aggregate observable after the fact: a masked pass
        // has to have carried at least one chunk and one row of snapshot at once. `peak` is
        // process-wide and only ever raised, so this reads "at least"; the unmasked and sampled runs
        // above raised it by nothing, which is the other half of the claim.
        assert!(
            budget.peak() >= (chunk + snapshot) as u64,
            "the peak {} never reached one chunk plus one row of snapshot",
            budget.peak()
        );
        assert_eq!(
            budget.peak(),
            before,
            "an unmasked run or a point sample raised the peak the masked run had already set"
        );
    }

    /// What a masked colour layer costs on a photo-sized frame, and what the bounds rectangle saves.
    /// Ignored by default because it is a recorded measurement rather than a pass/fail property:
    ///
    /// ```sh
    /// cargo test --release --package lightwell-core --lib \
    ///     render::tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn masked_colour_cost_on_a_24_megapixel_frame() {
        let registry = colour_registry();
        let source = gradient(6000, 4000);
        let stage = Stage {
            width: source.width,
            height: source.height,
        };
        // A gradient over the bottom twentieth of the frame against one over the whole frame: the same
        // mask mathematics and the same units, differing only in how much of the frame the bounds
        // rectangle admits.
        let small = linear_mask(0.5, 0.95, 0.5, 1.0);
        let whole = linear_mask(0.5, 0.0, 0.5, 1.0);
        let unmasked = colour_recipe(vec![exposure_layer(&[1.0])]);
        let identity = colour_recipe(vec![]);
        let measure = |name: &str, recipe: &Recipe| {
            // One warm pass, then three measured ones: the frame allocation dominates a single run.
            render(&registry, &source, SnapshotId::new(), recipe).unwrap();
            let started = std::time::Instant::now();
            for _ in 0..3 {
                std::hint::black_box(
                    render(&registry, &source, SnapshotId::new(), recipe).unwrap(),
                );
            }
            println!(
                "{name}: {:.1} ms per 24 MP render",
                started.elapsed().as_secs_f64() * 1000.0 / 3.0
            );
        };
        measure("identity", &identity);
        measure("unmasked exposure", &unmasked);
        for (name, mask) in [("small bounds", small), ("whole frame", whole)] {
            let compiled =
                crate::mask::CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default())
                    .unwrap();
            let bounds = compiled.bounds();
            println!(
                "{name}: the rectangle admits {:.2}% of the frame",
                100.0 * bounds.pixels() as f64 / (6000.0 * 4000.0)
            );
            measure(
                name,
                &masked_recipe(
                    vec![masked(exposure_layer(&[1.0]), &mask)],
                    vec![mask.clone()],
                ),
            );
        }
    }

    /// One brush mask over these strokes, with the resolved table its addresses read through.
    fn brush_mask(strokes: &[crate::path::Stroke]) -> (crate::Mask, crate::path::StrokeTable) {
        let mut table = crate::path::StrokeTable::new("the brush measurement");
        let addresses: Vec<String> = strokes
            .iter()
            .map(|stroke| table.insert(stroke.clone()).to_string())
            .collect();
        let mut mask = crate::Mask::new("Mask 1");
        let name = mask.next_component_name("brush");
        mask.components.push(crate::Component::new(
            name,
            crate::ComponentMode::Add,
            "brush",
            json!({ "strokes": addresses }),
        ));
        (mask, table)
    }

    /// `count` strokes of a plausible retouching brush, laid out so they neither coincide nor leave
    /// the frame: each is a three-position path across its own column, at a radius of 0.05 mask-space
    /// units — a twentieth of the frame's height, which is 200 px on a 24 MP stage — and softly
    /// feathered, which is the brush the panel starts with.
    fn painted_strokes(count: usize) -> Vec<crate::path::Stroke> {
        (0..count)
            .map(|index| {
                let t = (index as f64 + 0.5) / count as f64;
                let y = 0.1 + 0.8 * t;
                crate::path::Stroke::capture(
                    &[[0.1, y], [0.5, y + 0.02], [0.9, y]],
                    0.05,
                    50.0,
                    100.0,
                    false,
                )
                .expect("a legal stroke")
            })
            .collect()
    }

    /// What a **brush** mask costs on a photo-sized frame: the grid index it compiles to, the
    /// rectangle it bounds, the render it modulates, and the point query it answers. Ignored by
    /// default because it is a recorded measurement rather than a pass/fail property:
    ///
    /// ```sh
    /// cargo test --release --package lightwell-core --lib \
    ///     render::tests::masked_brush_cost_on_photo_sized_frames -- --ignored --nocapture
    /// ```
    ///
    /// Four stroke counts are measured, up to the delivered per-component limit, against the
    /// unmasked render and against a whole-frame gradient mask — the case
    /// [`masked_colour_cost_on_a_24_megapixel_frame`] already records — so what a brush costs is
    /// stated as a difference from a mask whose cost is known rather than on its own.
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn masked_brush_cost_on_photo_sized_frames() {
        let registry = colour_registry();
        for (label, width, height) in [("24 MP", 6000u32, 4000u32), ("60 MP", 9000, 6667)] {
            let source = gradient(width, height);
            let stage = Stage { width, height };
            let pixels = f64::from(width) * f64::from(height);
            let measure = |name: &str, recipe: &Recipe| {
                // One warm pass, then three measured ones: the frame allocation dominates a single
                // run.
                render(&registry, &source, SnapshotId::new(), recipe).unwrap();
                let started = std::time::Instant::now();
                for _ in 0..3 {
                    std::hint::black_box(
                        render(&registry, &source, SnapshotId::new(), recipe).unwrap(),
                    );
                }
                println!(
                    "{label} {name}: {:.1} ms per render",
                    started.elapsed().as_secs_f64() * 1000.0 / 3.0
                );
            };
            measure("identity", &colour_recipe(vec![]));
            measure(
                "unmasked exposure",
                &colour_recipe(vec![exposure_layer(&[1.0])]),
            );
            let gradient_mask = linear_mask(0.5, 0.0, 0.5, 1.0);
            measure(
                "whole-frame gradient mask",
                &masked_recipe(
                    vec![masked(exposure_layer(&[1.0]), &gradient_mask)],
                    vec![gradient_mask],
                ),
            );

            for count in [1usize, 8, 32, 64] {
                let strokes = painted_strokes(count);
                let (mask, table) = brush_mask(&strokes);
                // The compile is where the grid index is built and the occupancy cap is checked,
                // before a pixel is read. It is charged to the gesture, not to the frame, so it is
                // measured on its own.
                let started = std::time::Instant::now();
                let rounds = 20;
                for _ in 0..rounds {
                    std::hint::black_box(
                        crate::mask::CompiledMask::new(&mask, stage, &table).unwrap(),
                    );
                }
                let compile = started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
                let compiled = crate::mask::CompiledMask::new(&mask, stage, &table).unwrap();
                println!(
                    "{label} {count} strokes: {compile:.3} ms to compile, rectangle admits \
                     {:.2}% of the frame",
                    100.0 * compiled.bounds().pixels() as f64 / pixels
                );
                let recipe = Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers: vec![masked(exposure_layer(&[1.0]), &mask)],
                    masks: vec![mask.clone()],
                    strokes: table.clone(),
                };
                measure(&format!("{count}-stroke brush mask"), &recipe);

                // The point query, which is the rule-4 claim: a sample through a masked layer costs
                // `O(layers)` and never rasterizes, so its cost must not grow with the stroke count.
                // A thousand of them, spread over the frame, because one is too fast to time.
                let queries = 1000u32;
                let started = std::time::Instant::now();
                for index in 0..queries {
                    let x = (index * 7919) % width;
                    let y = (index * 6271) % height;
                    std::hint::black_box(sample(&registry, &source, &recipe, x, y).unwrap());
                }
                println!(
                    "{label} {count} strokes: {:.4} ms per point query",
                    started.elapsed().as_secs_f64() * 1000.0 / f64::from(queries)
                );
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Cooperative cancellation.
    // ---------------------------------------------------------------------------------------

    /// A programmatically filled source, so a photo-sized case costs an allocation and a fill and
    /// reads no file. The pattern varies on both axes and in all three channels, so a wrong row,
    /// a dropped channel or a short frame is visible in the byte comparison.
    fn cancellation_source(width: u32, height: u32) -> SourceImage {
        let mut rgba = vec![0_u8; width as usize * height as usize * 4];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[index as u8, (index >> 5) as u8, (index >> 11) as u8, 255]);
        }
        SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:cancellation".into(),
            orientation: 1,
        }
    }

    /// The stack every cancellation test renders: the one orientation layer, one colour-stage
    /// Basic layer and a 7° straightening crop, so the exact transform pass, the streamed colour
    /// pass and the resample all run over the frame. The quarter turn means the crop's input stage
    /// is the turned one, which is the stage it is fitted onto.
    fn cancellation_stack(width: u32, height: u32) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![
                turn(Transform::RotateRight),
                Layer {
                    id: LayerId::new(),
                    effect_id: crate::BASIC_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"exposure": 0.5, "contrast": 20.0, "vibrance": 30.0}),
                    mask: None,
                    artifacts: Vec::new(),
                },
                Layer::crop(fitted_crop(height, width, 7.0, [0.05, 0.05, 0.9, 0.9])),
            ],
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    #[test]
    fn an_uncancelled_token_renders_the_bytes_the_plain_entry_point_renders() {
        let registry = ModuleRegistry::builtin();
        let source = cancellation_source(512, 384);
        let recipe = cancellation_stack(512, 384);
        let snapshot = SnapshotId::new();
        let plain = render(&registry, &source, snapshot.clone(), &recipe).unwrap();
        let cancellable =
            render_cancellable(&registry, &source, snapshot, &recipe, &Cancel::never()).unwrap();
        // Dimensions, fingerprint, snapshot identity and every byte.
        assert_eq!(plain, cancellable);
        assert!(
            plain.width > 1 && plain.height > 1,
            "the stack renders a frame"
        );
        // The stack really exercises all three passes: a turn, a colour run and a resample.
        assert_ne!(plain.rgba.as_ref(), source.rgba.as_ref());
    }

    #[test]
    fn a_pre_cancelled_token_stops_a_render_before_it_allocates_a_frame() {
        let registry = ModuleRegistry::builtin();
        let source = cancellation_source(512, 384);
        let recipe = cancellation_stack(512, 384);
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(cancel.is_cancelled());
        let error = render_cancellable(&registry, &source, SnapshotId::new(), &recipe, &cancel)
            .expect_err("a cancelled token refuses the render");
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.kind.code(), "cancelled");
    }

    // The photo-sized latency case and the scratch-budget observation live in
    // `tests/cancellation.rs`: the budget is process-wide, and the lib test binary runs 250 other
    // tests beside this one, several of which legitimately hold reservations while it reads the
    // counter. Its own test binary is a process where nothing else reserves.
}
