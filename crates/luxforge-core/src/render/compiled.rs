//! The compiled IR: a recipe as the ordered rasterizing segments the registry compiles it into,
//! with the stage boundary that produces each one's input frame.

use super::{
    Byte, Evaluation, PixelDomain, RenderContext, SpatialEntry,
    linear::{LinearRows, LinearScratch, TAP_BLOCK_PIXELS},
    map::WarpStep,
    nearest_index, resample_frame,
    spatial::Tiling,
    window::RegionFallback,
};
use crate::{
    Cancel, Error,
    mask_field::MaskField,
    modules::{ExactGeometry, Global, Processing, Region, Resample, SpatialOperation, Stage},
};
use std::sync::Arc;

/// What produces one segment's input frame, and therefore what separates it from the segment
/// before it. Every kind is a stage boundary: the frame before it is finished, it reads that frame
/// and writes the next one.
///
/// Everything the renderer asks of a boundary is one of the methods below, each a single dispatch
/// over the kinds: the stage it produces and the rectangle of it a frame holds, the rectangle it
/// reads, a windowed proxy's plan and cut of it, one point mapped back through it or evaluated
/// through it, its forward map, its materialized frame in either driver, its estimates, how the
/// linear rows load it, and whether it runs a spatial operation. A new kind of boundary is one more
/// variant with one more arm in each of these methods; no caller matches on the kind.
#[derive(Clone)]
pub(crate) enum Entry {
    /// An interpolating boundary that also changes the stage.
    Resample(ResampleEntry),
    /// A neighbourhood boundary at the same dimensions as the stage it receives.
    Spatial(SpatialEntry),
}

/// A resample boundary: the resample, and where the frame it reads and the frame it writes lie in
/// the stages it was compiled against, which only a windowed proxy's cut ([`super::window`])
/// moves.
#[derive(Clone)]
pub(crate) struct ResampleEntry {
    pub(super) resample: Resample,
    /// Where the frame this resample reads lies in the stage it was compiled against: `(0, 0)`,
    /// except behind a windowed proxy's cut.
    pub(super) origin: (u32, u32),
    /// The rectangle of the resample's full output stage held by a cut frame. Its integer origin is
    /// added before the resample's floating-point inverse map, preserving exact taps.
    pub(super) window: Option<Region>,
}

impl ResampleEntry {
    /// The rectangle of its full output stage its frame holds: [`Self::window`], or the whole
    /// stage.
    pub(super) fn window(&self) -> Region {
        self.window.unwrap_or(Region::whole(Stage {
            width: self.resample.output_width,
            height: self.resample.output_height,
        }))
    }

    /// Pixel `(x, y)` of its frame in the resample's full output stage.
    #[inline]
    pub(super) fn output_at(&self, x: u32, y: u32) -> (u32, u32) {
        match self.window {
            Some(window) => (window.x0 + x, window.y0 + y),
            None => (x, y),
        }
    }

    /// The continuous coordinate, in the frame it reads, that pixel `(x, y)` of its frame samples.
    #[inline]
    pub(super) fn input_at(&self, x: u32, y: u32) -> (f64, f64) {
        let (full_x, full_y) = self.output_at(x, y);
        self.resample.input_from(self.origin, full_x, full_y)
    }

    /// The rectangle of the frame it reads, a `received` stage, that `window` of its full output
    /// stage reads ([`Resample::reads`]).
    #[inline]
    pub(super) fn reads(&self, window: Region, received: Stage) -> Option<Region> {
        self.resample.reads(self.origin, window, received)
    }
}

impl Entry {
    /// A resample boundary over the whole stage it was compiled against.
    pub(crate) fn resample(resample: Resample) -> Self {
        Self::Resample(ResampleEntry {
            resample,
            origin: (0, 0),
            window: None,
        })
    }

    /// Compose another output-to-input step into the one sampling boundary.
    pub(crate) fn fuse(&mut self, step: WarpStep, output: Stage) -> Result<bool, Error> {
        let Self::Resample(entry) = self else {
            return Ok(false);
        };
        let crate::modules::Mapping::Warp(chain) = &mut entry.resample.map else {
            return Ok(false);
        };
        Arc::make_mut(chain).prepend(step)?;
        entry.resample.output_width = output.width;
        entry.resample.output_height = output.height;
        Ok(true)
    }

    pub(crate) fn has_warp(&self) -> bool {
        matches!(self, Self::Resample(entry) if entry.resample.map.has_warp())
    }

    /// A spatial boundary. `prefix_hash` is the SHA-256 of the canonical JSON of the layers before
    /// this one, which together with the source and the stage identifies what a global estimate
    /// was prepared from.
    #[cfg(test)]
    pub(crate) fn spatial(operation: SpatialOperation, prefix_hash: String) -> Self {
        Self::Spatial(SpatialEntry::new(operation, prefix_hash))
    }

    pub(crate) fn spatial_tagged(
        operation: SpatialOperation,
        prefix_hash: String,
        stage: crate::EffectStage,
    ) -> Self {
        let mut entry = SpatialEntry::new(operation, prefix_hash);
        entry.stage = stage;
        Self::Spatial(entry)
    }

    /// The whole stage this boundary produces from the whole stage it receives, before any cut.
    pub(crate) fn stage(&self, received: Stage) -> Stage {
        match self {
            Self::Resample(entry) => Stage {
                width: entry.resample.output_width,
                height: entry.resample.output_height,
            },
            Self::Spatial(_) => received,
        }
    }

    /// The rectangle of [`Self::stage`] its frame holds, given the frame it reads is a `received`
    /// stage: the whole stage, except a resample behind a windowed proxy's cut.
    pub(crate) fn held(&self, received: Stage) -> Region {
        match self {
            Self::Resample(entry) => entry.window(),
            Self::Spatial(_) => Region::whole(received),
        }
    }

    /// The rectangle of the frame it reads, a `received` stage, that producing `window` of its
    /// output stage reads: a resample's taps with their margin ([`Resample::reads`]), or a spatial
    /// operation's tiles grown by their halo ([`SpatialEntry::reads`]). `None` when it cannot say,
    /// which a caller answers by reading everything.
    pub(crate) fn reads(&self, window: Region, received: Stage) -> Option<Region> {
        match self {
            Self::Resample(entry) => entry.reads(window, received),
            Self::Spatial(entry) => Some(entry.reads(window, received)),
        }
    }

    /// One step of a windowed proxy's plan ([`super::window::WindowPlan::of_rect`]), against the
    /// uncut compilation: the rectangle of the stage before it, a `received` stage, that `read`, a
    /// rectangle of its output stage, needs. `tiled_before` says whether the stage it reads comes
    /// through an earlier spatial operation: a spatial operation that prepares a
    /// global estimate there reads the whole stage, so every segment before it is kept whole and it
    /// reduces its own input as a frame does, since no window can hand it the estimate of a stage
    /// behind another spatial operation.
    pub(crate) fn plan_window(
        &self,
        read: Region,
        received: Stage,
        tiled_before: bool,
    ) -> Result<Region, RegionFallback> {
        match self {
            Self::Resample(entry) => entry
                .resample
                .reads((0, 0), read, received)
                .ok_or(RegionFallback::UnplannableGeometry),
            Self::Spatial(entry) if entry.prepares_estimates() && tiled_before => {
                Ok(Region::whole(received))
            }
            Self::Spatial(entry) => Ok(entry.reads(read, received)),
        }
    }

    /// [`Self::plan_window`] for a boundary a GPU preview evaluates rather than the CPU
    /// ([`super::window::WindowPlan::of_gpu_rect`]): a resample's taps as the CPU reads them, which
    /// the geometry tail clamps to, and a spatial operation's halo with no tile grid
    /// ([`SpatialEntry::halo_reads`]). A global estimate is never prepared from the window: the
    /// GPU computes the whole stage's from the source by its light link, so an estimate behind an
    /// earlier spatial layer cuts like any other.
    pub(crate) fn plan_gpu_window(
        &self,
        read: Region,
        received: Stage,
    ) -> Result<Region, RegionFallback> {
        match self {
            Self::Resample(_) => self.plan_window(read, received, false),
            Self::Spatial(entry) => Ok(entry.halo_reads(read, received)),
        }
    }

    /// Cut this boundary for a windowed proxy ([`super::window::WindowPlan::apply`]): `read` is the
    /// rectangle of its whole output stage `whole` that its segment reads, and `previous` the
    /// rectangle of the whole stage it receives that the cut frame before it holds. Answers the
    /// rectangle of `whole` its own cut frame holds. `globals` answers the estimates it is handed
    /// when it prepares one over a stage that is cut; it is asked nothing otherwise.
    pub(crate) fn cut(
        &mut self,
        read: Region,
        previous: Region,
        whole: Stage,
        globals: impl FnOnce() -> Result<Vec<Option<Global>>, Error>,
    ) -> Result<Region, Error> {
        match self {
            Self::Resample(entry) => {
                // Keep only the part of its full output stage the segment's geometry reads, so a
                // small viewport never materializes the whole crop.
                entry.origin = (previous.x0, previous.y0);
                entry.window = Some(read);
                Ok(read)
            }
            Self::Spatial(entry) => {
                entry.cut(previous, whole, globals)?;
                Ok(previous)
            }
        }
    }

    /// The pixel of the frame it reads, a `received` stage, that pixel `(x, y)` of its frame shows:
    /// the nearest pixel to a resample's sampled coordinate, or the same pixel through a spatial
    /// operation, which keeps every coordinate of its stage.
    pub(crate) fn locate(&self, x: u32, y: u32, received: Stage) -> (u32, u32) {
        match self {
            Self::Resample(entry) => {
                let (u, v) = entry.input_at(x, y);
                (
                    nearest_index(u, received.width),
                    nearest_index(v, received.height),
                )
            }
            Self::Spatial(_) => (x, y),
        }
    }

    /// Append this boundary's output-to-input steps in content-to-output traversal order. The
    /// whole map evaluates these steps backwards for a content read and inverts them for an
    /// output position. Window translations retain the same global coordinates as rasterization;
    /// a spatial boundary contributes nothing because it moves no coordinate.
    pub(super) fn mapping_steps(&self, steps: &mut Vec<WarpStep>) {
        if let Self::Resample(entry) = self {
            let (x, y) = entry.origin;
            if (x, y) != (0, 0) {
                steps.push(WarpStep::Affine([
                    1.0,
                    0.0,
                    -f64::from(x),
                    0.0,
                    1.0,
                    -f64::from(y),
                ]));
            }
            steps.extend(entry.resample.map.steps_forward());
            if let Some(window) = entry.window {
                steps.push(WarpStep::Affine([
                    1.0,
                    0.0,
                    f64::from(window.x0),
                    0.0,
                    1.0,
                    f64::from(window.y0),
                ]));
            }
        }
    }

    /// Pixel `(x, y)` of the input frame of segment `index` of `evaluation`, whose entry this is:
    /// a resample's bilinear blend of the segment before it, or a spatial operation's output, from
    /// the evaluation's frame of it.
    #[inline(always)]
    pub(super) fn pixel<D: PixelDomain>(
        &self,
        evaluation: &Evaluation<'_, D>,
        index: usize,
        x: u32,
        y: u32,
    ) -> Result<D::Pixel, Error> {
        match self {
            Self::Resample(entry) => evaluation.resample_pixel(index, entry, x, y),
            Self::Spatial(_) => evaluation.spatial_entry_pixel(index, x, y),
        }
    }

    /// The frame `evaluation` materializes for segment `index`, whose entry this is, in
    /// [`super::SpatialMode::Frames`]: a spatial operation's output over its whole stage. A
    /// resample materializes nothing there; its pixels are pulled through [`Self::pixel`].
    pub(super) fn frame<D: PixelDomain>(
        &self,
        evaluation: &Evaluation<'_, D>,
        index: usize,
        cancel: &Cancel,
    ) -> Result<Option<D::SpatialFrame>, Error> {
        match self {
            Self::Resample(_) => Ok(None),
            Self::Spatial(entry) => evaluation.spatial_frame(index, entry, cancel).map(Some),
        }
    }

    /// The global estimates this boundary reads as the entry of segment `index` of `evaluation`,
    /// exactly as a frame would resolve them. Empty for a boundary that reads none.
    pub(super) fn globals<D: PixelDomain>(
        &self,
        evaluation: &Evaluation<'_, D>,
        index: usize,
    ) -> Result<Vec<Option<Global>>, Error> {
        match self {
            Self::Resample(_) => Ok(Vec::new()),
            Self::Spatial(entry) => evaluation.spatial_globals(index, entry),
        }
    }

    /// The byte driver's frame of this boundary ([`super::rasterize`]) over `input`, the finished
    /// byte frame of the `received` stage before it. Its dimensions are [`Self::held`]'s.
    // Keep the explicit driver contract: source/input, stage, scheduling, cancellation, shared
    // context and output precision are independently supplied to the same boundary dispatch.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn byte_frame(
        &self,
        domain: &Byte<'_>,
        input: &super::byte::ByteFrame,
        received: Stage,
        tiling: Tiling,
        cancel: &Cancel,
        context: &RenderContext,
        wide: bool,
    ) -> Result<super::byte::ByteFrame, Error> {
        match self {
            Self::Resample(entry) => {
                let super::byte::ByteFrame::Narrow(input) = input else {
                    unreachable!("a resample reads a narrow frame")
                };
                let frame = resample_frame(
                    input,
                    received.width,
                    received.height,
                    &entry.resample,
                    entry.origin,
                    entry.window(),
                    cancel,
                )?;
                #[cfg(test)]
                context.note_resample_bytes(frame.len());
                Ok(super::byte::ByteFrame::Narrow(frame))
            }
            Self::Spatial(entry) => super::byte::spatial_frame(
                domain, entry, input, received, tiling, cancel, context, wide,
            ),
        }
    }

    /// The bytes one worker of the linear rows reserves to load this boundary: a resample's block
    /// of taps, and nothing for a spatial frame, whose values go straight into the chunk.
    pub(super) fn linear_scratch(&self) -> usize {
        match self {
            Self::Resample(_) => TAP_BLOCK_PIXELS as usize * std::mem::size_of::<[f64; 3]>(),
            Self::Spatial(_) => 0,
        }
    }

    /// Load one chunk of the linear rows of a segment that enters through this boundary: a
    /// resample's taps block by block ([`LinearRows::load_resampled`]), or a spatial frame's rows
    /// walked through the segment's geometry, each pixel pulled through [`Self::pixel`] where the
    /// evaluation holds no frame ([`LinearRows::load_pulled`]).
    pub(super) fn load_linear(
        &self,
        rows: &LinearRows<'_, '_, '_>,
        scratch: &mut LinearScratch,
        y0: u32,
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        match self {
            Self::Resample(entry) => rows.load_resampled(entry, scratch, y0, chunk),
            Self::Spatial(_) => rows.load_pulled(scratch, y0, chunk),
        }
    }

    /// Whether a point through this boundary is a bilinear blend of the segment before it, which
    /// the linear path pulls rather than materializes, and so allows once.
    pub(crate) fn blends(&self) -> bool {
        match self {
            Self::Resample(_) => true,
            Self::Spatial(_) => false,
        }
    }

    /// The spatial operation this boundary runs, whose whole frame a read through it is
    /// answered from; `None` for a resample, which a point query pulls through.
    pub(crate) fn spatial_operation(&self) -> Option<&SpatialOperation> {
        match self {
            Self::Resample(_) => None,
            Self::Spatial(entry) => Some(&entry.operation),
        }
    }
}

/// One rasterizing pass: the exact operations that share an input frame, their composed geometry and
/// the stage they produce. `entry` is what produces this segment's input frame, so consecutive
/// segments are separated by exactly one stage boundary ([`Entry`]), and the first segment reads
/// the source.
#[derive(Clone)]
pub(crate) struct Segment {
    pub(crate) entry: Option<Entry>,
    pub(crate) operations: Vec<Processing>,
    pub(crate) geometry: ExactGeometry,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) has_pixels: bool,
    pub(crate) has_color: bool,
    /// The segment output's first pixel in its original uncut output stage. Pointwise finish
    /// units read these original coordinates even when a viewport keeps only a rectangle.
    pub(crate) output_origin: (u32, u32),
}

impl Segment {
    /// The stage this segment produces.
    pub(crate) fn stage(&self) -> Stage {
        Stage {
            width: self.width,
            height: self.height,
        }
    }

    pub(crate) fn new(entry: Option<Entry>, width: u32, height: u32) -> Self {
        Self {
            entry,
            operations: Vec::new(),
            geometry: ExactGeometry::identity(width, height),
            width,
            height,
            has_pixels: false,
            has_color: false,
            output_origin: (0, 0),
        }
    }

    /// Its resample entry took one more warp or resample into its map, which now writes an
    /// `output` stage: the segment's frame is that stage, and its geometry the identity over it.
    /// A segment only fuses while its geometry is the identity, so nothing else changes; keeping
    /// the stage it had before would map its frame onto a stage it no longer reads, which a
    /// windowed proxy's cut refuses.
    pub(crate) fn fused(&mut self, output: Stage) {
        self.width = output.width;
        self.height = output.height;
        self.geometry = ExactGeometry::identity(output.width, output.height);
    }

    /// Whether this pass writes anything into its frame. An identity pass that does not shares the
    /// source allocation instead of copying it.
    pub(super) fn writes_pixels(&self) -> bool {
        self.has_pixels || self.has_color
    }
}

/// One recipe compiled by the registry: the ordered rasterizing passes and the resamples between
/// them. A recipe without a resample is one segment, which is the M1 and M2 behavior unchanged.
///
/// `Clone` holds no pixels, only the operation lists and geometry `O(layers)` compiling already
/// allocated, so cloning a compiled prefix out of a cache to reuse it for several sampled points is
/// far cheaper than recompiling it.
#[derive(Clone)]
pub(crate) struct Compiled {
    pub(crate) segments: Vec<Segment>,
    /// Where each layer of the compiled stack begins, in stack order: the segment that was being
    /// filled when the layer compiled and how many operations it held then. A layer's operation,
    /// when it compiled to one, is at that position, and a boundary it opened is the next
    /// segment's entry; a neutral layer, or a warp fused into the entry before it, holds the
    /// position of the layer after it. What a GPU plan reads to find the input of a named layer
    /// ([`super::gpu`]). `O(layers)`, like the rest of the compilation.
    pub(crate) layers: Box<[(usize, usize)]>,
}

impl Compiled {
    /// Plan on `cancel`'s progress meter, when it has one, the spatial tiles a whole-frame render
    /// of the entries of `segments` runs: each spatial entry over the stage it receives, the
    /// output of the segment before it, or `source` for segment 0.
    pub(super) fn plan_progress(
        &self,
        cancel: &Cancel,
        source: Option<Stage>,
        segments: std::ops::Range<usize>,
        tiling: Tiling,
    ) {
        let Some(progress) = cancel.progress() else {
            return;
        };
        let tiles = segments
            .filter_map(|index| match &self.segments.get(index)?.entry {
                Some(Entry::Spatial(entry)) => {
                    let received = match index.checked_sub(1) {
                        Some(previous) => self.segments[previous].stage(),
                        None => source?,
                    };
                    Some(super::spatial::tile_count(
                        &entry.operation,
                        received,
                        tiling,
                    ))
                }
                _ => None,
            })
            .sum();
        progress.plan(tiles);
    }

    pub(super) fn last(&self) -> &Segment {
        self.segments
            .last()
            .expect("a compiled recipe always has one segment")
    }

    pub(crate) fn stage(&self) -> Stage {
        self.last().stage()
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
                Processing::Color(colour) => colour.mask().is_some_and(MaskField::supersampled),
                Processing::Spatial(spatial) => spatial.mask().is_some_and(MaskField::supersampled),
                _ => false,
            })
        })
    }

    /// Why a frame of this compilation is an approximation of the exact render at its size: a
    /// spatial operation, whose neighbourhoods scale with the stage, and a mask the proxy phase
    /// supersampled. `O(layers + components)`, no pixel read.
    pub(crate) fn approximation(&self) -> crate::ProxyApproximation {
        crate::ProxyApproximation {
            spatial: self.segments.iter().any(|s| matches!(&s.entry, Some(Entry::Spatial(e)) if e.stage == crate::EffectStage::Spatial)),
            restoration: self.segments.iter().any(|s| matches!(&s.entry, Some(Entry::Spatial(e)) if e.stage == crate::EffectStage::Restoration)),
            mask: self.supersampled_masks(),
            reduced_detail: false,
        }
    }

    /// The width the full recipe demands at the sampled prefix's last spatial boundary.
    /// Cutting off later units must not change that earlier hand-off's precision.
    pub(crate) fn prefix_spatial_input_wide(&self, prefix: &Compiled) -> bool {
        prefix
            .segments
            .iter()
            .rposition(|segment| matches!(&segment.entry, Some(Entry::Spatial(_))))
            .and_then(|index| {
                super::byte::byte_frame_widths(self)
                    .get(index)
                    .map(|width| width.input)
            })
            .unwrap_or(false)
    }

    /// Segment whose input holds the last boundary of the leading restoration run: where a test
    /// reads the width the restoration run hands its colour run.
    #[cfg(test)]
    pub(crate) fn restoration_boundary(&self) -> Option<usize> {
        let mut boundary = None;
        for (index, segment) in self.segments.iter().enumerate() {
            match &segment.entry {
                Some(Entry::Spatial(e)) if e.stage == crate::EffectStage::Restoration => {
                    boundary = Some(index)
                }
                Some(_) => break,
                None => {}
            }
            if segment.has_color {
                break;
            }
        }
        boundary
    }

    /// Whether answering one pixel of this compilation reads through a spatial segment, whose whole
    /// frame the reference renderer materializes to answer it: a pixel of such a stack is never
    /// `O(layers)`. The coverage overlay reads this to refuse rather than to pay it.
    /// `O(segments)` and reads no pixels.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.spatial_before(self.segments.len())
    }

    /// Whether a segment entered through a spatial operation ([`Entry::spatial_operation`]) comes
    /// before segment `index`, so that the stage `index` reads comes through that operation's
    /// whole frame rather than from the source alone.
    pub(crate) fn spatial_before(&self, index: usize) -> bool {
        self.segments[..index].iter().any(|segment| {
            segment
                .entry
                .as_ref()
                .is_some_and(|entry| entry.spatial_operation().is_some())
        })
    }
}

/// Where one segment-output pixel comes from: the input-frame pixel it reads and the replacement
/// that wins there, with that replacement's position in the operation list. The position decides
/// which colour runs still reach the pixel: a replacement overwrites everything the runs before it
/// produced, and only the runs after it process the replaced value.
pub(super) struct Resolved {
    pub(super) replacement: Option<(usize, [u8; 3])>,
    pub(super) input_x: u32,
    pub(super) input_y: u32,
}

impl Segment {
    /// Where output pixel `(x, y)` comes from, or `None` outside the output stage. A segment
    /// without point replacements has none to find, so it answers straight from the unmap; one
    /// with them walks its operations backwards, composing the geometry each replacement is
    /// carried through, as [`mapped_replacements`] does.
    pub(super) fn resolve(&self, x: u32, y: u32) -> Option<Resolved> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (input_x, input_y) = self.geometry.unmap(x, y);
        if !self.has_pixels {
            return Some(Resolved {
                replacement: None,
                input_x,
                input_y,
            });
        }
        self.replacement_at(x, y, input_x, input_y)
    }

    /// The walk [`Self::resolve`] takes for a segment with replacements: every operation's
    /// geometry composed backwards from the output, and the last replacement that lands on
    /// `(x, y)`, if any.
    fn replacement_at(&self, x: u32, y: u32, input_x: u32, input_y: u32) -> Option<Resolved> {
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
                Processing::Color(_)
                | Processing::Spatial(_)
                | Processing::Resample(_)
                | Processing::Warp(_) => {}
            }
        }
        Some(Resolved {
            replacement: None,
            input_x,
            input_y,
        })
    }

    /// [`Self::resolve`] by the full composition whatever the segment holds: the reference the
    /// shortcut for a segment without replacements is held to.
    #[cfg(test)]
    pub(super) fn resolve_composed(&self, x: u32, y: u32) -> Option<Resolved> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (input_x, input_y) = self.geometry.unmap(x, y);
        self.replacement_at(x, y, input_x, input_y)
    }
}

/// Where each of one segment's point replacements lands in its output frame, in stack order, with
/// its position in the operation list. One backward walk accumulates the suffix geometry that
/// carries each replacement; a replacement a later crop discards is simply absent. The result is
/// bounded by the layer count and reads no pixels.
pub(super) fn mapped_replacements(segment: &Segment) -> Vec<(usize, u32, u32, [u8; 3])> {
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
            Processing::Color(_)
            | Processing::Spatial(_)
            | Processing::Resample(_)
            | Processing::Warp(_) => {}
        }
    }
    mapped.reverse();
    mapped
}
