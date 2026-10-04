//! The finite-support machinery the three Presence units share: rectangles, scratch planes, the
//! box mean, the box minimum, the two guided filters, the integer reduction and bilinear upsample,
//! the radius and halo rules, and the compressive gain.
//!
//! This file is the `f32` production transcription of `docs/design/presence-study.md` and of the
//! independent `f64` reference at `crates/luxforge-reference/src/presence.rs`; the three
//! must be read together, and every constant here is named identically to the constant of the same
//! name there. The sRGB working domain, luminance and the luminance-ratio reconstruction are not
//! restated: the units use [`crate::colour`]'s, which are the analytically continued transfer
//! function and the tone study's reconstruction the study quotes.
//!
//! **Two deliberate differences from the reference, both inside the frozen tolerance.**
//!
//! 1. The reference evaluates every box window by direct summation so that a tile and the whole
//!    frame agree bit for bit. Production accumulates each box pass as a running sum in `f64` —
//!    entering value added, leaving value subtracted — which is what the study's "up to float
//!    summation order" allowance covers. The accumulator is `f64`, so the drift along one pass is
//!    on the order of `1e-13` rather than the `1e-6` a `f32` running sum would carry, and the
//!    result is closer to the `f64` reference than direct `f32` summation is. A tiled run therefore
//!    agrees with a whole-frame run within the frozen tolerance rather than bit for bit.
//! 2. The vertical pass walks strips of [`STRIP`] columns with one accumulator per column, so it
//!    reads its input row by row instead of column by column. Same arithmetic, cache-friendly
//!    order.
//!
//! Every pass takes the [`Parallelism`] the host chose for the tile. Under
//! [`Parallelism::Pool`] a pass runs its independent rows, or the vertical pass its strips, on the
//! shared pool; each value is still the same arithmetic in the same order, so the choice changes
//! when a value is computed and never a bit of it.
//!
//! Every plane below carries the frame it belongs to and the sub-rectangle it actually holds.
//! Reads clamp to the frame, exactly as the host's [`Planes::sample`](crate::modules::Planes) does,
//! and a read whose clamped coordinate is outside the held rectangle is a halo bug that panics in a
//! debug build and is caught by the slice bounds otherwise. The box mean, the guided filters and the
//! upsample read the columns where no tap clamps from row slices ([`Plane::row`], [`Plane::span`]),
//! cutting each row at the frame edges once, and the columns within their reach of a frame edge
//! through the clamped [`Plane::get`]; both read the same values, so the cut changes no result.

use crate::{
    Error,
    colour::{luma, srgb},
    modules::{Cells, GridPlanes, Parallelism, Planes, Region, Stage},
};
use rayon::prelude::*;
use std::ops::Range;

// ---------------------------------------------------------------------------------------------
// Frozen constants shared by more than one unit.
// ---------------------------------------------------------------------------------------------

/// The reference long side every radius is quoted at: 6000 px, a 24 MP stage.
const REFERENCE_LONG_SIDE: f64 = 6000.0;

/// The long side above which radii stop growing, so the summed halo stays inside the host's 512 px
/// bound at every stage the host accepts. The study records this as a deliberate trade.
const MAX_SCALE_LONG_SIDE: u32 = 10_000;

/// How many columns one vertical pass carries accumulators for at a time.
const STRIP: usize = 64;

/// How many rows one horizontal pass runs together. Each row keeps its own running sum, so the
/// rows' dependent add chains overlap rather than each waiting on its own previous add.
const ROWS: usize = 4;

/// The frozen radius scaling rule: a radius quoted at [`REFERENCE_LONG_SIDE`] scales with the
/// stage's long side (capped at [`MAX_SCALE_LONG_SIDE`]), rounded to an integer with a minimum
/// of 1.
pub(super) fn scaled_radius(radius_at_6000: f64, long_side: u32) -> i64 {
    let long_side = long_side.min(MAX_SCALE_LONG_SIDE);
    let scaled = radius_at_6000 * f64::from(long_side) / REFERENCE_LONG_SIDE;
    (scaled.round() as i64).max(1)
}

/// A full-resolution radius expressed on a grid reduced by `reduction` per axis, with the study's
/// minimum of one reduced pixel.
pub(super) fn reduced_radius(full: i64, reduction: i64) -> i64 {
    (((full as f64) / (reduction as f64)).round() as i64).max(1)
}

/// The full-resolution halo of a stage computed on a reduced grid: `reach` reduced pixels of filter
/// reach, one more reduced index for the bilinear upsample, and a full pixel sitting at the far end
/// of its own block.
pub(super) fn reduced_halo(reach: i64, reduction: i64) -> i64 {
    (reach + 2) * reduction - 1
}

/// Encoded luminance: the tone study's working domain, reused unchanged.
pub(super) fn encoded_luminance(rgb: [f32; 3]) -> f32 {
    srgb::encode_f32(luma::rec709(rgb))
}

/// The compressive (soft-clipping) gain both luminance units apply to their raw encoded excursion:
///
/// ```text
/// headroom = 1 - e        for raw > 0          headroom = e      for raw < 0
/// lim      = min(limit, max(0, headroom))
/// delta    = lim * tanh(raw / lim)             (0 when lim == 0)
/// ```
///
/// `|delta| < lim <= limit` bounds the excursion; `lim <= headroom` keeps an encoded value inside
/// `[0, 1]` from leaving it; outside `[0, 1]` the headroom is zero, so the value passes through
/// untouched rather than being clamped; and `raw = 0` returns exactly `0`.
pub(super) fn soft_clip(raw: f32, encoded: f32, limit: f32) -> f32 {
    if raw == 0.0 {
        return 0.0;
    }
    let headroom = if raw > 0.0 { 1.0 - encoded } else { encoded };
    let lim = limit.min(headroom.max(0.0));
    if lim <= 0.0 {
        return 0.0;
    }
    lim * (raw / lim).tanh()
}

// ---------------------------------------------------------------------------------------------
// Rectangles.
// ---------------------------------------------------------------------------------------------

/// A half-open rectangle in the pixel coordinates of some frame, signed so that a rectangle may be
/// grown past an edge before it is clipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) x0: i64,
    pub(super) y0: i64,
    pub(super) x1: i64,
    pub(super) y1: i64,
}

impl Rect {
    pub(super) fn new(x0: i64, y0: i64, x1: i64, y1: i64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub(super) fn frame(width: i64, height: i64) -> Self {
        Self::new(0, 0, width, height)
    }

    /// The host's region as a rectangle of the same stage.
    pub(super) fn of(region: crate::modules::Region) -> Self {
        Self::new(
            i64::from(region.x0),
            i64::from(region.y0),
            i64::from(region.x1()),
            i64::from(region.y1()),
        )
    }

    pub(super) fn width(self) -> i64 {
        (self.x1 - self.x0).max(0)
    }

    pub(super) fn height(self) -> i64 {
        (self.y1 - self.y0).max(0)
    }

    pub(super) fn pixels(self) -> usize {
        (self.width() * self.height()) as usize
    }

    pub(super) fn is_empty(self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    pub(super) fn contains(self, x: i64, y: i64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub(super) fn expand(self, by: i64) -> Self {
        Self::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }

    /// Grown along y only. A box mean's horizontal pass is held over exactly this rectangle:
    /// growing it in x as well would read `2r` columns beyond the output and inflate the filter's
    /// halo from `r` to `2r`.
    pub(super) fn expand_y(self, by: i64) -> Self {
        Self::new(self.x0, self.y0 - by, self.x1, self.y1 + by)
    }

    pub(super) fn clip(self, other: Rect) -> Self {
        Self::new(
            self.x0.max(other.x0),
            self.y0.max(other.y0),
            self.x1.min(other.x1),
            self.y1.min(other.y1),
        )
    }
}

/// The reduced frame size for an integer reduction factor: `ceil(size / s)` per axis, so the right
/// and bottom blocks may be partial.
pub(super) fn reduced_frame(width: i64, height: i64, reduction: i64) -> (i64, i64) {
    (
        width.div_euclid(reduction) + i64::from(width.rem_euclid(reduction) != 0),
        height.div_euclid(reduction) + i64::from(height.rem_euclid(reduction) != 0),
    )
}

/// The reduced rectangle covering a full-resolution rectangle, on the grid anchored at the frame
/// origin.
pub(super) fn reduced_rect(rect: Rect, reduction: i64) -> Rect {
    if rect.is_empty() {
        return Rect::new(0, 0, 0, 0);
    }
    Rect::new(
        rect.x0.div_euclid(reduction),
        rect.y0.div_euclid(reduction),
        (rect.x1 - 1).div_euclid(reduction) + 1,
        (rect.y1 - 1).div_euclid(reduction) + 1,
    )
}

/// The full-resolution rectangle a reduced rectangle's blocks cover.
pub(super) fn full_rect(rect: Rect, reduction: i64) -> Rect {
    Rect::new(
        rect.x0 * reduction,
        rect.y0 * reduction,
        rect.x1 * reduction,
        rect.y1 * reduction,
    )
}

// ---------------------------------------------------------------------------------------------
// Scratch planes.
// ---------------------------------------------------------------------------------------------

/// A bump allocator over the scratch the host reserved for this tile. A unit takes its planes from
/// it in a fixed order, and [`Scratch::branch`] hands a nested allocator the memory that is still
/// free, so two sequential steps reuse the same bytes instead of each declaring their own.
pub(super) struct Scratch<'a> {
    free: &'a mut [f32],
}

impl<'a> Scratch<'a> {
    pub(super) fn new(values: &'a mut [f32]) -> Self {
        Self { free: values }
    }

    /// `len` values of scratch, or the `internal` error that says the unit asked for more than it
    /// declared. The host sizes the buffer from [`SpatialUnit::scratch_bytes`], so this is a
    /// disagreement between a unit's declaration and its own code, never a user-visible condition.
    ///
    /// [`SpatialUnit::scratch_bytes`]: crate::modules::SpatialUnit::scratch_bytes
    pub(super) fn take(&mut self, len: usize) -> Result<&'a mut [f32], Error> {
        let free = std::mem::take(&mut self.free);
        if free.len() < len {
            self.free = free;
            return Err(Error::internal(format!(
                "a presence unit asked for {len} scratch values with {} left of what it declared",
                self.free.len()
            )));
        }
        let (taken, rest) = free.split_at_mut(len);
        self.free = rest;
        Ok(taken)
    }

    /// An allocator over the memory still free here. Anything it hands out lives only as long as
    /// the borrow, so the next step reuses the same bytes.
    pub(super) fn branch(&mut self) -> Scratch<'_> {
        Scratch {
            free: &mut self.free[..],
        }
    }
}

/// A frame and the sub-rectangle of it a plane holds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Geometry {
    width: i64,
    height: i64,
    rect: Rect,
}

impl Geometry {
    pub(super) fn new(width: i64, height: i64, rect: Rect) -> Self {
        Self {
            width,
            height,
            rect,
        }
    }

    pub(super) fn frame(self) -> Rect {
        Rect::frame(self.width, self.height)
    }

    fn clamp(self, x: i64, y: i64) -> (i64, i64) {
        (x.clamp(0, self.width - 1), y.clamp(0, self.height - 1))
    }

    fn index(self, x: i64, y: i64) -> usize {
        debug_assert!(
            self.rect.contains(x, y),
            "a presence read of ({x}, {y}) is outside the {:?} the plane holds: the declared halo \
             is too small",
            self.rect
        );
        ((y - self.rect.y0) * self.rect.width() + (x - self.rect.x0)) as usize
    }

    /// Where frame column `x` sits in a held row. Not clamped: a pass asks only for columns inside
    /// the frame, and one outside the held rectangle fails the slice bounds.
    fn column(self, x: i64) -> usize {
        (x - self.rect.x0) as usize
    }
}

/// One scalar plane of a frame, holding only its rectangle of it.
#[derive(Clone, Copy)]
pub(super) struct Plane<'a> {
    geometry: Geometry,
    data: &'a [f32],
}

impl<'a> Plane<'a> {
    /// A plane over values held elsewhere: `data` holds `rect` of the frame `geometry` names,
    /// row-major, such as a reduced grid's held planes.
    pub(super) fn over(data: &'a [f32], geometry: Geometry, rect: Rect) -> Result<Self, Error> {
        let len = rect.pixels();
        if data.len() < len {
            return Err(Error::internal(format!(
                "a presence plane of {len} values was handed {}",
                data.len()
            )));
        }
        Ok(Self {
            geometry: Geometry::new(geometry.width, geometry.height, rect),
            data: &data[..len],
        })
    }

    pub(super) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Edge-clamped read, the host's own rule.
    #[inline]
    pub(super) fn get(&self, x: i64, y: i64) -> f32 {
        let (x, y) = self.geometry.clamp(x, y);
        self.data[self.geometry.index(x, y)]
    }

    /// The held row at frame row `y`, clamped to the frame as [`Self::get`] clamps it; its first
    /// value is the rectangle's `x0`, so frame column `x` is at `x - x0`. A row outside the held
    /// rectangle after clamping is a halo bug, which panics.
    #[inline]
    pub(super) fn row(&self, y: i64) -> &'a [f32] {
        let y = y.clamp(0, self.geometry.height - 1);
        let rect = self.geometry.rect;
        debug_assert!(
            y >= rect.y0 && y < rect.y1,
            "a presence read of row {y} is outside the {rect:?} the plane holds: the declared halo \
             is too small"
        );
        let width = rect.width() as usize;
        let start = (y - rect.y0) as usize * width;
        &self.data[start..start + width]
    }

    /// Frame columns `x0..x1` of row `y`: the row clamped as [`Self::row`] clamps it, the columns
    /// not, so they must lie inside the frame, where clamping changes nothing, and inside the held
    /// rectangle, or the slice bounds panic.
    #[inline]
    pub(super) fn span(&self, y: i64, x0: i64, x1: i64) -> &'a [f32] {
        &self.row(y)[self.geometry.column(x0)..self.geometry.column(x1)]
    }
}

/// One writable scalar plane over a slice of the tile's scratch.
pub(super) struct PlaneMut<'a> {
    geometry: Geometry,
    data: &'a mut [f32],
}

impl<'a> PlaneMut<'a> {
    /// A plane over the front of `buffer`, or the `internal` error that says the unit declared less
    /// scratch than it uses.
    pub(super) fn over(
        buffer: &'a mut [f32],
        geometry: Geometry,
        rect: Rect,
    ) -> Result<Self, Error> {
        let geometry = Geometry::new(geometry.width, geometry.height, rect);
        let len = rect.pixels();
        if buffer.len() < len {
            return Err(Error::internal(format!(
                "a presence plane of {len} values does not fit the {} values declared for it",
                buffer.len()
            )));
        }
        Ok(Self {
            geometry,
            data: &mut buffer[..len],
        })
    }

    pub(super) fn rect(&self) -> Rect {
        self.geometry.rect
    }

    pub(super) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Only the frozen reference passes write one value at a time.
    #[cfg(test)]
    pub(super) fn set(&mut self, x: i64, y: i64, value: f32) {
        let index = self.geometry.index(x, y);
        self.data[index] = value;
    }

    /// Every value of the rectangle, row-major: the frozen reference's pooled vertical pass cuts
    /// its strips from them.
    #[cfg(test)]
    pub(super) fn values_mut(&mut self) -> &mut [f32] {
        self.data
    }

    pub(super) fn as_plane(&self) -> Plane<'_> {
        Plane {
            geometry: self.geometry,
            data: self.data,
        }
    }

    /// Write the plane row by row: `body` receives a row's `y` and its values, which start at the
    /// rectangle's `x0`. On the pool under [`Parallelism::Pool`], in order otherwise.
    pub(super) fn for_rows(
        &mut self,
        parallelism: Parallelism,
        body: impl Fn(i64, &mut [f32]) + Sync + Send,
    ) {
        let rect = self.geometry.rect;
        if rect.is_empty() {
            return;
        }
        let (data, width) = (&mut self.data[..rect.pixels()], rect.width() as usize);
        match parallelism {
            Parallelism::Pool => data
                .par_chunks_mut(width)
                .enumerate()
                .for_each(|(row, values)| body(rect.y0 + row as i64, values)),
            Parallelism::Serial => data
                .chunks_mut(width)
                .enumerate()
                .for_each(|(row, values)| body(rect.y0 + row as i64, values)),
        }
    }
}

/// Write several planes over one rectangle row by row: `body` receives a row's `y` and that row of
/// each plane, which start at the rectangle's `x0`. On the pool under [`Parallelism::Pool`], in
/// order otherwise.
pub(super) fn for_rows_of<const N: usize>(
    parallelism: Parallelism,
    planes: [&mut PlaneMut<'_>; N],
    body: impl Fn(i64, &mut [&mut [f32]; N]) + Sync + Send,
) {
    let rect = planes[0].geometry.rect;
    debug_assert!(
        planes.iter().all(|plane| plane.geometry.rect == rect),
        "planes written row by row together hold the same rectangle"
    );
    if rect.is_empty() {
        return;
    }
    let width = rect.width() as usize;
    let mut chunks = planes.map(|plane| plane.data[..rect.pixels()].chunks_mut(width));
    let mut rows: Vec<[&mut [f32]; N]> = (0..rect.height())
        .map(|_| std::array::from_fn(|plane| chunks[plane].next().expect("a row of each plane")))
        .collect();
    match parallelism {
        Parallelism::Pool => rows
            .par_iter_mut()
            .enumerate()
            .for_each(|(row, values)| body(rect.y0 + row as i64, values)),
        Parallelism::Serial => rows
            .iter_mut()
            .enumerate()
            .for_each(|(row, values)| body(rect.y0 + row as i64, values)),
    }
}

/// Where stage columns `x0..x1` sit in a row of the unit's input ([`Planes::row`]), found once for
/// a pass that reads those columns of many rows, in place of one [`Planes::sample`] per pixel.
/// Every such pass reads columns inside the stage, where `sample`'s clamp changes nothing, which
/// this checks once; a column outside the input's rectangle fails the slice bounds, as `sample`
/// panics.
pub(super) fn input_columns(input: &Planes<'_>, x0: i64, x1: i64) -> Range<usize> {
    assert!(
        0 <= x0 && x0 <= x1 && x1 <= i64::from(input.stage().width),
        "a presence unit reads its input's columns {x0}..{x1} inside the stage"
    );
    let first = i64::from(input.region().x0);
    (x0 - first) as usize..(x1 - first) as usize
}

// ---------------------------------------------------------------------------------------------
// Box filters.
// ---------------------------------------------------------------------------------------------

/// The horizontal mean over `2r + 1` columns, as a running `f64` sum along each row seeded by a
/// direct sum at the row's first column. [`ROWS`] rows run together, each with its own sum taking
/// its own values in the same order, so the rows' add chains overlap and no value changes.
pub(super) fn horizontal_mean(
    src: &Plane<'_>,
    r: i64,
    dst: &mut PlaneMut<'_>,
    parallelism: Parallelism,
) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    let pass = Horizontal::new(src, r, out);
    let width = out.width() as usize;
    let group = |(index, block): (usize, &mut [f32])| {
        let y = out.y0 + (index * ROWS) as i64;
        let mut rows = block.chunks_exact_mut(width);
        if rows.len() == ROWS {
            pass.rows::<ROWS>(
                std::array::from_fn(|k| y + k as i64),
                std::array::from_fn(|_| rows.next().expect("a row of the group")),
            );
        } else {
            for (k, row) in rows.enumerate() {
                pass.rows::<1>([y + k as i64], [row]);
            }
        }
    };
    let data = &mut dst.data[..out.pixels()];
    match parallelism {
        Parallelism::Pool => data
            .par_chunks_mut(ROWS * width)
            .enumerate()
            .for_each(group),
        Parallelism::Serial => data.chunks_mut(ROWS * width).enumerate().for_each(group),
    }
}

/// One horizontal mean's rows, cut at the frame edges once for every row. Column `x` of the output
/// is the running sum's step that adds column `x + r` and drops column `x - 1 - r`, after a seed
/// that sums columns `out.x0 - r ..= out.x0 + r` directly.
struct Horizontal<'p, 'a> {
    src: &'p Plane<'a>,
    r: i64,
    n: f64,
    out: Rect,
    /// The seed's columns left of the frame, inside it and right of it.
    seed: [std::ops::Range<i64>; 3],
    /// The steps (output columns after the first) before, between and after the columns where
    /// both of a step's columns lie inside the frame.
    steps: [std::ops::Range<i64>; 3],
}

impl<'p, 'a> Horizontal<'p, 'a> {
    fn new(src: &'p Plane<'a>, r: i64, out: Rect) -> Self {
        let width = src.geometry.width;
        let (seed_start, seed_end) = (out.x0 - r, out.x0 + r + 1);
        let seed_inside = 0.clamp(seed_start, seed_end);
        let seed_right = width.clamp(seed_inside, seed_end);
        let first = out.x0 + 1;
        let inside = (r + 1).clamp(first, out.x1);
        let after = (width - r).clamp(inside, out.x1);
        Self {
            src,
            r,
            n: (2 * r + 1) as f64,
            out,
            seed: [
                seed_start..seed_inside,
                seed_inside..seed_right,
                seed_right..seed_end,
            ],
            steps: [first..inside, inside..after, after..out.x1],
        }
    }

    /// `K` rows at frame rows `ys`, written to `rows`, which start at `out.x0`. Each row's sum is
    /// `0.0` plus each seed column in order, then `+ (entering - leaving)` per step, exactly as one
    /// row alone; the columns where a read clamps go through [`Plane::get`], the rest read the row.
    fn rows<const K: usize>(&self, ys: [i64; K], mut rows: [&mut [f32]; K]) {
        let (src, r, n, out) = (self.src, self.r, self.n, self.out);
        let lines = ys.map(|y| src.row(y));
        let [left, inside, right] = &self.seed;
        let mut sums = [0.0_f64; K];
        for k in 0..K {
            let sum = &mut sums[k];
            for x in left.clone() {
                *sum += f64::from(src.get(x, ys[k]));
            }
            let columns = src.geometry.column(inside.start)..src.geometry.column(inside.end);
            for value in &lines[k][columns] {
                *sum += f64::from(*value);
            }
            for x in right.clone() {
                *sum += f64::from(src.get(x, ys[k]));
            }
            rows[k][0] = (*sum / n) as f32;
        }

        let [before, inside, after] = &self.steps;
        let edge = |x: i64, sums: &mut [f64; K], rows: &mut [&mut [f32]; K]| {
            for k in 0..K {
                sums[k] += f64::from(src.get(x + r, ys[k])) - f64::from(src.get(x - 1 - r, ys[k]));
                rows[k][(x - out.x0) as usize] = (sums[k] / n) as f32;
            }
        };
        for x in before.clone() {
            edge(x, &mut sums, &mut rows);
        }
        let len = (inside.end - inside.start).max(0) as usize;
        if len > 0 {
            let entering = src.geometry.column(inside.start + r);
            let leaving = src.geometry.column(inside.start - 1 - r);
            let entering = lines.map(|line| &line[entering..][..len]);
            let leaving = lines.map(|line| &line[leaving..][..len]);
            let first = (inside.start - out.x0) as usize;
            let written = rows.each_mut().map(|row| &mut row[first..][..len]);
            for i in 0..len {
                for k in 0..K {
                    sums[k] += f64::from(entering[k][i]) - f64::from(leaving[k][i]);
                    written[k][i] = (sums[k] / n) as f32;
                }
            }
        }
        for x in after.clone() {
            edge(x, &mut sums, &mut rows);
        }
    }
}

/// The vertical mean over `2r + 1` rows, in strips of [`STRIP`] columns so the reads run along rows
/// rather than down columns. One `f64` accumulator per column. The strips are independent, so under
/// [`Parallelism::Pool`] they run on the pool, each writing its own columns of every row.
pub(super) fn vertical_mean(
    src: &Plane<'_>,
    r: i64,
    dst: &mut PlaneMut<'_>,
    parallelism: Parallelism,
) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    // Only the rows clamp: every column read is an output column, which no clamp moves.
    assert!(
        out.x0 >= 0 && out.x1 <= src.geometry.width,
        "a vertical mean's columns {}..{} lie inside the frame",
        out.x0,
        out.x1
    );
    let n = (2 * r + 1) as f64;
    let width = out.width() as usize;
    let data = &mut dst.data[..out.pixels()];
    match parallelism {
        Parallelism::Serial => {
            let mut x0 = out.x0;
            while x0 < out.x1 {
                let columns = ((out.x1 - x0) as usize).min(STRIP);
                let first = (x0 - out.x0) as usize;
                vertical_strip(src, r, out, x0, columns, |row, sums| {
                    let start = row * width + first;
                    store(&mut data[start..start + columns], sums, n);
                });
                x0 += columns as i64;
            }
        }
        Parallelism::Pool => {
            // Each strip's own segment of every row, so the strips can be written concurrently.
            let mut strips: Vec<Vec<&mut [f32]>> = (0..width.div_ceil(STRIP))
                .map(|_| Vec::with_capacity(out.height() as usize))
                .collect();
            for row in data.chunks_mut(width) {
                for (strip, segment) in row.chunks_mut(STRIP).enumerate() {
                    strips[strip].push(segment);
                }
            }
            strips
                .par_iter_mut()
                .enumerate()
                .for_each(|(strip, segments)| {
                    let columns = segments[0].len();
                    let x0 = out.x0 + (strip * STRIP) as i64;
                    vertical_strip(src, r, out, x0, columns, |row, sums| {
                        store(segments[row], sums, n);
                    });
                });
        }
    }
}

/// One strip of the vertical mean: `columns` columns from `x0`, over every row of `out`, each with
/// its own `f64` running sum seeded by a direct sum at the rectangle's first row. `emit` receives
/// each row's sums (the row counted from `out.y0`). Each read is a row clamped to the frame, read
/// across the strip as one slice, so the independent column sums vectorize; the strip's columns lie
/// inside `out` and so inside the frame, where clamping changes nothing.
fn vertical_strip(
    src: &Plane<'_>,
    r: i64,
    out: Rect,
    x0: i64,
    columns: usize,
    mut emit: impl FnMut(usize, &[f64]),
) {
    let x1 = x0 + columns as i64;
    let mut accumulator = [0.0_f64; STRIP];
    let sums = &mut accumulator[..columns];
    for dy in -r..=r {
        for (sum, value) in sums.iter_mut().zip(src.span(out.y0 + dy, x0, x1)) {
            *sum += f64::from(*value);
        }
    }
    emit(0, sums);
    for (row, y) in ((out.y0 + 1)..out.y1).enumerate() {
        let entering = src.span(y + r, x0, x1);
        let leaving = src.span(y - 1 - r, x0, x1);
        for ((sum, entering), leaving) in sums.iter_mut().zip(entering).zip(leaving) {
            *sum += f64::from(*entering) - f64::from(*leaving);
        }
        emit(row + 1, sums);
    }
}

/// A row of a mean's sums divided by the window's size into its values.
fn store(values: &mut [f32], sums: &[f64], n: f64) {
    for (value, sum) in values.iter_mut().zip(sums) {
        *value = (*sum / n) as f32;
    }
}

/// The box mean over a `(2r+1)^2` window, evaluated separably: the horizontal mean first, then the
/// vertical mean of that. Halo: `r`. `temp` is the horizontal pass's plane and must hold the
/// output rectangle grown by `r` along y.
pub(super) fn box_mean(
    src: &Plane<'_>,
    r: i64,
    dst: &mut PlaneMut<'_>,
    temp: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let mid = dst.rect().expand_y(r).clip(geometry.frame());
    let mut horizontal = PlaneMut::over(temp, geometry, mid)?;
    horizontal_mean(src, r, &mut horizontal, parallelism);
    vertical_mean(&horizontal.as_plane(), r, dst, parallelism);
    Ok(())
}

/// The minimum over a `(2r+1)^2` window, evaluated separably in the same order. Halo: `r`. A
/// minimum has no running form, so both passes read their whole window; the one unit that uses it
/// does so on its 4x reduced grid, where the radius is a handful of pixels.
pub(super) fn box_min(
    src: &Plane<'_>,
    r: i64,
    dst: &mut PlaneMut<'_>,
    temp: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let mid = dst.rect().expand_y(r).clip(geometry.frame());
    let mut horizontal = PlaneMut::over(temp, geometry, mid)?;
    horizontal.for_rows(parallelism, |y, row| {
        for x in mid.x0..mid.x1 {
            let mut value = f32::INFINITY;
            for dx in -r..=r {
                value = value.min(src.get(x + dx, y));
            }
            row[(x - mid.x0) as usize] = value;
        }
    });
    let source = horizontal.as_plane();
    let out = dst.rect();
    dst.for_rows(parallelism, |y, row| {
        for x in out.x0..out.x1 {
            let mut value = f32::INFINITY;
            for dy in -r..=r {
                value = value.min(source.get(x, y + dy));
            }
            row[(x - out.x0) as usize] = value;
        }
    });
    Ok(())
}

/// How many scratch values one [`guided_self`] call takes for an input rectangle of `pixels`
/// pixels: the squared plane, the two means, the second coefficient mean and the box mean's
/// horizontal temporary.
pub(super) const GUIDED_SELF_PLANES: usize = 5;

/// The self-guided edge-preserving smoother (`G = I`), where `cov = var`, so `a = var/(var+eps)`
/// and `b = (1-a)*mean` need no second set of box passes:
///
/// ```text
/// var = max(0, mean(I*I) - mean(I)^2)      a = var / (var + eps)      b = (1 - a) * mean(I)
/// q   = mean(a) * I + mean(b)
/// ```
///
/// Two sequential box passes of radius `r`, so the halo is `2r`. `var` is floored at zero because
/// `mean(I*I) - mean(I)^2` can be a very small negative number in floating point on a constant
/// window; the floor never changes a mathematically positive variance.
pub(super) fn guided_self(
    src: &Plane<'_>,
    r: i64,
    eps: f32,
    dst: &mut PlaneMut<'_>,
    scratch: &mut Scratch<'_>,
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let frame = geometry.frame();
    let out = dst.rect();
    if out.is_empty() {
        return Ok(());
    }
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let mut scratch = scratch.branch();
    let squared_buffer = scratch.take(source.pixels())?;
    let mean_buffer = scratch.take(inner.pixels())?;
    let mean_squared_buffer = scratch.take(inner.pixels())?;
    let coefficient_buffer = scratch.take(out.pixels())?;
    let temp_buffer = scratch.take(inner.expand_y(r).clip(frame).pixels())?;

    // Every pointwise loop here runs over a rectangle inside the frame (`source` and `inner` are
    // clipped to it, and the box mean into `dst` asserts `out` lies in it), where no read clamps,
    // so each reads its planes' rows as slices.
    let mut squared = PlaneMut::over(squared_buffer, geometry, source)?;
    squared.for_rows(parallelism, |y, row| {
        for (squared, value) in row.iter_mut().zip(src.span(y, source.x0, source.x1)) {
            *squared = value * value;
        }
    });

    let mut mean = PlaneMut::over(mean_buffer, geometry, inner)?;
    box_mean(src, r, &mut mean, temp_buffer, parallelism)?;
    let mut mean_squared = PlaneMut::over(mean_squared_buffer, geometry, inner)?;
    box_mean(
        &squared.as_plane(),
        r,
        &mut mean_squared,
        temp_buffer,
        parallelism,
    )?;

    // `a` replaces `mean(I*I)` and `b` replaces `mean(I)`: both are read once, at this pixel.
    for_rows_of(parallelism, [&mut mean, &mut mean_squared], |_, rows| {
        let [mean, mean_squared] = rows;
        for column in 0..mean.len() {
            let m = mean[column];
            let variance = (mean_squared[column] - m * m).max(0.0);
            let a = variance / (variance + eps);
            mean_squared[column] = a;
            mean[column] = (1.0 - a) * m;
        }
    });

    // `mean(b)` goes straight into the result and `mean(a)` into the one remaining plane, so the
    // combination below needs no third buffer.
    box_mean(&mean.as_plane(), r, dst, temp_buffer, parallelism)?;
    let mut mean_a = PlaneMut::over(coefficient_buffer, geometry, out)?;
    box_mean(
        &mean_squared.as_plane(),
        r,
        &mut mean_a,
        temp_buffer,
        parallelism,
    )?;
    let mean_a = mean_a.as_plane();
    dst.for_rows(parallelism, |y, row| {
        let (mean_a, src) = (mean_a.span(y, out.x0, out.x1), src.span(y, out.x0, out.x1));
        for ((value, a), i) in row.iter_mut().zip(mean_a).zip(src) {
            *value += a * i;
        }
    });
    Ok(())
}

/// How many scratch values one [`guided_filter`] call takes for an input rectangle of `pixels`
/// pixels.
pub(super) const GUIDED_PLANES: usize = 8;

/// The box-based guided filter of `input` under `guide` (He, Sun and Tang):
///
/// ```text
/// var = max(0, mean(G*G) - mean(G)^2)       cov = mean(G*I) - mean(G)*mean(I)
/// a   = cov / (var + eps)                   b   = mean(I) - a * mean(G)
/// q   = mean(a) * G + mean(b)
/// ```
///
/// Two sequential box passes of radius `r`, so the halo is `2r`.
pub(super) fn guided_filter(
    guide: &Plane<'_>,
    input: &Plane<'_>,
    r: i64,
    eps: f32,
    dst: &mut PlaneMut<'_>,
    scratch: &mut Scratch<'_>,
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let frame = geometry.frame();
    let out = dst.rect();
    if out.is_empty() {
        return Ok(());
    }
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let mut scratch = scratch.branch();
    let guide_squared_buffer = scratch.take(source.pixels())?;
    let guide_input_buffer = scratch.take(source.pixels())?;
    let mean_guide_buffer = scratch.take(inner.pixels())?;
    let mean_input_buffer = scratch.take(inner.pixels())?;
    let mean_guide_squared_buffer = scratch.take(inner.pixels())?;
    let mean_guide_input_buffer = scratch.take(inner.pixels())?;
    let coefficient_buffer = scratch.take(out.pixels())?;
    let temp_buffer = scratch.take(inner.expand_y(r).clip(frame).pixels())?;

    // As in `guided_self`, every pointwise loop runs over a rectangle inside the frame and reads its
    // planes' rows as slices.
    let mut guide_squared = PlaneMut::over(guide_squared_buffer, geometry, source)?;
    let mut guide_input = PlaneMut::over(guide_input_buffer, geometry, source)?;
    for_rows_of(
        parallelism,
        [&mut guide_squared, &mut guide_input],
        |y, rows| {
            let [guide_squared, guide_input] = rows;
            let guide = guide.span(y, source.x0, source.x1);
            let input = input.span(y, source.x0, source.x1);
            for (((squared, product), g), i) in guide_squared
                .iter_mut()
                .zip(guide_input.iter_mut())
                .zip(guide)
                .zip(input)
            {
                *squared = g * g;
                *product = g * i;
            }
        },
    );

    let mut mean_guide = PlaneMut::over(mean_guide_buffer, geometry, inner)?;
    box_mean(guide, r, &mut mean_guide, temp_buffer, parallelism)?;
    let mut mean_input = PlaneMut::over(mean_input_buffer, geometry, inner)?;
    box_mean(input, r, &mut mean_input, temp_buffer, parallelism)?;
    let mut mean_guide_squared = PlaneMut::over(mean_guide_squared_buffer, geometry, inner)?;
    box_mean(
        &guide_squared.as_plane(),
        r,
        &mut mean_guide_squared,
        temp_buffer,
        parallelism,
    )?;
    let mut mean_guide_input = PlaneMut::over(mean_guide_input_buffer, geometry, inner)?;
    box_mean(
        &guide_input.as_plane(),
        r,
        &mut mean_guide_input,
        temp_buffer,
        parallelism,
    )?;

    // `a` replaces `mean(G*I)` and `b` replaces `mean(I)`.
    let (mean_guide, mean_guide_squared) = (mean_guide.as_plane(), mean_guide_squared.as_plane());
    for_rows_of(
        parallelism,
        [&mut mean_guide_input, &mut mean_input],
        |y, rows| {
            let [mean_guide_input, mean_input] = rows;
            let mean_guide = mean_guide.span(y, inner.x0, inner.x1);
            let mean_guide_squared = mean_guide_squared.span(y, inner.x0, inner.x1);
            for (((mean_guide_input, mean_input), &mg), &mean_guide_squared) in mean_guide_input
                .iter_mut()
                .zip(mean_input.iter_mut())
                .zip(mean_guide)
                .zip(mean_guide_squared)
            {
                let mi = *mean_input;
                let variance = (mean_guide_squared - mg * mg).max(0.0);
                let covariance = *mean_guide_input - mg * mi;
                let a = covariance / (variance + eps);
                *mean_guide_input = a;
                *mean_input = mi - a * mg;
            }
        },
    );

    box_mean(&mean_input.as_plane(), r, dst, temp_buffer, parallelism)?;
    let mut mean_a = PlaneMut::over(coefficient_buffer, geometry, out)?;
    box_mean(
        &mean_guide_input.as_plane(),
        r,
        &mut mean_a,
        temp_buffer,
        parallelism,
    )?;
    let mean_a = mean_a.as_plane();
    dst.for_rows(parallelism, |y, row| {
        let (mean_a, guide) = (
            mean_a.span(y, out.x0, out.x1),
            guide.span(y, out.x0, out.x1),
        );
        for ((value, a), g) in row.iter_mut().zip(mean_a).zip(guide) {
            *value += a * g;
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reduction and upsample.
// ---------------------------------------------------------------------------------------------

/// Box-average downsample by an integer factor: reduced pixel `(i, j)` is the mean of the full
/// pixels in `[i*s, min((i+1)*s, width)) x [j*s, min((j+1)*s, height))`, so a partial block at the
/// right or bottom edge is averaged over its actual pixels and never over clamped copies. The block
/// grid is anchored at the *stage* origin, which is what makes a tile's reduced pixels identical to
/// the whole frame's.
pub(super) fn downsample(
    src: &Plane<'_>,
    reduction: i64,
    dst: &mut PlaneMut<'_>,
    parallelism: Parallelism,
) {
    let full = src.geometry().frame();
    let out = dst.rect();
    dst.for_rows(parallelism, |j, row| {
        let y0 = j * reduction;
        let y1 = ((j + 1) * reduction).min(full.y1);
        for i in out.x0..out.x1 {
            let x0 = i * reduction;
            let x1 = ((i + 1) * reduction).min(full.x1);
            let mut sum = 0.0_f64;
            let mut count = 0.0_f64;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += f64::from(src.get(x, y));
                    count += 1.0;
                }
            }
            row[(i - out.x0) as usize] = (sum / count) as f32;
        }
    });
}

/// Bilinear upsample from a reduced plane back to full resolution. A full pixel `x` samples the
/// reduced coordinate `u = (x + 0.5)/s - 0.5`, blending reduced indices `floor(u)` and
/// `floor(u) + 1`, each clamped to the reduced frame, so a full pixel reaches at most one reduced
/// index beyond its own block.
///
/// Each output column's reduced index and weights are computed once per call, not once per pixel,
/// into a table as wide as the output rectangle (16 bytes a column, so tens of KiB at most for the
/// widest tile rectangle; it is a small heap allocation of its own, not part of the tile's
/// declared scratch). The columns whose two reduced indices both lie inside the reduced frame read
/// them from the two reduced rows as slices, one pair per reduced index; the columns at the frame
/// edges, where an index clamps, read through [`Plane::get`]. Every value is the same products and
/// sums of the same weights in the same order.
pub(super) fn upsample(
    reduced: &Plane<'_>,
    reduction: i64,
    dst: &mut PlaneMut<'_>,
    parallelism: Parallelism,
) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    let s = reduction as f32;
    let columns: Vec<Sample> = (out.x0..out.x1)
        .map(|x| {
            let u = ((x as f32) + 0.5) / s - 0.5;
            let i0 = u.floor();
            let fx = u - i0;
            Sample {
                i0: i0 as i64,
                fx,
                gx: 1.0 - fx,
            }
        })
        .collect();
    // The interior: the run of columns whose indices `i0` and `i0 + 1` need no clamp. `i0` never
    // decreases along a row, so it is one run; it ends early, leaving the rest to the clamped
    // reads, should it ever decrease. `runs[k]` counts its columns whose `i0` is `first + k`.
    let reduced_width = reduced.geometry.width;
    let inside = |sample: &Sample| sample.i0 >= 0 && sample.i0 + 1 < reduced_width;
    let start = columns.iter().position(inside).unwrap_or(columns.len());
    let mut end = start;
    while end < columns.len()
        && inside(&columns[end])
        && (end == start || columns[end].i0 >= columns[end - 1].i0)
    {
        end += 1;
    }
    let (first, runs) = if start < end {
        let first = columns[start].i0;
        let mut runs = vec![0_usize; (columns[end - 1].i0 - first + 1) as usize];
        for sample in &columns[start..end] {
            runs[(sample.i0 - first) as usize] += 1;
        }
        (reduced.geometry.column(first), runs)
    } else {
        (0, Vec::new())
    };
    dst.for_rows(parallelism, |y, row| {
        let v = ((y as f32) + 0.5) / s - 0.5;
        let j0 = v.floor();
        let fy = v - j0;
        let gy = 1.0 - fy;
        let j0 = j0 as i64;
        let clamped = |sample: &Sample| {
            let (i0, fx, gx) = (sample.i0, sample.fx, sample.gx);
            let top = reduced.get(i0, j0) * gx + reduced.get(i0 + 1, j0) * fx;
            let bottom = reduced.get(i0, j0 + 1) * gx + reduced.get(i0 + 1, j0 + 1) * fx;
            top * gy + bottom * fy
        };
        let (head, rest) = row.split_at_mut(start);
        let (body, tail) = rest.split_at_mut(end - start);
        for (value, sample) in head.iter_mut().zip(&columns[..start]) {
            *value = clamped(sample);
        }
        for (value, sample) in tail.iter_mut().zip(&columns[end..]) {
            *value = clamped(sample);
        }
        if runs.is_empty() {
            return;
        }
        let count = runs.len();
        // The interior's reduced indices, from its first column's `i0` to one past its last
        // column's, in rows `j0` and `j0 + 1` (the rows clamped to the frame as `get` clamps
        // them): run `k` blends the values at `k` and `k + 1` of the two.
        let upper = &reduced.row(j0)[first..first + count + 1];
        let lower = &reduced.row(j0 + 1)[first..first + count + 1];
        let mut pixels = body.iter_mut().zip(&columns[start..end]);
        for ((((&run, &a), &b), &c), &d) in runs
            .iter()
            .zip(&upper[..count])
            .zip(&upper[1..])
            .zip(&lower[..count])
            .zip(&lower[1..])
        {
            for (value, sample) in pixels.by_ref().take(run) {
                let top = a * sample.gx + b * sample.fx;
                let bottom = c * sample.gx + d * sample.fx;
                *value = top * gy + bottom * fy;
            }
        }
    });
}

/// Where one output column of [`upsample`] samples the reduced grid: the reduced index its sample
/// falls after, unclamped, and the weights of that index's right neighbour (`fx`) and of the index
/// itself (`gx = 1 - fx`).
struct Sample {
    i0: i64,
    fx: f32,
    gx: f32,
}

// ---------------------------------------------------------------------------------------------
// Held reduced planes.
// ---------------------------------------------------------------------------------------------

/// A reduced rectangle as the host's region of the grid.
pub(super) fn grid_region(rect: Rect) -> Region {
    if rect.is_empty() {
        return Region::EMPTY;
    }
    Region {
        x0: rect.x0 as u32,
        y0: rect.y0 as u32,
        width: rect.width() as u32,
        height: rect.height() as u32,
    }
}

/// Plane `index` of held reduced planes as a plane of the reduced frame `geometry` names, after
/// checking that they are planes of that frame and hold every cell of `reach`: what the host
/// vouched for when it handed them over, checked once rather than trusted.
pub(super) fn held_plane<'a>(
    planes: &GridPlanes<'a>,
    index: usize,
    geometry: Geometry,
    reach: Rect,
) -> Result<Plane<'a>, Error> {
    let frame = geometry.frame();
    let grid = planes.grid();
    let rect = Rect::of(planes.rect());
    let holds = reach.is_empty()
        || (reach.x0 >= rect.x0
            && reach.y0 >= rect.y0
            && reach.x1 <= rect.x1
            && reach.y1 <= rect.y1);
    if (i64::from(grid.width), i64::from(grid.height)) != (frame.x1, frame.y1) || !holds {
        return Err(Error::internal(format!(
            "held reduced planes of {}x{} cells over {rect:?} cannot serve {reach:?} of a {}x{} \
             grid",
            grid.width, grid.height, frame.x1, frame.y1
        )));
    }
    Plane::over(planes.plane(index), geometry, rect)
}

/// Copy the cells a tile hands back out of `plane`, a plane of the reduced grid this tile computed
/// over a rectangle that holds them, into plane `index` of `cells`.
pub(super) fn hand_back(cells: &mut Cells, index: usize, plane: &Plane<'_>) {
    let rect = Rect::of(cells.rect());
    if rect.is_empty() {
        return;
    }
    let held = plane.geometry.rect;
    assert!(
        rect.x0 >= held.x0 && rect.y0 >= held.y0 && rect.x1 <= held.x1 && rect.y1 <= held.y1,
        "a tile hands back cells {rect:?} it did not compute: it computed {held:?}"
    );
    let width = rect.width() as usize;
    let values = cells.plane_mut(index);
    for (row, y) in (rect.y0..rect.y1).enumerate() {
        values[row * width..(row + 1) * width].copy_from_slice(plane.span(y, rect.x0, rect.x1));
    }
}

// ---------------------------------------------------------------------------------------------
// Scratch accounting.
// ---------------------------------------------------------------------------------------------

/// The bytes a number of `f32` scratch values occupies.
pub(super) fn scratch_bytes(values: u64) -> u64 {
    values.saturating_mul(std::mem::size_of::<f32>() as u64)
}

/// How many pixels an input region holds: the bound every full-resolution plane a unit takes is
/// sized against, because every one of them lies inside the input region.
pub(super) fn region_values(region: Stage) -> u64 {
    u64::from(region.width) * u64::from(region.height)
}

/// How many pixels a reduced plane over an input region can hold: the region's reduced rectangle,
/// with one block of slack on each axis for a region that does not start on a block boundary.
pub(super) fn reduced_values(region: Stage, reduction: u32) -> u64 {
    u64::from(region.width.div_ceil(reduction) + 2)
        * u64::from(region.height.div_ceil(reduction) + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(width: i64, height: i64, value: f32) -> (Geometry, Vec<f32>) {
        (
            Geometry::new(width, height, Rect::frame(width, height)),
            vec![value; (width * height) as usize],
        )
    }

    #[test]
    fn a_box_mean_of_a_constant_is_that_constant() {
        let (geometry, mut data) = constant(16, 16, 0.375);
        let src = PlaneMut::over(&mut data, geometry, geometry.rect).unwrap();
        let mut out = vec![0.0; 16 * 16];
        let mut temp = vec![0.0; 16 * 16];
        let mut dst = PlaneMut::over(&mut out, geometry, geometry.rect).unwrap();
        box_mean(&src.as_plane(), 3, &mut dst, &mut temp, Parallelism::Serial).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                assert!(
                    (dst.as_plane().get(x, y) - 0.375).abs() < 1e-6,
                    "at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn a_partial_block_averages_over_its_actual_pixels() {
        // 6 columns reduced by 4: block 1 holds columns 4 and 5 only.
        let geometry = Geometry::new(6, 1, Rect::frame(6, 1));
        let mut values: Vec<f32> = (0..6).map(|x| x as f32).collect();
        let src = PlaneMut::over(&mut values, geometry, geometry.rect).unwrap();
        let reduced_geometry = Geometry::new(2, 1, Rect::frame(2, 1));
        let mut reduced = vec![0.0; 2];
        let mut dst =
            PlaneMut::over(&mut reduced, reduced_geometry, reduced_geometry.rect).unwrap();
        downsample(&src.as_plane(), 4, &mut dst, Parallelism::Serial);
        assert!((dst.as_plane().get(0, 0) - 1.5).abs() < 1e-6);
        assert!((dst.as_plane().get(1, 0) - 4.5).abs() < 1e-6);
    }

    #[test]
    fn soft_clip_is_zero_at_zero_bounded_and_transparent_outside_the_unit_range() {
        assert_eq!(soft_clip(0.0, 0.5, 0.10), 0.0);
        assert_eq!(soft_clip(5.0, 1.2, 0.15), 0.0);
        assert_eq!(soft_clip(-5.0, -0.2, 0.15), 0.0);
        for encoded in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            for step in -40..=40 {
                let raw = step as f32 / 10.0;
                let delta = soft_clip(raw, encoded, 0.15);
                assert!(delta.abs() <= 0.15);
                let out = encoded + delta;
                assert!(
                    (-1e-6..=1.0 + 1e-6).contains(&out),
                    "encoded {encoded} raw {raw} left [0, 1]: {out}"
                );
            }
        }
    }

    #[test]
    fn the_radius_rules_match_the_study_table() {
        assert_eq!(scaled_radius(96.0, 6000), 96);
        assert_eq!(scaled_radius(96.0, 480), 8);
        assert_eq!(scaled_radius(96.0, 16384), 160, "the scale is capped");
        assert_eq!(scaled_radius(1.0, 480), 1, "a radius is never below 1");
        assert_eq!(reduced_radius(96, 4), 24);
        assert_eq!(reduced_radius(2, 4), 1);
        assert_eq!(reduced_halo(2 * 24, 4), 199);
    }
}
