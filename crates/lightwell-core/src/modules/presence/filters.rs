//! The finite-support machinery the three Presence units share: rectangles, scratch planes, the
//! box mean, the box minimum, the two guided filters, the integer reduction and bilinear upsample,
//! the radius and halo rules, and the compressive gain.
//!
//! This file is the `f32` production transcription of `docs/design/presence-study.md` and of the
//! independent `f64` reference at `crates/lightwell-core/tests/reference/presence.rs`; the three
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
//! debug build and is caught by the slice bounds otherwise.

use crate::{
    Error, ErrorKind,
    colour::{luma, srgb},
    modules::{Parallelism, Stage},
};
use rayon::prelude::*;

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

fn internal(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Internal, detail)
}

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
            return Err(internal(format!(
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
}

/// One scalar plane of a frame, holding only its rectangle of it.
#[derive(Clone, Copy)]
pub(super) struct Plane<'a> {
    geometry: Geometry,
    data: &'a [f32],
}

impl<'a> Plane<'a> {
    pub(super) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Edge-clamped read, the host's own rule.
    #[inline]
    pub(super) fn get(&self, x: i64, y: i64) -> f32 {
        let (x, y) = self.geometry.clamp(x, y);
        self.data[self.geometry.index(x, y)]
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
            return Err(internal(format!(
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

    pub(super) fn set(&mut self, x: i64, y: i64, value: f32) {
        let index = self.geometry.index(x, y);
        self.data[index] = value;
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

// ---------------------------------------------------------------------------------------------
// Box filters.
// ---------------------------------------------------------------------------------------------

/// The horizontal mean over `2r + 1` columns, as a running `f64` sum along each row seeded by a
/// direct sum at the row's first column.
fn horizontal_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    let n = (2 * r + 1) as f64;
    dst.for_rows(parallelism, |y, row| {
        let mut sum = 0.0_f64;
        for dx in -r..=r {
            sum += f64::from(src.get(out.x0 + dx, y));
        }
        row[0] = (sum / n) as f32;
        for x in (out.x0 + 1)..out.x1 {
            sum += f64::from(src.get(x + r, y)) - f64::from(src.get(x - 1 - r, y));
            row[(x - out.x0) as usize] = (sum / n) as f32;
        }
    });
}

/// The vertical mean over `2r + 1` rows, in strips of [`STRIP`] columns so the reads run along rows
/// rather than down columns. One `f64` accumulator per column. The strips are independent, so under
/// [`Parallelism::Pool`] they run on the pool, each writing its own columns of every row.
fn vertical_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    match parallelism {
        Parallelism::Serial => {
            let mut x0 = out.x0;
            while x0 < out.x1 {
                let columns = ((out.x1 - x0) as usize).min(STRIP);
                vertical_strip(src, r, out, x0, columns, |row, column, value| {
                    dst.set(x0 + column as i64, out.y0 + row as i64, value);
                });
                x0 += columns as i64;
            }
        }
        Parallelism::Pool => {
            // Each strip's own segment of every row, so the strips can be written concurrently.
            let width = out.width() as usize;
            let mut strips: Vec<Vec<&mut [f32]>> = (0..width.div_ceil(STRIP))
                .map(|_| Vec::with_capacity(out.height() as usize))
                .collect();
            for row in dst.data[..out.pixels()].chunks_mut(width) {
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
                    vertical_strip(src, r, out, x0, columns, |row, column, value| {
                        segments[row][column] = value;
                    });
                });
        }
    }
}

/// One strip of the vertical mean: `columns` columns from `x0`, over every row of `out`, each with
/// its own `f64` running sum seeded by a direct sum at the rectangle's first row. `write` places
/// the value of a row (counted from `out.y0`) and column (counted from `x0`).
fn vertical_strip(
    src: &Plane<'_>,
    r: i64,
    out: Rect,
    x0: i64,
    columns: usize,
    mut write: impl FnMut(usize, usize, f32),
) {
    let n = (2 * r + 1) as f64;
    let mut accumulator = [0.0_f64; STRIP];
    for dy in -r..=r {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            *slot += f64::from(src.get(x0 + column as i64, out.y0 + dy));
        }
    }
    for (column, slot) in accumulator.iter().take(columns).enumerate() {
        write(0, column, (*slot / n) as f32);
    }
    for (row, y) in ((out.y0 + 1)..out.y1).enumerate() {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            let x = x0 + column as i64;
            *slot += f64::from(src.get(x, y + r)) - f64::from(src.get(x, y - 1 - r));
        }
        for (column, slot) in accumulator.iter().take(columns).enumerate() {
            write(row + 1, column, (*slot / n) as f32);
        }
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

    let mut squared = PlaneMut::over(squared_buffer, geometry, source)?;
    squared.for_rows(parallelism, |y, row| {
        for x in source.x0..source.x1 {
            let value = src.get(x, y);
            row[(x - source.x0) as usize] = value * value;
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
        for x in out.x0..out.x1 {
            let column = (x - out.x0) as usize;
            row[column] += mean_a.get(x, y) * src.get(x, y);
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

    let mut guide_squared = PlaneMut::over(guide_squared_buffer, geometry, source)?;
    let mut guide_input = PlaneMut::over(guide_input_buffer, geometry, source)?;
    for_rows_of(
        parallelism,
        [&mut guide_squared, &mut guide_input],
        |y, rows| {
            let [guide_squared, guide_input] = rows;
            for x in source.x0..source.x1 {
                let column = (x - source.x0) as usize;
                let g = guide.get(x, y);
                guide_squared[column] = g * g;
                guide_input[column] = g * input.get(x, y);
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
            for x in inner.x0..inner.x1 {
                let column = (x - inner.x0) as usize;
                let mg = mean_guide.get(x, y);
                let mi = mean_input[column];
                let variance = (mean_guide_squared.get(x, y) - mg * mg).max(0.0);
                let covariance = mean_guide_input[column] - mg * mi;
                let a = covariance / (variance + eps);
                mean_guide_input[column] = a;
                mean_input[column] = mi - a * mg;
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
        for x in out.x0..out.x1 {
            let column = (x - out.x0) as usize;
            row[column] += mean_a.get(x, y) * guide.get(x, y);
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
pub(super) fn upsample(
    reduced: &Plane<'_>,
    reduction: i64,
    dst: &mut PlaneMut<'_>,
    parallelism: Parallelism,
) {
    let out = dst.rect();
    let s = reduction as f32;
    dst.for_rows(parallelism, |y, row| {
        let v = ((y as f32) + 0.5) / s - 0.5;
        let j0 = v.floor();
        let fy = v - j0;
        let j0 = j0 as i64;
        for x in out.x0..out.x1 {
            let u = ((x as f32) + 0.5) / s - 0.5;
            let i0 = u.floor();
            let fx = u - i0;
            let i0 = i0 as i64;
            let top = reduced.get(i0, j0) * (1.0 - fx) + reduced.get(i0 + 1, j0) * fx;
            let bottom = reduced.get(i0, j0 + 1) * (1.0 - fx) + reduced.get(i0 + 1, j0 + 1) * fx;
            row[(x - out.x0) as usize] = top * (1.0 - fy) + bottom * fy;
        }
    });
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
