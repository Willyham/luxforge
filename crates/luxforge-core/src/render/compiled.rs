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
/// linear rows load it, and whether a point query evaluates it in tiles. A new kind of boundary is one more variant
/// with one more arm in each of these methods; no caller matches on the kind.
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
    pub(crate) fn spatial(operation: SpatialOperation, prefix_hash: String) -> Self {
        Self::Spatial(SpatialEntry::new(operation, prefix_hash))
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
    /// rectangle of its output stage, needs. `tiled_before` says whether a point query reaches the
    /// stage it reads through an earlier boundary's tiles.
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
            Self::Spatial(entry) => {
                // A window cannot hold a whole-stage reduction, and a stage behind another spatial
                // operation cannot be handed one without that operation's whole output.
                if entry.prepares_estimates() && tiled_before {
                    return Err(RegionFallback::EstimateAfterSpatial);
                }
                Ok(entry.reads(read, received))
            }
        }
    }

    /// Cut this boundary for a windowed proxy ([`super::window::WindowPlan::apply`]): `read` is the
    /// rectangle of its whole output stage `whole` that its segment reads, and `previous` the
    /// rectangle of the whole stage it receives that the cut frame before it holds. Answers the
    /// rectangle of `whole` its own cut frame holds. `globals` answers the estimates it is handed
    /// when it prepares one; it is asked nothing otherwise.
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

    /// `forward`, the map from the content stage to the frame this boundary reads, followed by
    /// this boundary's own forward map to its frame. A resample declares the map from its output
    /// back to its input, the direction a sampler reads, so its forward map is that inverted; a
    /// spatial operation moves no coordinate and contributes nothing.
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
    /// the evaluation's frame of it or else from the query's tiles.
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
            Self::Spatial(entry) => evaluation.spatial_entry_pixel(index, entry, x, y),
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
    pub(super) fn byte_frame(
        &self,
        domain: &Byte<'_>,
        input: &[u8],
        received: Stage,
        tiling: Tiling,
        cancel: &Cancel,
        context: &RenderContext,
    ) -> Result<Arc<Vec<u8>>, Error> {
        match self {
            Self::Resample(entry) => {
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
                Ok(frame)
            }
            Self::Spatial(entry) => {
                super::byte::spatial_frame(domain, entry, input, received, tiling, cancel, context)
            }
        }
    }

    /// The bytes one worker of the linear rows reserves to load this boundary: a resample's block
    /// of taps, and nothing for a boundary whose pixels are pulled one at a time.
    pub(super) fn linear_scratch(&self) -> usize {
        match self {
            Self::Resample(_) => TAP_BLOCK_PIXELS as usize * std::mem::size_of::<[f64; 3]>(),
            Self::Spatial(_) => 0,
        }
    }

    /// Load one chunk of the linear rows of a segment that enters through this boundary: a
    /// resample's taps block by block ([`LinearRows::load_resampled`]), or each pixel pulled
    /// through [`Self::pixel`] ([`LinearRows::load_pulled`]).
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

    /// The spatial operation a point query evaluates this boundary through, tile by tile, in its
    /// [`super::spatial::PointTiles`]; `None` for a boundary a point query pulls without tiles.
    pub(crate) fn point_tiles(&self) -> Option<&SpatialOperation> {
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
}

impl Compiled {
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
            spatial: self.evaluates_spatial(),
            mask: self.supersampled_masks(),
            reduced_detail: false,
        }
    }

    /// Whether answering one pixel of this compilation evaluates a spatial segment.
    ///
    /// A spatial point query is the declared exception to [performance rule
    /// 4](../../../../docs/engineering/performance-rules.md#rules): it evaluates the stage-aligned tiles
    /// its pixels need, each once per query, so a caller that asks per display cell over the whole
    /// stage evaluates every tile of it. The coverage overlay reads this to refuse rather than to
    /// pay it. `O(segments)` and reads no pixels.
    pub(crate) fn evaluates_spatial(&self) -> bool {
        self.spatial_before(self.segments.len())
    }

    /// Whether a segment entered through tiles ([`Entry::point_tiles`]) comes before segment
    /// `index`, so that, in a point query, the stage `index` reads comes through
    /// [`super::spatial::PointTiles`] rather than from the source alone.
    pub(crate) fn spatial_before(&self, index: usize) -> bool {
        self.segments[..index].iter().any(|segment| {
            segment
                .entry
                .as_ref()
                .is_some_and(|entry| entry.point_tiles().is_some())
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
    pub(super) fn resolve(&self, x: u32, y: u32) -> Option<Resolved> {
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
