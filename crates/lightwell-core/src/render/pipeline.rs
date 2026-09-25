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
//! by tile ([`spatial_output`]) and the terminal conversion a sample and a grid share.
//!
//! What stays with each domain's rasterizer is which frames it materializes. The byte driver
//! ([`super::rasterize`]) writes every segment's output as a byte frame, because a byte frame is
//! exactly what the next boundary reads. The linear driver ([`super::linear::rasterize`]) pulls
//! every output pixel through [`Evaluation`], because a linear value between two boundaries is an
//! `f64` the next resample blends: materializing it as `f32` would change bytes and as `f64` would
//! double the RAW planar bound. A spatial operation's output is `f32` on both paths, so the linear
//! driver materializes those, as a render always has.

use super::{
    Cancel, ColorRun, Compiled, Entry, RenderContext, color_runs,
    spatial::{
        PointTiles, SpatialPlan, build_reduction, fill_planes, resolve_globals, run_batches,
        run_tile,
    },
};
use crate::{
    Error, ErrorKind,
    modules::{Global, Parallelism, Region, SpatialOperation, Stage},
};
#[cfg(test)]
use std::sync::Weak;
use std::{borrow::Cow, sync::Arc};

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
    /// stage, with the arithmetic a rasterizing pass applies to a row of them.
    fn colour<'r>(
        pixel: Self::Pixel,
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
    ) -> Result<Self::Pixel, Error>;

    /// The pixel a segment answers, once its replacement and colour are applied.
    fn finish(pixel: Self::Pixel) -> Result<Self::Pixel, Error>;

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
        alpha: &(dyn Fn(u32, u32) -> u8 + Sync),
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
            }) = &evaluation.compiled.segments[index].entry
            else {
                continue;
            };
            let stage = evaluation.spatial_stage(index);
            let planes = Arc::new(spatial_output::<D>(
                stage,
                operation,
                || evaluation.spatial_globals(index, operation, stage, prefix_hash),
                evaluation.tile,
                cancel,
                evaluation.context,
                |x, y| evaluation.spatial_read(index, x, y),
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
    pub(super) fn entry_pixel(&self, index: usize, x: u32, y: u32) -> Result<D::Pixel, Error> {
        match &self.compiled.segments[index].entry {
            None => self.domain.source_pixel(x, y),
            Some(Entry::Spatial {
                operation,
                prefix_hash,
            }) => match &self.frame {
                Some(frame) if frame.index == index => Ok(D::frame_pixel(
                    &frame.planes,
                    self.spatial_stage(index),
                    x,
                    y,
                )),
                _ => self.spatial_pixel(index, operation, prefix_hash, x, y),
            },
            Some(Entry::Resample(resample)) => {
                let previous = &self.compiled.segments[index - 1];
                let (u, v) = resample.input_at(x, y);
                D::blend(u, v, previous.width, previous.height, |x, y| {
                    self.pixel_in(index - 1, x, y)?.ok_or_else(|| {
                        Error::new(ErrorKind::Render, "a resample tap was outside its stage")
                    })
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
        let pixel = self.pixel_in(index - 1, x, y)?.ok_or_else(|| {
            Error::new(
                ErrorKind::Render,
                "a spatial read was outside its input stage",
            )
        })?;
        Ok(D::spatial_input(pixel))
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
    ) -> Result<Vec<Option<Global>>, Error> {
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
            || self.spatial_globals(index, operation, stage, prefix_hash),
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
                let (u, v) = resample.input_at(x, y);
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
/// frame: `globals` resolves the operation's global estimates once the plan is known, `read` pulls
/// one pixel of the stage it reads and `alpha` that pixel's alpha. Every tile runs through
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
    read: impl Fn(u32, u32) -> Result<[f32; 3], Error> + Sync,
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
                |region, planes| fill_planes(region, planes, parallelism, &read),
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
