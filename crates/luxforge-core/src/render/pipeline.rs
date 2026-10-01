//! The one pipeline, generic over its pixel domain.
//!
//! A recipe compiles into segments separated by stage boundaries (a resample or a spatial
//! operation), and every segment is its exact geometry, point replacements and colour runs. What a
//! pixel *is* between those steps is the only thing that differs between a JPEG and a developed
//! RAW, and [`PixelDomain`] is where that difference lives:
//!
//! - **Byte** ([`super::Byte`]): opaque 8-bit sRGB. Each colour run decodes through the sRGB
//!   table, runs its units and quantizes at the run's end, so a point replacement, a resample and
//!   the end of the recipe are quantization boundaries, as is a spatial operation's output.
//! - **Linear** ([`super::linear::Linear`]): signed unbounded `f64` linear sRGB, with the RAW
//!   exposure and approximate white balance applied to each source pixel. A colour segment is `f32`
//!   from its entry to its end, nothing is quantized before the terminal boundary, and there is no
//!   alpha.
//!
//! Everything else exists once, here: walking a point back through the segments ([`Evaluation`],
//! which asks each segment's boundary through [`super::Entry`]), the resample recursion, a spatial
//! entry answered from the query's tiles or from a materialized frame, the global estimates a
//! spatial entry reads and its output built tile by tile ([`SpatialEntry`], [`spatial_entry`],
//! which both drivers materialize a spatial operation through), the replacements and colour runs
//! applied to rows of a segment's output ([`segment_pass`]) and the terminal conversion a sample
//! and a grid share. The rectangle a resample reads is [`crate::modules::Resample::reads`], beside
//! the resample's own mapping.
//!
//! What stays with each domain's rasterizer is which frames it materializes. The byte driver
//! ([`super::rasterize`]) writes every segment's output as a byte frame, because a byte frame is
//! exactly what the next boundary reads. The linear driver ([`super::linear::rasterize`]) writes
//! only the last segment's rows and pulls everything before them through [`Evaluation`], because a
//! linear value between two boundaries is an `f64` the next resample blends: materializing it as
//! `f32` would change bytes and as `f64` would double the RAW planar bound. It pulls a bounded
//! rectangle at a time ([`Evaluation::region_in`]) — a spatial operation's input one row at a
//! time, a resample's taps one block at a time — so the colour before a boundary still runs over
//! rows. A spatial operation's output is `f32` on both paths, so the linear driver materializes
//! those, as a render always has.

use super::{
    ColorRun, Compiled, RenderContext, ResampleEntry, ScratchBudget, Segment, color_chunk_rows,
    color_runs, mapped_replacements,
    spatial::{
        PointTiles, SpatialPlan, Tiling, build_reduction_cancellable, fill_planes, resolve_globals,
        run_batches, run_tile,
    },
};

/// The input received by a colour operation inside its final float run. Replacement boundaries
/// retain normal domain quantization; the final run retains its unclamped working values.
pub(super) fn colour_input<'r, D: PixelDomain>(
    mut pixel: D::Pixel,
    runs: impl Iterator<Item = ColorRun<'r>>,
    x: u32,
    y: u32,
    wide: bool,
) -> Result<[f64; 3], Error> {
    let mut linear = [D::spatial_input(pixel)];
    let mut snapshot = [[0.0; 3]; 1];
    for run in runs {
        if run.followed_by_replace() {
            pixel = D::colour(pixel, std::iter::once(run), x, y, wide)?;
            linear[0] = D::spatial_input(pixel);
        } else {
            super::apply_units(&run, y, x, &mut linear, &mut snapshot)?;
        }
    }
    Ok(linear[0].map(f64::from))
}
use crate::{
    Cancel, Error,
    modules::{Global, Parallelism, Reduction, Region, SpatialOperation, Stage},
};
use rayon::prelude::*;
#[cfg(test)]
use std::sync::Weak;
use std::{borrow::Cow, ops::Range, sync::Arc};

/// What separates one pixel domain from the other. Every method is the one place a difference
/// between the byte and the linear evaluation is written; see the [module](self) documentation.
pub(crate) trait PixelDomain: Sync {
    /// One pixel between two steps of a segment.
    type Pixel: Copy + Send + Sync;
    /// One spatial operation's output over its whole stage.
    type SpatialFrame: Send + Sync;
    /// One evaluated tile, as the parallel half of [`spatial_entry`] hands it to the serial write:
    /// rows already in the frame's own layout, so the write copies whole rows.
    type TileOutput: Send;

    /// The fingerprint a global estimate is keyed by.
    fn fingerprint(&self) -> &str;

    /// What identifies a spatial operation's input beyond the source fingerprint and the layers
    /// before it. The byte path's input is the recipe prefix over the decoded source, so it is
    /// the prefix hash. The linear path's also depends on the development, the view and the
    /// settings, none of which the recipe names.
    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str>;

    /// Refuse an output stage this domain cannot produce, before a point is answered in it.
    /// Nothing is refused unless a domain says so; the linear path has its own output limit.
    fn check_output(&self, _width: u32, _height: u32) -> Result<(), Error> {
        Ok(())
    }

    /// One pixel of the source, which is the first segment's input.
    fn source_pixel(&self, x: u32, y: u32) -> Result<Self::Pixel, Error>;

    /// A point replacement's value written over `pixel`.
    fn replace(pixel: Self::Pixel, rgb: [u8; 3]) -> Self::Pixel;

    /// Every colour run of `runs`, in order, over one pixel at `(x, y)` of its segment's output
    /// stage: the per-pixel form of [`SegmentRows::run`], with the same arithmetic.
    fn colour<'r>(
        pixel: Self::Pixel,
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
        wide: bool,
    ) -> Result<Self::Pixel, Error>;

    /// The pixel a segment answers, once its replacement and colour are applied: the pixel itself
    /// unless a domain checks it; the linear path refuses a non-finite value here.
    fn finish(pixel: Self::Pixel) -> Result<Self::Pixel, Error> {
        Ok(pixel)
    }

    fn narrow(pixel: Self::Pixel) -> Self::Pixel {
        pixel
    }

    fn finish_width(pixel: Self::Pixel, _: bool) -> Result<Self::Pixel, Error> {
        Self::finish(pixel)
    }

    /// [`Self::colour`] and [`Self::finish`] over one contiguous run of row `y` starting at column
    /// `x0`, with the same arithmetic. Units are pointwise and are handed a row with its
    /// coordinates, so each domain runs them over the whole row at once, as the rows of a rendered
    /// segment do; `scratch` is the worker's reused float row.
    fn colour_row<'r>(
        pixels: &mut [Self::Pixel],
        runs: impl Iterator<Item = ColorRun<'r>>,
        y: u32,
        x0: u32,
        scratch: &mut RowScratch,
        wide: bool,
    ) -> Result<(), Error>;

    /// One resample tap set: the bilinear blend at the continuous input coordinate `(u, v)` of a
    /// `width` × `height` stage, whose pixels `fetch` reads.
    fn blend(
        u: f64,
        v: f64,
        width: u32,
        height: u32,
        fetch: impl FnMut(u32, u32) -> Result<Self::Pixel, Error>,
    ) -> Result<Self::Pixel, Error>;

    /// A pixel as a spatial operation reads it, in linear `f32`.
    fn spatial_input(pixel: Self::Pixel) -> [f32; 3];

    /// A pixel in linear light, as a mask's value-based component reads the input of the layer it
    /// modulates.
    fn linear(pixel: Self::Pixel) -> [f64; 3];

    /// A spatial operation's output value as a pixel.
    fn spatial_output(rgb: [f32; 3], wide: bool) -> Result<Self::Pixel, Error>;

    /// The terminal byte of one output pixel.
    fn terminal(pixel: Self::Pixel) -> Result<[u8; 4], Error>;

    /// An empty spatial frame of `stage`, inside the domain's frame limit.
    fn spatial_frame(stage: Stage, wide: bool) -> Result<Self::SpatialFrame, Error>;

    /// One tile's output, from the rectangle `region` its last unit wrote, with each of the tile's
    /// rows in the layout the frame holds it in, computed in the parallel phase.
    fn tile_output(
        region: Region,
        values: Vec<f32>,
        tile: Region,
        parallelism: Parallelism,
        wide: bool,
    ) -> Self::TileOutput;

    /// Place one tile's output into the frame of `stage`, one `copy_from_slice` per row and plane.
    fn write_tile(
        frame: &mut Self::SpatialFrame,
        stage: Stage,
        tile: Region,
        output: Self::TileOutput,
    );

    /// One pixel of a spatial frame of `stage`.
    fn frame_pixel(frame: &Self::SpatialFrame, stage: Stage, x: u32, y: u32) -> Self::Pixel;
}

/// Identify the pixels of a recipe prefix in this domain. This only builds metadata: it neither
/// reads pixels nor prepares an estimate. Global estimates and retained restoration frames use
/// the same source development, view and approximation identity through this one key builder.
pub(super) fn input_prefix_key<'p, D: PixelDomain>(
    domain: &D,
    prefix_hash: &'p str,
) -> Cow<'p, str> {
    domain.estimate_prefix(prefix_hash)
}

/// How an [`Evaluation`] answers the pixels of its spatial segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpatialMode {
    /// Materialize every spatial operation's output once over its whole stage, for a pass that
    /// reads every pixel.
    Frames,
    /// Evaluate every spatial segment only in the tiles the requested pixels need, through one
    /// [`PointTiles`] cache, for a point query, which is the declared exception to performance
    /// rule 4. Nothing is materialized: a later segment's halo reads an earlier segment's tiles
    /// from the same cache, and so does a reduction of a stage behind a spatial segment.
    Point,
}

/// The output of the latest spatial segment an evaluation has materialized, with the index of the
/// segment it enters.
pub(super) struct SpatialFrame<F> {
    pub(super) index: usize,
    pub(super) planes: Arc<F>,
}

/// One compiled recipe bound to its source in one domain: the stage it produces and point queries
/// that never allocate a frame. Compiling once serves any number of sampled pixels.
pub(crate) struct Evaluation<'a, D: PixelDomain> {
    pub(super) domain: D,
    pub(super) compiled: Cow<'a, Compiled>,
    widths: Vec<super::byte::FrameWidths>,
    /// The latest spatial frame, in [`SpatialMode::Frames`]. A pull stops at the first spatial
    /// entry it meets walking back — a spatial entry reads its own frame and only a resample reads
    /// the segment before it — so once a spatial segment's frame exists nothing reads an earlier
    /// one. Building a frame therefore holds at most two, and the evaluation keeps one.
    pub(super) frame: Option<SpatialFrame<D::SpatialFrame>>,
    /// Every frame this evaluation built, in order, with how many of them were alive when it was
    /// finished and before the one it replaces was released, which is the peak.
    #[cfg(test)]
    pub(super) built: Vec<(Weak<D::SpatialFrame>, usize)>,
    /// In [`SpatialMode::Point`], the tiles of every spatial segment this query has evaluated.
    pub(super) tiles: Option<PointTiles<'a>>,
    /// How each spatial segment is cut into tiles: [`Tiling::Halo`] everywhere but in the tests
    /// that prove the result does not depend on it.
    tiling: Tiling,
    cancel: Cancel,
    context: &'a RenderContext,
}

impl<'a, D: PixelDomain> Evaluation<'a, D> {
    /// An evaluation of a stack compiled against the domain's source dimensions. In
    /// [`SpatialMode::Frames`] every spatial operation's output is materialized here, in stage
    /// order, under `cancel`; in [`SpatialMode::Point`] nothing is, and a spatial segment's pixels
    /// are evaluated one tile at a time as they are asked for.
    pub(crate) fn new(
        domain: D,
        compiled: Cow<'a, Compiled>,
        tiling: Tiling,
        mode: SpatialMode,
        cancel: &Cancel,
        context: &'a RenderContext,
    ) -> Result<Self, Error> {
        let mut evaluation = Self {
            domain,
            widths: super::byte::byte_frame_widths(&compiled),
            compiled,
            frame: None,
            #[cfg(test)]
            built: Vec::new(),
            tiles: (mode == SpatialMode::Point).then(|| PointTiles::new(tiling, context.spatial())),
            tiling,
            cancel: cancel.clone(),
            context,
        };
        if mode == SpatialMode::Point {
            return Ok(evaluation);
        }
        // In order, because a later spatial operation pulls its input through the earlier one, and
        // each frame replaces the one before it once it exists.
        for index in 0..evaluation.compiled.segments.len() {
            let Some(entry) = &evaluation.compiled.segments[index].entry else {
                continue;
            };
            let Some(frame) = entry.frame(&evaluation, index, cancel)? else {
                continue;
            };
            let planes = Arc::new(frame);
            #[cfg(test)]
            {
                // This frame and every earlier one still alive.
                let alive = 1 + evaluation
                    .built
                    .iter()
                    .filter(|(frame, _)| frame.strong_count() > 0)
                    .count();
                evaluation.built.push((Arc::downgrade(&planes), alive));
            }
            // Releases the frame before it: every later pull stops at this one.
            evaluation.frame = Some(SpatialFrame { index, planes });
        }
        Ok(evaluation)
    }

    pub(super) fn frames_prefix(
        domain: D,
        compiled: Cow<'a, Compiled>,
        tiling: Tiling,
        cancel: &Cancel,
        context: &'a RenderContext,
        boundary: usize,
    ) -> Result<Self, Error> {
        let mut evaluation = Self::new(
            domain,
            compiled,
            tiling,
            SpatialMode::Point,
            cancel,
            context,
        )?;
        evaluation.tiles = None;
        evaluation.materialize(0, boundary + 1, cancel)?;
        Ok(evaluation)
    }

    pub(super) fn frames_suffix(
        domain: D,
        compiled: Cow<'a, Compiled>,
        tiling: Tiling,
        cancel: &Cancel,
        context: &'a RenderContext,
        boundary: usize,
        planes: Arc<D::SpatialFrame>,
    ) -> Result<Self, Error> {
        let mut evaluation = Self::new(
            domain,
            compiled,
            tiling,
            SpatialMode::Point,
            cancel,
            context,
        )?;
        evaluation.tiles = None;
        evaluation.frame = Some(SpatialFrame {
            index: boundary,
            planes,
        });
        evaluation.materialize(boundary + 1, evaluation.compiled.segments.len(), cancel)?;
        Ok(evaluation)
    }

    fn materialize(&mut self, start: usize, end: usize, cancel: &Cancel) -> Result<(), Error> {
        for index in start..end {
            let Some(entry) = &self.compiled.segments[index].entry else {
                continue;
            };
            let Some(frame) = entry.frame(self, index, cancel)? else {
                continue;
            };
            self.frame = Some(SpatialFrame {
                index,
                planes: Arc::new(frame),
            });
        }
        Ok(())
    }

    /// Read an operation's prefix with the spatial boundary width selected by the whole recipe.
    /// A truncated prefix can otherwise choose a different width from a later point replacement
    /// or colour run. Only point evaluations use this mode.
    pub(crate) fn with_input_width(mut self, wide: bool) -> Self {
        debug_assert!(self.frame.is_none());
        let last = self.widths.len() - 1;
        let Some(target) = self
            .compiled
            .segments
            .iter()
            .rposition(|segment| matches!(&segment.entry, Some(super::Entry::Spatial(_))))
        else {
            return self;
        };
        for index in (0..=last).rev() {
            let segment = &self.compiled.segments[index];
            let spatial = matches!(&segment.entry, Some(super::Entry::Spatial(_)));
            let next_spatial = self
                .compiled
                .segments
                .get(index + 1)
                .is_some_and(|s| matches!(&s.entry, Some(super::Entry::Spatial(_))));
            let output = (index == last && target == last && wide && !segment.has_pixels)
                || (next_spatial && (segment.has_color || self.widths[index + 1].input));
            let input = spatial
                && (if index == target {
                    wide
                } else {
                    (segment.has_color && !segment.has_pixels)
                        || (!segment.writes_pixels() && next_spatial)
                });
            self.widths[index] = super::byte::FrameWidths { input, output };
        }
        self
    }

    #[cfg(test)]
    /// The same point evaluation with another spatial tile size. A spatial unit's value at a
    /// pixel depends on that pixel's neighbourhood only, so this changes nothing but the schedule.
    pub(crate) fn with_tile(mut self, tile: u32) -> Self {
        self.tiling = Tiling::Fixed(tile);
        self.tiles = Some(PointTiles::new(self.tiling, self.context.spatial()));
        self
    }

    /// The tiles this point evaluation has evaluated so far.
    #[cfg(test)]
    pub(crate) fn point_tiles(&self) -> &PointTiles<'a> {
        self.tiles.as_ref().expect("a point evaluation holds tiles")
    }

    /// The output stage.
    pub(crate) fn stage(&self) -> Stage {
        self.compiled.stage()
    }

    /// The global estimates the boundary entering segment `index` reads
    /// ([`super::Entry::globals`]), from the store or from one reduction of its input stage,
    /// exactly as a frame of this compilation would resolve them; the store then holds them for
    /// that frame. Empty when segment `index` enters through no boundary that reads one.
    pub(crate) fn globals_of(&self, index: usize) -> Result<Vec<Option<Global>>, Error> {
        match &self.compiled.segments[index].entry {
            Some(entry) => entry.globals(self, index),
            None => Ok(Vec::new()),
        }
    }

    /// The final spatial prefix on one output rectangle, through the render's own tile function.
    /// Input grids use a whole tile for dense cells and a one-pixel window for sparse cells.
    pub(crate) fn restoration_region(&self, region: Region) -> Result<Vec<D::Pixel>, Error> {
        let index = self.compiled.segments.len() - 1;
        let segment = &self.compiled.segments[index];
        let Some(super::Entry::Spatial(entry)) = &segment.entry else {
            return Err(Error::internal(
                "an input grid prefix must end at a restoration boundary",
            ));
        };
        if segment.writes_pixels() {
            return Err(Error::internal("an input grid prefix has a pointwise tail"));
        }
        self.cancel.check()?;
        let stage = segment.stage();
        let plan = SpatialPlan::new(&entry.operation, stage, self.tiling)?;
        let globals = self.spatial_globals(index, entry)?;
        let _reservation = self.context.spatial().reserve(plan.working_set(), 1);
        let parallelism = if super::parallel::pooled(
            super::parallel::RenderPass::Spatial,
            stage.width as u64 * stage.height as u64,
        ) {
            crate::modules::Parallelism::Pool
        } else {
            crate::modules::Parallelism::Serial
        };
        let (written, values) = run_tile(
            &plan,
            &entry.operation,
            &globals,
            region,
            parallelism,
            &mut super::spatial::TileScratch::default(),
            &self.cancel,
            |input, planes| self.fill_rows(index - 1, input, planes, parallelism),
        )?;
        let count = written.pixels() as usize;
        let mut output = Vec::with_capacity(region.pixels() as usize);
        for y in region.y0..region.y1() {
            self.cancel.check()?;
            for x in region.x0..region.x1() {
                let from =
                    (y - written.y0) as usize * written.width as usize + (x - written.x0) as usize;
                output.push(D::spatial_output(
                    [values[from], values[count + from], values[2 * count + from]],
                    self.widths[index].input,
                )?);
            }
        }
        Ok(output)
    }

    /// One output pixel, `None` when the coordinate lies outside the output stage.
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Result<Option<D::Pixel>, Error> {
        self.pixel_in(self.compiled.segments.len() - 1, x, y)
    }

    /// One output pixel's terminal byte, `None` outside the output stage.
    pub(crate) fn terminal(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.pixel(x, y)?.map(D::terminal).transpose()
    }

    pub(super) fn colour_input(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        let index = self.compiled.segments.len() - 1;
        let segment = &self.compiled.segments[index];
        let Some(resolved) = segment.resolve(x, y) else {
            return Ok(None);
        };
        let mut pixel = self.entry_pixel(index, resolved.input_x, resolved.input_y)?;
        if let Some((_, rgb)) = resolved.replacement {
            pixel = D::replace(pixel, rgb);
        }
        let after = resolved.replacement.map_or(0, |(index, _)| index + 1);
        colour_input::<D>(
            pixel,
            color_runs(segment).filter(|run| run.start >= after),
            x,
            y,
            self.widths[index].output,
        )
        .map(Some)
    }

    /// One pixel of one segment's output stage. A resample is evaluated recursively as the bilinear
    /// blend of four pixels of the previous segment, so a point query costs `O(layers · 4^resamples)`
    /// and never allocates a frame; a stack holds at most one crop layer.
    ///
    /// A spatial entry is the one exception to "a point query never rasterizes", declared in the
    /// [performance rules](../../../../docs/engineering/performance-rules.md): see
    /// [`Self::spatial_pixel`].
    ///
    /// The colour phases are the rasterizing pass's, applied to this one pixel: the replacement that
    /// wins here ends the runs before it, and every run after it is applied in turn, so the sampled
    /// byte is the byte the frame holds.
    pub(super) fn pixel_in(&self, index: usize, x: u32, y: u32) -> Result<Option<D::Pixel>, Error> {
        let segment = &self.compiled.segments[index];
        let Some(resolved) = segment.resolve(x, y) else {
            return Ok(None);
        };
        let mut pixel = self.entry_pixel(index, resolved.input_x, resolved.input_y)?;
        if let Some((_, rgb)) = resolved.replacement {
            pixel = D::replace(pixel, rgb);
        }
        if segment.has_color {
            let after = resolved.replacement.map_or(0, |(index, _)| index + 1);
            pixel = D::colour(
                pixel,
                color_runs(segment).filter(|run| run.start >= after),
                x,
                y,
                self.widths[index].output,
            )?;
        }
        D::finish_width(pixel, self.widths[index].output).map(Some)
    }

    /// One pixel of segment `index`'s input frame, which its exact geometry reads: the source, or
    /// what the boundary entering it answers there ([`super::Entry::pixel`]).
    #[inline(always)]
    pub(super) fn entry_pixel(&self, index: usize, x: u32, y: u32) -> Result<D::Pixel, Error> {
        match &self.compiled.segments[index].entry {
            None => self.domain.source_pixel(x, y),
            Some(entry) => entry.pixel(self, index, x, y),
        }
    }

    /// One pixel of the input frame of segment `index`, which enters through the resample
    /// `entry`: the bilinear blend of four pixels of the segment before it.
    #[inline(always)]
    pub(super) fn resample_pixel(
        &self,
        index: usize,
        entry: &ResampleEntry,
        x: u32,
        y: u32,
    ) -> Result<D::Pixel, Error> {
        let previous = &self.compiled.segments[index - 1];
        let (u, v) = entry.input_at(x, y);
        D::blend(u, v, previous.width, previous.height, |x, y| {
            self.pixel_in(index - 1, x, y)?
                .ok_or_else(|| Error::render("a resample tap was outside its stage"))
        })
    }

    /// One pixel of the input frame of segment `index`, which enters through the spatial `entry`:
    /// from this evaluation's frame of it, or else from the query's tiles
    /// ([`Self::spatial_pixel`]).
    #[inline(always)]
    pub(super) fn spatial_entry_pixel(
        &self,
        index: usize,
        entry: &SpatialEntry,
        x: u32,
        y: u32,
    ) -> Result<D::Pixel, Error> {
        match &self.frame {
            Some(frame) if frame.index == index => Ok(D::frame_pixel(
                &frame.planes,
                self.spatial_stage(index),
                x,
                y,
            )),
            _ => self.spatial_pixel(index, entry, x, y),
        }
    }

    /// The output of the spatial `entry` of segment `index` over its whole stage, its stage read
    /// by rows ([`Self::fill_rows`]): the frame [`SpatialMode::Frames`] materializes.
    pub(super) fn spatial_frame(
        &self,
        index: usize,
        entry: &SpatialEntry,
        cancel: &Cancel,
    ) -> Result<D::SpatialFrame, Error> {
        spatial_entry(
            &self.domain,
            entry,
            self.spatial_stage(index),
            self.tiling,
            cancel,
            self.context,
            self.widths[index].input,
            |region, planes, parallelism| self.fill_rows(index - 1, region, planes, parallelism),
        )
    }

    /// The stage spatial segment `index` reads, which is the stage it writes.
    fn spatial_stage(&self, index: usize) -> Stage {
        self.compiled.segments[index - 1].stage()
    }

    /// Segment `index`'s output over `region`, row-major, into `out`: exactly the values
    /// [`Self::pixel_in`] answers there, for a caller that reads a neighbourhood of them. A
    /// segment with colour and no replacement pulls each row's entry through its geometry and
    /// runs its colour over the row at once ([`PixelDomain::colour_row`]), which is the same
    /// arithmetic as one pixel at a time; any other segment is pulled pixel by pixel. `region`
    /// must lie inside the segment's output stage.
    pub(super) fn region_in(
        &self,
        index: usize,
        region: Region,
        out: &mut Vec<D::Pixel>,
        scratch: &mut RowScratch,
    ) -> Result<(), Error> {
        out.clear();
        let segment = &self.compiled.segments[index];
        let batched = segment.has_color && !segment.has_pixels;
        let outside = || Error::render("a region read was outside its stage");
        for y in region.y0..region.y1() {
            let start = out.len();
            for x in region.x0..region.x1() {
                if batched {
                    let (input_x, input_y) = segment.geometry.unmap(x, y);
                    out.push(self.entry_pixel(index, input_x, input_y)?);
                } else {
                    out.push(self.pixel_in(index, x, y)?.ok_or_else(outside)?);
                }
            }
            if batched {
                D::colour_row(
                    &mut out[start..],
                    color_runs(segment),
                    y,
                    region.x0,
                    scratch,
                    self.widths[index].output,
                )?;
            }
        }
        Ok(())
    }

    /// Segment `index`'s output over `region`, as the three planes a spatial operation reads: one
    /// [`Self::region_in`] per row, on the pool under [`Parallelism::Pool`], for a segment whose
    /// colour runs over rows. Any other segment is read pixel by pixel straight into the planes,
    /// since a row buffer would only copy what a pull already answers.
    ///
    /// This is how the stage a spatial entry reads is read everywhere: a tile's input, in a frame
    /// and in a point query, and the reduction its global estimates are prepared from.
    fn fill_rows(
        &self,
        index: usize,
        region: Region,
        planes: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        let segment = &self.compiled.segments[index];
        if !segment.has_color || segment.has_pixels {
            return fill_planes(region, planes, parallelism, |x, y| {
                let pixel = self
                    .pixel_in(index, x, y)?
                    .ok_or_else(|| Error::render("a spatial read was outside its input stage"))?;
                Ok(D::spatial_input(pixel))
            });
        }
        if region.is_empty() {
            return Ok(());
        }
        let len = region.pixels() as usize;
        let width = region.width as usize;
        let (red, rest) = planes[..3 * len].split_at_mut(len);
        let (green, blue) = rest.split_at_mut(len);
        let row = |scratch: &mut (Vec<D::Pixel>, RowScratch),
                   (row, ((red, green), blue)): PlaneRow<'_>|
         -> Result<(), Error> {
            let (pixels, scratch) = scratch;
            let line = Region {
                x0: region.x0,
                y0: region.y0 + row as u32,
                width: region.width,
                height: 1,
            };
            self.region_in(index, line, pixels, scratch)?;
            for (column, pixel) in pixels.iter().enumerate() {
                let [r, g, b] = D::spatial_input(*pixel);
                red[column] = r;
                green[column] = g;
                blue[column] = b;
            }
            Ok(())
        };
        match parallelism {
            Parallelism::Pool => red
                .par_chunks_mut(width)
                .zip(green.par_chunks_mut(width))
                .zip(blue.par_chunks_mut(width))
                .enumerate()
                .try_for_each_init(Default::default, row),
            Parallelism::Serial => {
                let mut scratch = Default::default();
                red.chunks_mut(width)
                    .zip(green.chunks_mut(width))
                    .zip(blue.chunks_mut(width))
                    .enumerate()
                    .try_for_each(|item| row(&mut scratch, item))
            }
        }
    }

    /// The global estimates of spatial segment `index`, whose entry is `entry`
    /// ([`SpatialEntry::globals`]), its stage read by rows ([`Self::fill_rows`]). A point query's
    /// reduction reads through [`PointTiles::reduce`]; a frame's, which [`spatial_entry`] resolves
    /// itself, reads the stage as a render does.
    pub(super) fn spatial_globals(
        &self,
        index: usize,
        entry: &SpatialEntry,
    ) -> Result<Vec<Option<Global>>, Error> {
        let stage = self.spatial_stage(index);
        let fill = |region: Region, planes: &mut [f32]| {
            self.fill_rows(index - 1, region, planes, Parallelism::Serial)
        };
        entry.globals(&self.domain, self.context, stage, || match &self.tiles {
            Some(tiles) => {
                // The tile side of the nearest spatial segment before this one, whose tiles the
                // stage is read through.
                let through = (0..index).rev().find_map(|earlier| {
                    let operation = self.compiled.segments[earlier]
                        .entry
                        .as_ref()?
                        .point_tiles()?;
                    Some(self.tiling.tile(operation, self.spatial_stage(earlier)))
                });
                tiles.reduce(stage, through, &self.cancel, fill)
            }
            None => build_reduction_cancellable(stage, &self.cancel, fill),
        })
    }

    /// One pixel of the frame a spatial entry produces, without its frame.
    ///
    /// The value at a pixel depends on a bounded neighbourhood of it, so there is no way to answer
    /// this in `O(layers)`: the sample evaluates the stage-aligned tile that contains the pixel,
    /// reading that tile plus the operation's summed halo through the compiled prefix, with exactly
    /// the tile function the render uses, and holds it in this evaluation's [`PointTiles`]. The
    /// sampled value is therefore the value a render of that tile produces, by construction rather
    /// than by agreement. When the prefix holds an earlier spatial segment, the halo reads that
    /// segment's tiles from the same cache, so each (segment, tile) is evaluated once per query.
    /// Its cost is `O((tile + halo)² × layers)` per evaluated tile, plus one bounded reduction of
    /// the stage when a unit's global estimate is not already stored, and it allocates one tile
    /// working set at a time from the spatial budget and no frame. This is the declared exception to
    /// performance rule 4.
    fn spatial_pixel(
        &self,
        index: usize,
        entry: &SpatialEntry,
        x: u32,
        y: u32,
    ) -> Result<D::Pixel, Error> {
        let tiles = self
            .tiles
            .as_ref()
            .expect("a pull meets only the latest frame or a point query's tiles");
        let rgb = tiles.pixel(
            index,
            &entry.operation,
            self.spatial_stage(index),
            x,
            y,
            &self.cancel,
            || self.spatial_globals(index, entry),
            |region, planes| self.fill_rows(index - 1, region, planes, Parallelism::Serial),
        )?;
        D::spatial_output(rgb, self.widths[index].input)
    }
}

/// One row of three planes being filled, with its index in the region.
type PlaneRow<'p> = (usize, ((&'p mut [f32], &'p mut [f32]), &'p mut [f32]));

/// The float rows a domain runs one row's colour through, reused by every row one worker takes.
#[derive(Default)]
pub(crate) struct RowScratch {
    /// The row itself, in `f32`.
    pub(super) linear: Vec<[f32; 3]>,
    /// One row of a masked operation's own input.
    pub(super) snapshot: Vec<[f32; 3]>,
}

/// The four pixels a bilinear sample at one continuous input coordinate reads, with indices clamped
/// to the stage's edge, and their weights. Both domains blend these taps.
pub(super) struct Taps {
    /// Top-left, top-right, bottom-left, bottom-right.
    pub(super) corners: [(u32, u32); 4],
    pub(super) weights: [f64; 4],
}

impl Taps {
    #[inline]
    pub(super) fn new(u: f64, v: f64, width: u32, height: u32) -> Self {
        // The mapped coordinate is a pixel center, so index space starts half a pixel earlier.
        let x = u - 0.5;
        let y = v - 0.5;
        let left = x.floor();
        let top = y.floor();
        let weight_x = x - left;
        let weight_y = y - top;
        let (left_x, right_x) = (clamp_index(left, width), clamp_index(left + 1.0, width));
        let (top_y, bottom_y) = (clamp_index(top, height), clamp_index(top + 1.0, height));
        Self {
            corners: [
                (left_x, top_y),
                (right_x, top_y),
                (left_x, bottom_y),
                (right_x, bottom_y),
            ],
            weights: [
                (1.0 - weight_x) * (1.0 - weight_y),
                weight_x * (1.0 - weight_y),
                (1.0 - weight_x) * weight_y,
                weight_x * weight_y,
            ],
        }
    }
}

#[inline]
fn clamp_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    if value <= 0.0 {
        0
    } else if value >= f64::from(last) {
        last
    } else {
        value as u32
    }
}

/// A spatial boundary ([`super::Entry::Spatial`]): the operation, the recipe prefix its estimates
/// are keyed by and the estimates a windowed proxy handed it, if any.
#[derive(Clone)]
pub(crate) struct SpatialEntry {
    pub(super) operation: SpatialOperation,
    pub(crate) stage: crate::EffectStage,
    pub(crate) fit_settle: crate::FitSettle,
    /// The SHA-256 of the canonical JSON of the layers before this one, which together with the
    /// source and the stage identifies what a global estimate was prepared from.
    prefix_hash: String,
    /// The global estimates this operation is handed instead of reducing its own stage: set only
    /// by a windowed proxy, whose stage is a window that cannot be reduced as a whole
    /// ([`super::window`]). `None` everywhere else.
    pub(super) globals: Option<super::window::Globals>,
}

impl SpatialEntry {
    pub(super) fn new(operation: SpatialOperation, prefix_hash: String) -> Self {
        Self {
            operation,
            stage: crate::EffectStage::Spatial,
            fit_settle: crate::FitSettle::Proxy,
            prefix_hash,
            globals: None,
        }
    }

    /// Whether any unit of the operation prepares a global estimate from a reduction of its stage.
    pub(super) fn prepares_estimates(&self) -> bool {
        self.operation
            .units()
            .iter()
            .any(|unit| unit.estimate_key().is_some())
    }

    /// The rectangle of its `received` stage that producing `window` of it reads: `window` grown by
    /// the operation's summed halo and clamped to the stage, with its origin moved down to the grid
    /// of the tiles the operation runs in ([`Tiling::Halo`]), so a stage cut to it holds exactly
    /// the whole stage's own tiles there.
    pub(super) fn reads(&self, window: Region, received: Stage) -> Region {
        let operation = &self.operation;
        let grown = window.grown(operation.summed_halo(received), received);
        let tile = Tiling::Halo.tile(operation, received);
        let x0 = grown.x0 / tile * tile;
        let y0 = grown.y0 / tile * tile;
        Region {
            x0,
            y0,
            width: grown.x1() - x0,
            height: grown.y1() - y0,
        }
    }

    /// Cut to `previous`, the rectangle of its `whole` stage its cut stage holds: its mask is read
    /// at the window's offset, and an operation that prepares a global estimate is handed the
    /// whole stage's, from `globals`.
    pub(super) fn cut(
        &mut self,
        previous: Region,
        whole: Stage,
        globals: impl FnOnce() -> Result<Vec<Option<Global>>, Error>,
    ) -> Result<(), Error> {
        if previous != Region::whole(whole)
            && let Some(mask) = self.operation.mask()
        {
            let windowed = mask.windowed(previous);
            self.operation = self.operation.clone().with_mask(windowed);
        }
        if self.prepares_estimates() {
            self.globals = Some(Arc::new(globals()?));
        }
        Ok(())
    }

    /// The global estimates this entry reads over `stage` in `domain`: the ones it was handed, or
    /// else the store's, or one preparation from `reduce`'s reduction of the stage. A frame and a
    /// point evaluation of the same recipe ask with the same key, so they use the same estimate.
    fn globals<D: PixelDomain>(
        &self,
        domain: &D,
        context: &RenderContext,
        stage: Stage,
        reduce: impl FnOnce() -> Result<Reduction, Error>,
    ) -> Result<Vec<Option<Global>>, Error> {
        if let Some(globals) = &self.globals {
            return Ok(globals.as_ref().clone());
        }
        resolve_globals(
            context.estimates(),
            &self.operation,
            stage,
            domain.fingerprint(),
            &input_prefix_key(domain, &self.prefix_hash),
            reduce,
        )
    }
}

/// One spatial entry's output over its whole `stage`, written tile by tile into the domain's frame:
/// the one place either driver materializes a spatial operation. `fill` reads one rectangle of the
/// stage it reads into three planes, for every tile and, on a store miss, for the reduction its
/// global estimates ([`SpatialEntry::globals`]) are prepared from.
/// Every tile runs through [`run_tile`], in batches whose concurrency the spatial budget sets,
/// checking `cancel` between batches. No full-frame float buffer exists beside the output, only
/// one tile's working set per tile in flight, charged to the spatial budget before each batch of
/// tiles allocates.
#[allow(clippy::too_many_arguments)]
pub(super) fn spatial_entry<D: PixelDomain>(
    domain: &D,
    entry: &SpatialEntry,
    stage: Stage,
    tiling: Tiling,
    cancel: &Cancel,
    context: &RenderContext,
    wide: bool,
    fill: impl Fn(Region, &mut [f32], Parallelism) -> Result<(), Error> + Sync,
) -> Result<D::SpatialFrame, Error> {
    let operation = &entry.operation;
    let plan = SpatialPlan::new(operation, stage, tiling)?;
    let globals = entry.globals(domain, context, stage, || {
        build_reduction_cancellable(stage, cancel, |region, planes| {
            fill(region, planes, Parallelism::Serial)
        })
    })?;
    let mut frame = D::spatial_frame(stage, wide)?;
    #[cfg(test)]
    context.note_spatial_frame();
    run_batches(
        &plan,
        context.spatial(),
        cancel,
        |tile, parallelism, scratch| {
            let (region, values) = run_tile(
                &plan,
                operation,
                &globals,
                tile,
                parallelism,
                scratch,
                cancel,
                |region, planes| fill(region, planes, parallelism),
            )?;
            Ok(D::tile_output(region, values, tile, parallelism, wide))
        },
        |tile, output| {
            D::write_tile(&mut frame, stage, tile, output);
            Ok(())
        },
    )?;
    Ok(frame)
}

/// How one domain holds a chunk of a segment's output rows while the segment's operations run over
/// them. [`segment_pass`] calls these in the one order both domains share.
pub(super) trait SegmentRows: Sync {
    type Sample: Send + Sync;
    fn samples_per_pixel(&self) -> usize {
        4
    }
    /// What one worker reuses for every chunk it takes.
    type Scratch: Default + Send;

    /// The float scratch one chunk of `rows` rows of `width` pixels holds while it runs, of which
    /// colour runs reach `coloured`, reserved from the colour budget; zero reserves nothing.
    fn scratch_bytes(&self, width: usize, rows: usize, coloured: usize) -> usize;

    /// Read the segment's input into the chunk of output rows starting at row `y0`: through the
    /// segment's exact geometry, from its input frame or its entry.
    fn load(
        &self,
        scratch: &mut Self::Scratch,
        y0: u32,
        chunk: &mut [Self::Sample],
    ) -> Result<(), Error>;

    /// Write one point replacement at pixel `offset` of the chunk.
    fn replace(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [Self::Sample],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error>;

    /// Apply one colour run to the chunk's rows `rows`, which start at row `y0 + rows.start` of the
    /// stage. `snapshot` is one row of a masked operation's own input.
    fn run(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [Self::Sample],
        run: &ColorRun<'_>,
        y0: u32,
        rows: Range<usize>,
        snapshot: &mut [[f32; 3]],
    ) -> Result<(), Error>;

    /// Write the chunk's finished values as its bytes.
    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [Self::Sample]) -> Result<(), Error>;
}

/// One segment's output, `frame`, written in bounded row chunks: each chunk is loaded, then the
/// segment's replacements and colour runs are applied to it as ordered phases — the replacements
/// before a colour run, then that run, then the replacements after it, so a later replacement wins
/// at the same coordinate exactly as a later layer does — and stored. Colour runs reach only the
/// rows in `band`; a resample that follows reads no other.
///
/// The chunks run on the shared Rayon pool when the segment's geometry, its colour runs or its
/// heavy colour runs reach their own threshold (`super::parallel`); smaller passes stay serial.
/// No full-frame float buffer exists at any point: each chunk reserves its float scratch from
/// `budget` before it uses it, in a buffer allocated once per Rayon split and reused by that
/// split's chunks. `cancel` is read once per chunk, before the reservation.
///
/// Pointwise operations are independent per pixel and a unit is handed one row at a time with its
/// row and first column, so applying the phases chunk by chunk is the same arithmetic in the same
/// order as applying each phase to the whole frame, and as [`Evaluation::pixel_in`] applying the
/// runs after the winning replacement to one pixel.
pub(super) fn segment_pass<R: SegmentRows>(
    rows: &R,
    segment: &Segment,
    frame: &mut [R::Sample],
    band: Range<usize>,
    cancel: &Cancel,
    budget: &ScratchBudget,
) -> Result<(), Error> {
    let width = segment.width as usize;
    if frame.is_empty() || width == 0 {
        return Ok(());
    }
    let replacements = mapped_replacements(segment);
    let runs: Vec<ColorRun<'_>> = color_runs(segment).collect();
    // Whether this pass needs snapshot scratch at all, decided once for the pass: an unmasked
    // segment reserves and allocates exactly what it would without masks.
    let masked = runs.iter().any(|run| run.has_mask());
    // The segment's geometry runs on the pool from the transform threshold, counted over the whole
    // segment; its colour runs from the colour threshold, or the lower heavy-colour one with
    // several colour units or a mask, counted over the rows they reach.
    let parallel = {
        use super::parallel::{RenderPass, pooled};
        let units: usize = runs
            .iter()
            .flat_map(|run| run.colour_operations())
            .map(|(_, operation)| operation.len())
            .sum();
        let colour = match units {
            _ if masked || units >= 3 => Some(RenderPass::HeavyColour),
            0 => None,
            _ => Some(RenderPass::Colour),
        };
        let coloured = segment.width as u64 * (band.end - band.start) as u64;
        pooled(
            RenderPass::Transform,
            segment.width as u64 * segment.height as u64,
        ) || colour.is_some_and(|pass| pooled(pass, coloured))
    };
    let chunk_rows = color_chunk_rows(segment.width);
    let chunk_bytes = chunk_rows * width * rows.samples_per_pixel();
    let process = |scratch: &mut (R::Scratch, Vec<[f32; 3]>),
                   index: usize,
                   chunk: &mut [R::Sample]|
     -> Result<(), Error> {
        // Before the reservation, so a cancelled pass never takes scratch it will not use.
        cancel.check()?;
        let count = chunk.len() / (width * rows.samples_per_pixel());
        let y0 = index * chunk_rows;
        // The chunk's rows that colour runs reach, as rows of the chunk.
        let coloured = band.start.clamp(y0, y0 + count) - y0..band.end.clamp(y0, y0 + count) - y0;
        let colours = !runs.is_empty() && !coloured.is_empty();
        let bytes = rows.scratch_bytes(width, count, coloured.len());
        let _reservation = (bytes > 0).then(|| budget.reserve(bytes));
        // One row of snapshot scratch for a masked operation's own input, reserved before it is
        // used and released with the chunk. It is a row and not a chunk because a unit is handed
        // one row at a time, and an unmasked segment takes none of it.
        let _snapshot_reservation =
            (masked && colours).then(|| budget.reserve(width * std::mem::size_of::<[f32; 3]>()));
        let (scratch, snapshot) = scratch;
        let mut unused = [[0.0f32; 3]; 1];
        let snapshot: &mut [[f32; 3]] = if masked {
            // Allocated by the worker's first chunk and exactly one row long from then on; a
            // masked operation overwrites what it reads, so an earlier chunk's values never show.
            snapshot.resize(width, [0.0; 3]);
            snapshot
        } else {
            &mut unused
        };
        rows.load(scratch, y0 as u32, chunk)?;
        let mut next = 0;
        let mut write = |scratch: &mut R::Scratch, chunk: &mut [R::Sample], before: usize| {
            while let Some(&(index, x, y, rgb)) = replacements.get(next) {
                if index >= before {
                    break;
                }
                let y = y as usize;
                if (y0..y0 + count).contains(&y) {
                    rows.replace(scratch, chunk, (y - y0) * width + x as usize, rgb)?;
                }
                next += 1;
            }
            Ok::<(), Error>(())
        };
        for run in &runs {
            write(scratch, chunk, run.start)?;
            if colours {
                rows.run(scratch, chunk, run, y0 as u32, coloured.clone(), snapshot)?;
            }
        }
        write(scratch, chunk, usize::MAX)?;
        rows.store(scratch, chunk)
    };
    if parallel {
        frame
            .par_chunks_mut(chunk_bytes)
            .enumerate()
            .try_for_each_init(Default::default, |scratch, (index, chunk)| {
                process(scratch, index, chunk)
            })
    } else {
        let mut scratch = Default::default();
        frame
            .chunks_mut(chunk_bytes)
            .enumerate()
            .try_for_each(|(index, chunk)| process(&mut scratch, index, chunk))
    }
}
