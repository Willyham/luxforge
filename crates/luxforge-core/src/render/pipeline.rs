//! The one pipeline, generic over its pixel domain.
//!
//! A recipe compiles into segments separated by stage boundaries (a resample or a spatial
//! operation), and every segment is its exact geometry, point replacements and colour runs. What a
//! pixel *is* between those steps is the only thing that differs between a JPEG and a developed
//! RAW, and [`PixelDomain`] is where that difference lives:
//!
//! - **Byte** ([`super::Byte`]): 8-bit sRGB with alpha. Each colour run decodes through the sRGB
//!   table, runs its units and quantizes at the run's end, so a point replacement, a resample and
//!   the end of the recipe are quantization boundaries, as is a spatial operation's output.
//! - **Linear** ([`super::linear::Linear`]): signed unbounded `f64` linear sRGB, with the RAW
//!   exposure and approximate white balance applied to each source pixel. A colour segment is `f32`
//!   from its entry to its end, nothing is quantized before the terminal boundary, and there is no
//!   alpha.
//!
//! Everything else exists once, here: walking a point back through the segments ([`Evaluation`]),
//! the resample recursion, a spatial entry answered from the query's tiles or from a materialized
//! frame, the global estimates a spatial operation reads, a spatial operation's output built tile
//! by tile ([`spatial_output`]), the replacements and colour runs applied to rows of a segment's
//! output ([`segment_pass`]) and the terminal conversion a sample and a grid share.
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
    Cancel, ColorRun, Compiled, Entry, RenderContext, ScratchBudget, Segment, color_chunk_rows,
    color_runs, mapped_replacements,
    spatial::{
        PointTiles, SpatialPlan, build_reduction, fill_planes, resolve_globals, run_batches,
        run_tile,
    },
};
use crate::{
    Error,
    modules::{Global, Parallelism, Region, SpatialOperation, Stage},
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
    /// One evaluated tile, as the parallel half of [`spatial_output`] hands it to the serial write.
    type TileOutput: Send;

    /// How [`super::Render::grid`] answers the pixels of its spatial segments. The byte path reads
    /// the points through one tile cache; the linear path materializes each spatial output once,
    /// as a render does, since the points spread over the whole stage and a linear spatial frame is
    /// exact.
    const GRID: SpatialMode;

    /// The fingerprint a global estimate is keyed by.
    fn fingerprint(&self) -> &str;

    /// What identifies a spatial operation's input beyond the source fingerprint and the layers
    /// before it. The byte path's input is the recipe prefix over the decoded source, so it is
    /// the prefix hash. The linear path's also depends on the development, the view and the
    /// settings, none of which the recipe names.
    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str>;

    /// Refuse a compilation this domain cannot evaluate. Nothing is refused on the byte path; the
    /// linear path evaluates at most one resample.
    fn check(&self, compiled: &Compiled) -> Result<(), Error>;

    /// Refuse an output stage this domain cannot produce, before a point is answered in it.
    fn check_output(&self, width: u32, height: u32) -> Result<(), Error>;

    /// One pixel of the source, which is the first segment's input.
    fn source_pixel(&self, x: u32, y: u32) -> Result<Self::Pixel, Error>;

    /// The source's alpha at one pixel, which only the byte domain has.
    fn source_alpha(&self, x: u32, y: u32) -> u8;

    /// A point replacement's value written over `pixel`.
    fn replace(pixel: Self::Pixel, rgb: [u8; 3]) -> Self::Pixel;

    /// Every colour run of `runs`, in order, over one pixel at `(x, y)` of its segment's output
    /// stage: the per-pixel form of [`SegmentRows::run`], with the same arithmetic.
    fn colour<'r>(
        pixel: Self::Pixel,
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
    ) -> Result<Self::Pixel, Error>;

    /// The pixel a segment answers, once its replacement and colour are applied.
    fn finish(pixel: Self::Pixel) -> Result<Self::Pixel, Error>;

    /// [`Self::colour`] and [`Self::finish`] over one contiguous run of row `y` starting at column
    /// `x0`. Units are pointwise and are handed a row with its coordinates, so a domain may run
    /// them over the whole row at once; this default applies them one pixel at a time.
    fn colour_row<'r>(
        pixels: &mut [Self::Pixel],
        runs: impl Iterator<Item = ColorRun<'r>> + Clone,
        y: u32,
        x0: u32,
        _scratch: &mut RowScratch,
    ) -> Result<(), Error> {
        for (offset, pixel) in pixels.iter_mut().enumerate() {
            *pixel = Self::finish(Self::colour(*pixel, runs.clone(), x0 + offset as u32, y)?)?;
        }
        Ok(())
    }

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

    /// A spatial operation's output value as a pixel; `alpha` is the input's alpha there, asked
    /// only by a domain that has one.
    fn spatial_output(rgb: [f32; 3], alpha: impl FnOnce() -> u8) -> Result<Self::Pixel, Error>;

    /// The terminal byte of one output pixel.
    fn terminal(pixel: Self::Pixel) -> Result<[u8; 4], Error>;

    /// A pixel in linear light, as a value-based mask component reads it.
    fn mask_input(pixel: Self::Pixel) -> [f64; 3];

    /// An empty spatial frame of `stage`, inside the domain's frame limit.
    fn spatial_frame(stage: Stage) -> Result<Self::SpatialFrame, Error>;

    /// One tile's output, from the rectangle `region` its last unit wrote, in the form
    /// [`Self::write_tile`] places.
    fn tile_output(
        region: Region,
        values: Vec<f32>,
        tile: Region,
        parallelism: Parallelism,
    ) -> Self::TileOutput;

    /// Place one tile's output into the frame. `alpha` reads the operation's input alpha, which
    /// the byte frame copies and the linear planes do not hold.
    fn write_tile(
        frame: &mut Self::SpatialFrame,
        stage: Stage,
        tile: Region,
        output: Self::TileOutput,
        alpha: &impl Fn(u32, u32) -> u8,
    );

    /// One pixel of a spatial frame of `stage`.
    fn frame_pixel(frame: &Self::SpatialFrame, stage: Stage, x: u32, y: u32) -> Self::Pixel;
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
    /// The latest spatial frame, in [`SpatialMode::Frames`]. A pull stops at the first spatial
    /// entry it meets walking back — a spatial entry reads its own frame and only a resample reads
    /// the segment before it — so once a spatial segment's frame exists nothing reads an earlier
    /// one. Building a frame therefore holds at most two, and the evaluation keeps one.
    pub(super) frame: Option<SpatialFrame<D::SpatialFrame>>,
    /// Every frame this evaluation built, in order, with how many of them were alive when it was
    /// finished and before the one it replaces was released, which is the peak.
    #[cfg(test)]
    pub(super) built: Vec<(Weak<D::SpatialFrame>, usize)>,
    /// In [`SpatialMode::Point`], the tiles of every spatial segment this query has evaluated, in
    /// tiles of [`super::spatial::PRODUCTION_TILE`] everywhere but in the tests that prove the
    /// result does not depend on it.
    pub(super) tiles: Option<PointTiles<'a>>,
    tile: u32,
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
        tile: u32,
        mode: SpatialMode,
        cancel: &Cancel,
        context: &'a RenderContext,
    ) -> Result<Self, Error> {
        domain.check(&compiled)?;
        let mut evaluation = Self {
            domain,
            compiled,
            frame: None,
            #[cfg(test)]
            built: Vec::new(),
            tiles: (mode == SpatialMode::Point).then(|| PointTiles::new(tile, context.spatial())),
            tile,
            context,
        };
        if mode == SpatialMode::Point {
            return Ok(evaluation);
        }
        // In order, because a later spatial operation pulls its input through the earlier one, and
        // each frame replaces the one before it once it exists.
        for index in 0..evaluation.compiled.segments.len() {
            let Some(Entry::Spatial {
                operation,
                prefix_hash,
                globals,
            }) = &evaluation.compiled.segments[index].entry
            else {
                continue;
            };
            let stage = evaluation.spatial_stage(index);
            let planes = Arc::new(spatial_output::<D>(
                stage,
                operation,
                || evaluation.spatial_globals(index, operation, stage, prefix_hash, globals),
                evaluation.tile,
                cancel,
                evaluation.context,
                |region, planes, parallelism| {
                    evaluation.fill_rows(index - 1, region, planes, parallelism)
                },
                |x, y| evaluation.alpha_in(index - 1, x, y).unwrap_or(255),
            )?);
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

    #[cfg(test)]
    /// The same point evaluation with another spatial tile size. A spatial unit's value at a
    /// pixel depends on that pixel's neighbourhood only, so this changes nothing but the schedule.
    pub(crate) fn with_tile(mut self, tile: u32) -> Self {
        self.tile = tile;
        self.tiles = Some(PointTiles::new(tile, self.context.spatial()));
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

    /// The global estimates the spatial operation entering segment `index` reads, from the store or
    /// from one reduction of its input stage, exactly as a frame of this compilation would resolve
    /// them; the store then holds them for that frame. Empty when segment `index` does not enter
    /// through a spatial operation.
    pub(crate) fn globals_of(&self, index: usize) -> Result<Vec<Option<Global>>, Error> {
        match &self.compiled.segments[index].entry {
            Some(Entry::Spatial {
                operation,
                prefix_hash,
                globals,
            }) => {
                let stage = self.spatial_stage(index);
                self.spatial_globals(index, operation, stage, prefix_hash, globals)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// One output pixel, `None` when the coordinate lies outside the output stage.
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Result<Option<D::Pixel>, Error> {
        self.pixel_in(self.compiled.segments.len() - 1, x, y)
    }

    /// One output pixel's terminal byte, `None` outside the output stage.
    pub(crate) fn terminal(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.pixel(x, y)?.map(D::terminal).transpose()
    }

    /// One pixel of one segment's output stage. A resample is evaluated recursively as the bilinear
    /// blend of four pixels of the previous segment, so a point query costs `O(layers · 4^resamples)`
    /// and never allocates a frame; a stack holds at most one crop layer.
    ///
    /// A spatial entry is the one exception to "a point query never rasterizes", declared in the
    /// [performance rules](../../docs/engineering/performance-rules.md): see
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
            )?;
        }
        D::finish(pixel).map(Some)
    }

    /// One pixel of segment `index`'s input frame, which its exact geometry reads: the source, a
    /// spatial operation's output or a resample of the segment before it.
    #[inline(always)]
    pub(super) fn entry_pixel(&self, index: usize, x: u32, y: u32) -> Result<D::Pixel, Error> {
        match &self.compiled.segments[index].entry {
            None => self.domain.source_pixel(x, y),
            Some(Entry::Spatial {
                operation,
                prefix_hash,
                globals,
            }) => match &self.frame {
                Some(frame) if frame.index == index => Ok(D::frame_pixel(
                    &frame.planes,
                    self.spatial_stage(index),
                    x,
                    y,
                )),
                _ => self.spatial_pixel(index, operation, prefix_hash, globals, x, y),
            },
            Some(Entry::Resample(resample)) => {
                let previous = &self.compiled.segments[index - 1];
                let origin = self.compiled.segments[index].entry_origin;
                let (u, v) = resample.input_from(origin, x, y);
                D::blend(u, v, previous.width, previous.height, |x, y| {
                    self.pixel_in(index - 1, x, y)?
                        .ok_or_else(|| Error::render("a resample tap was outside its stage"))
                })
            }
        }
    }

    /// The stage spatial segment `index` reads, which is the stage it writes.
    fn spatial_stage(&self, index: usize) -> Stage {
        let previous = &self.compiled.segments[index - 1];
        Stage {
            width: previous.width,
            height: previous.height,
        }
    }

    /// One pixel of the stage spatial segment `index` reads: the previous segment's output, pulled
    /// through [`Self::pixel_in`], which already applies every replacement and colour run.
    fn spatial_read(&self, index: usize, x: u32, y: u32) -> Result<[f32; 3], Error> {
        let pixel = self
            .pixel_in(index - 1, x, y)?
            .ok_or_else(|| Error::render("a spatial read was outside its input stage"))?;
        Ok(D::spatial_input(pixel))
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
                )?;
            }
        }
        Ok(())
    }

    /// Segment `index`'s output over `region`, as the three planes a spatial operation reads: one
    /// [`Self::region_in`] per row, on the pool under [`Parallelism::Pool`], for a segment whose
    /// colour runs over rows. Any other segment is read pixel by pixel straight into the planes,
    /// since a row buffer would only copy what a pull already answers.
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
                self.spatial_read(index + 1, x, y)
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

    /// The global estimates of spatial segment `index`, from the store or from one reduction of
    /// its input stage. A frame and a point evaluation of the same recipe ask with the same key, so
    /// they use the same estimate. A frame's reduction reads the stage as a render does; a point
    /// query's reads through [`PointTiles::reduce`].
    fn spatial_globals(
        &self,
        index: usize,
        operation: &SpatialOperation,
        stage: Stage,
        prefix_hash: &str,
        handed: &Option<super::window::Globals>,
    ) -> Result<Vec<Option<Global>>, Error> {
        if let Some(globals) = handed {
            return Ok(globals.as_ref().clone());
        }
        let read = |x: u32, y: u32| self.spatial_read(index, x, y);
        resolve_globals(
            self.context.estimates(),
            operation,
            stage,
            self.domain.fingerprint(),
            &self.domain.estimate_prefix(prefix_hash),
            || match &self.tiles {
                Some(tiles) => tiles.reduce(stage, self.compiled.spatial_before(index), read),
                None => build_reduction(stage, read),
            },
        )
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
        operation: &SpatialOperation,
        prefix_hash: &str,
        handed: &Option<super::window::Globals>,
        x: u32,
        y: u32,
    ) -> Result<D::Pixel, Error> {
        let tiles = self
            .tiles
            .as_ref()
            .expect("a pull meets only the latest frame or a point query's tiles");
        let stage = self.spatial_stage(index);
        let rgb = tiles.pixel(
            index,
            operation,
            stage,
            x,
            y,
            || self.spatial_globals(index, operation, stage, prefix_hash, handed),
            |x, y| self.spatial_read(index, x, y),
        )?;
        // Alpha is never touched by a unit; it is the input frame's, exactly as the render copies
        // it.
        D::spatial_output(rgb, || self.alpha_in(index - 1, x, y).unwrap_or(255))
    }

    /// The alpha of one pixel of one segment's output stage, or `None` outside it. No unit, point
    /// replacement or colour run writes alpha and a spatial boundary copies its input's, so this
    /// walks the geometry alone, blending through a resample exactly as the byte frame does, and
    /// never evaluates a colour run or a spatial tile.
    fn alpha_in(&self, index: usize, x: u32, y: u32) -> Option<u8> {
        let segment = &self.compiled.segments[index];
        let resolved = segment.resolve(x, y)?;
        let (x, y) = (resolved.input_x, resolved.input_y);
        match &segment.entry {
            None => Some(self.domain.source_alpha(x, y)),
            Some(Entry::Spatial { .. }) => self.alpha_in(index - 1, x, y),
            Some(Entry::Resample(resample)) => {
                let previous = &self.compiled.segments[index - 1];
                let (u, v) = resample.input_from(segment.entry_origin, x, y);
                let taps = Taps::new(u, v, previous.width, previous.height);
                let alpha: f64 = taps
                    .corners
                    .iter()
                    .zip(taps.weights)
                    .map(|(&(x, y), weight)| {
                        weight
                            * f64::from(
                                self.alpha_in(index - 1, x, y)
                                    .expect("clamped indices stay inside"),
                            )
                    })
                    .sum();
                Some(alpha.round().clamp(0.0, 255.0) as u8)
            }
        }
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

/// One spatial operation's output over its whole `stage`, written tile by tile into the domain's
/// frame: `globals` resolves the operation's global estimates once the plan is known, `fill` reads
/// one rectangle of the stage it reads into three planes and `alpha` one pixel's alpha. Every tile runs through
/// [`run_tile`], in batches whose concurrency the spatial budget sets, checking `cancel` between
/// batches. No full-frame float buffer exists beside the output, only one tile's working set per
/// tile in flight, charged to the spatial budget before each batch of tiles allocates.
#[allow(clippy::too_many_arguments)]
pub(super) fn spatial_output<D: PixelDomain>(
    stage: Stage,
    operation: &SpatialOperation,
    globals: impl FnOnce() -> Result<Vec<Option<Global>>, Error>,
    tile: u32,
    cancel: &Cancel,
    context: &RenderContext,
    fill: impl Fn(Region, &mut [f32], Parallelism) -> Result<(), Error> + Sync,
    alpha: impl Fn(u32, u32) -> u8 + Sync,
) -> Result<D::SpatialFrame, Error> {
    let plan = SpatialPlan::new(operation, stage, tile)?;
    let globals = globals()?;
    let mut frame = D::spatial_frame(stage)?;
    run_batches(
        &plan,
        context.spatial(),
        cancel,
        |tile, parallelism| {
            let (region, values) = run_tile(
                &plan,
                operation,
                &globals,
                tile,
                parallelism,
                |region, planes| fill(region, planes, parallelism),
            )?;
            Ok(D::tile_output(region, values, tile, parallelism))
        },
        |tile, output| {
            D::write_tile(&mut frame, stage, tile, output, &alpha);
            Ok(())
        },
    )?;
    Ok(frame)
}

/// How one domain holds a chunk of a segment's output rows while the segment's operations run over
/// them. [`segment_pass`] calls these in the one order both domains share.
pub(super) trait SegmentRows: Sync {
    /// What one worker reuses for every chunk it takes.
    type Scratch: Default + Send;

    /// The float scratch one chunk of `rows` rows of `width` pixels holds while it runs, of which
    /// colour runs reach `coloured`, reserved from the colour budget; zero reserves nothing.
    fn scratch_bytes(&self, width: usize, rows: usize, coloured: usize) -> usize;

    /// Read the segment's input into the chunk of output rows starting at row `y0`: through the
    /// segment's exact geometry, from its input frame or its entry.
    fn load(&self, scratch: &mut Self::Scratch, y0: u32, chunk: &mut [u8]) -> Result<(), Error>;

    /// Write one point replacement at pixel `offset` of the chunk.
    fn replace(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [u8],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error>;

    /// Apply one colour run to the chunk's rows `rows`, which start at row `y0 + rows.start` of the
    /// stage. `snapshot` is one row of a masked operation's own input.
    fn run(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [u8],
        run: &ColorRun<'_>,
        y0: u32,
        rows: Range<usize>,
        snapshot: &mut [[f32; 3]],
    ) -> Result<(), Error>;

    /// Write the chunk's finished values as its bytes.
    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [u8]) -> Result<(), Error>;
}

/// One segment's output, `frame`, written in bounded row chunks: each chunk is loaded, then the
/// segment's replacements and colour runs are applied to it as ordered phases — the replacements
/// before a colour run, then that run, then the replacements after it, so a later replacement wins
/// at the same coordinate exactly as a later layer does — and stored. Colour runs reach only the
/// rows in `band`; a resample that follows reads no other.
///
/// The chunks run on the shared Rayon pool above the one-megapixel threshold and serially below it.
/// No full-frame float buffer exists at any point: each chunk reserves its float scratch from
/// `budget` before it uses it, in a buffer its worker allocates once and reuses. `cancel` is read
/// once per chunk, before the reservation.
///
/// Pointwise operations are independent per pixel and a unit is handed one row at a time with its
/// row and first column, so applying the phases chunk by chunk is the same arithmetic in the same
/// order as applying each phase to the whole frame, and as [`Evaluation::pixel_in`] applying the
/// runs after the winning replacement to one pixel.
pub(super) fn segment_pass<R: SegmentRows>(
    rows: &R,
    segment: &Segment,
    frame: &mut [u8],
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
    let chunk_rows = color_chunk_rows(segment.width);
    let chunk_bytes = chunk_rows * width * 4;
    let process = |scratch: &mut (R::Scratch, Vec<[f32; 3]>),
                   index: usize,
                   chunk: &mut [u8]|
     -> Result<(), Error> {
        // Before the reservation, so a cancelled pass never takes scratch it will not use.
        cancel.check()?;
        let count = chunk.len() / (width * 4);
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
        let mut write = |scratch: &mut R::Scratch, chunk: &mut [u8], before: usize| {
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
    if segment.width as u64 * segment.height as u64 >= super::PARALLEL_RENDER_PIXELS {
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
