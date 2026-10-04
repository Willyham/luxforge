//! Bounded, fixed-order Detail passes. Planes borrow the tile's declared scratch.

use crate::{
    Cancel, Error,
    colour::oklab,
    modules::{Parallelism, Planes, PlanesMut, Region, SamplingScale, Stage},
};
use rayon::prelude::*;
use std::ops::Range;

pub(super) const NEUTRAL_CHROMA_SNAP: f32 = 1e-6;
pub(super) const DENOISE_EXACT_HALO_MAX: u32 = 32;
pub(super) const DENOISE_SAMPLED_HALO_MAX: u32 = 46;
pub(super) const SHARPEN_HALO_MAX: u32 = 16;

#[cfg(test)]
thread_local! {
    static CANCEL_AFTER_LEVEL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FINISHED_LEVELS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// A deterministic gate on the rendering thread, which runs small stages serially. It proves
// cancellation reaches the real unit while the first tile still holds its reservation.
#[cfg(test)]
pub(super) fn cancel_after_levels(levels: Option<usize>) {
    CANCEL_AFTER_LEVEL.set(levels);
    FINISHED_LEVELS.set(0);
}

#[cfg(test)]
pub(super) fn finished_level(cancel: &Cancel) {
    if let Some(remaining) = CANCEL_AFTER_LEVEL.get() {
        FINISHED_LEVELS.set(FINISHED_LEVELS.get() + 1);
        CANCEL_AFTER_LEVEL.set(Some(remaining.saturating_sub(1)));
        if remaining == 1 {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
pub(super) fn finished_levels() -> usize {
    FINISHED_LEVELS.get()
}

#[derive(Clone, Debug)]
pub(super) struct Kernel {
    pub taps: Vec<(i64, f32)>,
    pub radius: u32,
}
impl Kernel {
    pub fn b3(spacing: u32) -> Self {
        Self {
            taps: [
                (-2, 1.0 / 16.0),
                (-1, 4.0 / 16.0),
                (1, 4.0 / 16.0),
                (2, 1.0 / 16.0),
            ]
            .into_iter()
            .map(|(i, w)| (i * i64::from(spacing), w))
            .collect(),
            radius: 2 * spacing,
        }
    }
    pub fn gaussian(sigma: f64) -> Self {
        let radius = (3.0 * sigma).ceil().max(1.0) as u32;
        let variance = sigma * sigma;
        let weights: Vec<f64> = (-i64::from(radius)..=i64::from(radius))
            .map(|i| {
                if variance == 0.0 {
                    f64::from(i == 0)
                } else {
                    (-(i as f64).powi(2) / (2.0 * variance)).exp()
                }
            })
            .collect();
        let sum: f64 = weights.iter().sum();
        Self {
            taps: (-i64::from(radius)..=i64::from(radius))
                .filter(|i| *i != 0)
                .map(|i| (i, (weights[(i + i64::from(radius)) as usize] / sum) as f32))
                .collect(),
            radius,
        }
    }
    /// Whether `other` smooths exactly as this kernel does: the same radius and the same taps, their
    /// weights bit for bit.
    pub fn same(&self, other: &Self) -> bool {
        self.radius == other.radius
            && self.taps.len() == other.taps.len()
            && self
                .taps
                .iter()
                .zip(&other.taps)
                .all(|(a, b)| a.0 == b.0 && a.1.to_bits() == b.1.to_bits())
    }
}

pub(super) fn denoise_kernels(scale: SamplingScale) -> Vec<[Kernel; 2]> {
    (0..4)
        .map(|j| {
            if scale.x == 1.0 && scale.y == 1.0 {
                [Kernel::b3(1 << j), Kernel::b3(1 << j)]
            } else {
                [
                    Kernel::gaussian((1 << j) as f64 * scale.x),
                    Kernel::gaussian((1 << j) as f64 * scale.y),
                ]
            }
        })
        .collect()
}

pub(super) fn scratch_bytes(region: Stage, planes: u64) -> u64 {
    u64::from(region.width)
        .checked_mul(u64::from(region.height))
        .and_then(|v| v.checked_mul(planes))
        .and_then(|v| v.checked_mul(4))
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    pub stage: Stage,
    pub held: Region,
}
impl Geometry {
    pub fn index(self, x: i64, y: i64) -> usize {
        let x = x.clamp(0, i64::from(self.stage.width) - 1) as u32;
        let y = y.clamp(0, i64::from(self.stage.height) - 1) as u32;
        assert!(
            x >= self.held.x0 && x < self.held.x1() && y >= self.held.y0 && y < self.held.y1(),
            "Detail read outside its declared halo"
        );
        ((y - self.held.y0) as usize) * self.held.width as usize + (x - self.held.x0) as usize
    }
    pub fn sample(self, plane: &[f32], x: i64, y: i64) -> f32 {
        plane[self.index(x, y)]
    }
    /// The held row of `plane` at stage row `y`, clamped to the stage as [`Self::index`] clamps. A
    /// pass that reads rows checks its halo once with [`guard`] rather than once per read.
    pub fn row(self, plane: &[f32], y: i64) -> &[f32] {
        let y = y.clamp(0, i64::from(self.stage.height) - 1) as u32;
        let width = self.held.width as usize;
        let start = (y - self.held.y0) as usize * width;
        &plane[start..start + width]
    }
    /// The `n` values of [`Self::row`] from held column `first`, which no clamp moves: a pass reading
    /// columns inside the stage checks once with [`guard`] that they lie inside the held rectangle,
    /// and a column outside it fails the slice bounds. The slice is cut in the caller, so a loop of
    /// `n` reads it with no bounds check.
    #[inline(always)]
    pub fn span(self, plane: &[f32], y: i64, first: usize, n: usize) -> &[f32] {
        &self.row(plane, y)[first..][..n]
    }
}

/// The left, centre and right taps of a run of `n` pixels from `line`, which starts a column left
/// of the run: its `n` values from its first, second and third. Each is cut in the caller, so a
/// loop of `n` reads them with no bounds check.
#[inline(always)]
pub(super) fn taps(line: &[f32], n: usize) -> [&[f32]; 3] {
    [&line[..n], &line[1..][..n], &line[2..][..n]]
}

/// Where stage columns `x0..x1` sit in a row of a unit's input ([`Planes::row`]), found once for a
/// pass in place of one [`Planes::sample`] per pixel. Detail reads its input only at columns inside
/// the stage, where `sample`'s clamp changes nothing; this checks once that they lie inside the
/// input's rectangle, where `sample` would panic.
pub(super) fn input_columns(input: &Planes<'_>, x0: u32, x1: u32) -> Range<usize> {
    let given = input.region();
    assert!(
        given.x0 <= x0 && x0 <= x1 && x1 <= given.x1(),
        "Detail read its input's columns {x0}..{x1}, outside the {given:?} it was given"
    );
    (x0 - given.x0) as usize..(x1 - given.x0) as usize
}

/// Row `y` of the input's red, green and blue over `columns`, found by [`input_columns`], the
/// row clamped as [`Planes::row`] clamps it. Each is cut to `n` values in the caller, so a loop of
/// `n` reads them with no bounds check.
#[inline(always)]
pub(super) fn input_row<'a>(
    input: &Planes<'a>,
    y: i64,
    columns: &Range<usize>,
    n: usize,
) -> [&'a [f32]; 3] {
    let [red, green, blue] = input.row(y);
    [
        &red[columns.clone()][..n],
        &green[columns.clone()][..n],
        &blue[columns.clone()][..n],
    ]
}

/// Panics unless `reads`, every pixel a pass reads clamped to the stage, lies inside `within`, the
/// rectangle whose values it may read: the one halo check of a pass that reads whole rows.
pub(super) fn guard(within: Region, reads: Region) {
    assert!(
        reads.x0 >= within.x0
            && reads.x1() <= within.x1()
            && reads.y0 >= within.y0
            && reads.y1() <= within.y1(),
        "Detail read outside its declared halo"
    );
}

/// The rectangle a pass over `out` reads with taps up to `across` columns and `down` rows away,
/// clamped to the stage as each read is.
fn reach(out: Region, across: u32, down: u32, stage: Stage) -> Region {
    let x0 = out.x0.saturating_sub(across);
    let y0 = out.y0.saturating_sub(down);
    let x1 = out.x1().saturating_add(across).min(stage.width);
    let y1 = out.y1().saturating_add(down).min(stage.height);
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

pub(super) fn rows(
    dst: &mut [f32],
    geometry: Geometry,
    area: Region,
    parallelism: Parallelism,
    cancel: &Cancel,
    body: impl Fn(u32, &mut [f32]) + Sync + Send,
) -> Result<(), Error> {
    cancel.check()?;
    let width = geometry.held.width as usize;
    let start = (area.y0 - geometry.held.y0) as usize * width;
    let length = area.height as usize * width;
    let x0 = (area.x0 - geometry.held.x0) as usize;
    let run = |(r, row): (usize, &mut [f32])| {
        if !cancel.is_cancelled() {
            body(area.y0 + r as u32, &mut row[x0..x0 + area.width as usize]);
        }
    };
    match parallelism {
        Parallelism::Serial => dst[start..start + length]
            .chunks_mut(width)
            .enumerate()
            .for_each(run),
        Parallelism::Pool => dst[start..start + length]
            .par_chunks_mut(width)
            .enumerate()
            .for_each(run),
    }
    cancel.check()
}

/// Write several planes over `area` row by row, as [`rows`] writes one: `body` receives a row's
/// `y` and that row of each plane over `area`'s columns.
pub(super) fn rows_of<const N: usize>(
    planes: [&mut [f32]; N],
    geometry: Geometry,
    area: Region,
    parallelism: Parallelism,
    cancel: &Cancel,
    body: impl Fn(u32, [&mut [f32]; N]) + Sync + Send,
) -> Result<(), Error> {
    cancel.check()?;
    let width = geometry.held.width as usize;
    let start = (area.y0 - geometry.held.y0) as usize * width;
    let length = area.height as usize * width;
    let x0 = (area.x0 - geometry.held.x0) as usize;
    let columns = x0..x0 + area.width as usize;
    let mut planes = planes.map(|plane| plane[start..start + length].chunks_mut(width));
    let rows: Vec<[&mut [f32]; N]> = (0..area.height)
        .map(|_| {
            std::array::from_fn(|plane| {
                let row = planes[plane].next().expect("a row of each plane");
                &mut row[columns.clone()]
            })
        })
        .collect();
    let run = |(r, row): (usize, [&mut [f32]; N])| {
        if !cancel.is_cancelled() {
            body(area.y0 + r as u32, row);
        }
    };
    match parallelism {
        Parallelism::Serial => rows.into_iter().enumerate().for_each(run),
        Parallelism::Pool => rows.into_par_iter().enumerate().for_each(run),
    }
    cancel.check()
}

/// The separable smoothing of `src` over `out` into `dst`: the horizontal pass by `kernels[0]` into
/// `temporary`, over `out`'s columns and the rows the vertical pass by `kernels[1]` reads. Each value
/// is its centre plus each tap's weighted difference from it, the taps in the kernel's order and
/// clamped to the stage. Both passes run over whole rows, the taps outer and the pixels inner, which
/// adds each pixel's taps in that same order.
#[allow(clippy::too_many_arguments)]
pub(super) fn smooth(
    src: &[f32],
    dst: &mut [f32],
    temporary: &mut [f32],
    geometry: Geometry,
    out: Region,
    kernels: &[Kernel; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    cancel.check()?;
    if out.is_empty() {
        return Ok(());
    }
    horizontal(
        src,
        geometry.held,
        temporary,
        geometry,
        out,
        kernels,
        parallelism,
        cancel,
    )?;
    rows(dst, geometry, out, parallelism, cancel, |y, row| {
        vertical(temporary, geometry, out.x0, &kernels[1], y, row);
    })
}

/// [`smooth`] of one channel of a level's input `coarse`, whose values hold inside `valid`, into
/// `next` over `out`, leaving the level's detail band `coarse - next` in `coarse` over `out`. The
/// subtraction rides the vertical pass, which writes `coarse` only after the horizontal pass has
/// read all of it.
#[allow(clippy::too_many_arguments)]
pub(super) fn smooth_band(
    coarse: &mut [f32],
    valid: Region,
    next: &mut [f32],
    temporary: &mut [f32],
    geometry: Geometry,
    out: Region,
    kernels: &[Kernel; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    cancel.check()?;
    if out.is_empty() {
        return Ok(());
    }
    horizontal(
        coarse,
        valid,
        temporary,
        geometry,
        out,
        kernels,
        parallelism,
        cancel,
    )?;
    rows_of(
        [next, coarse],
        geometry,
        out,
        parallelism,
        cancel,
        |y, [row, band]| {
            vertical(temporary, geometry, out.x0, &kernels[1], y, row);
            for (band, smoothed) in band.iter_mut().zip(row.iter()) {
                *band -= smoothed;
            }
        },
    )
}

/// The horizontal pass of [`smooth`] into `temporary`. Every value of `src` it reads lies inside
/// `valid`, checked once for the pass.
#[allow(clippy::too_many_arguments)]
fn horizontal(
    src: &[f32],
    valid: Region,
    temporary: &mut [f32],
    geometry: Geometry,
    out: Region,
    kernels: &[Kernel; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let stage = geometry.stage;
    let down = kernels[1].radius;
    guard(valid, reach(out, kernels[0].radius, down, stage));
    let y0 = out.y0.saturating_sub(down);
    let y1 = out.y1().saturating_add(down).min(stage.height);
    let area = Region {
        x0: out.x0,
        y0,
        width: out.width,
        height: y1 - y0,
    };
    rows(temporary, geometry, area, parallelism, cancel, |y, row| {
        convolve(
            geometry.row(src, i64::from(y)),
            row,
            out.x0,
            geometry,
            &kernels[0],
        );
    })
}

/// One row of a horizontal pass: `row` receives the smoothing of `src`, one held row, from stage
/// column `x0`. The columns whose taps all lie inside the stage read sub-slices of `src`; the
/// columns within the kernel's radius of a stage edge clamp each tap.
fn convolve(src: &[f32], row: &mut [f32], x0: u32, geometry: Geometry, kernel: &Kernel) {
    let width = i64::from(geometry.stage.width);
    let held = i64::from(geometry.held.x0);
    let radius = i64::from(kernel.radius);
    let x0 = i64::from(x0);
    let x1 = x0 + row.len() as i64;
    let start = radius.clamp(x0, x1);
    let end = (width - radius).clamp(start, x1);
    let at = |x: i64| src[(x.clamp(0, width - 1) - held) as usize];
    for x in (x0..start).chain(end..x1) {
        let centre = at(x);
        let mut result = centre;
        for &(offset, weight) in &kernel.taps {
            result += weight * (at(x + offset) - centre);
        }
        row[(x - x0) as usize] = result;
    }
    if start < end {
        let values = &mut row[(start - x0) as usize..(end - x0) as usize];
        let first = (start - held) as usize;
        let centre = &src[first..first + values.len()];
        values.copy_from_slice(centre);
        for &(offset, weight) in &kernel.taps {
            let first = (start + offset - held) as usize;
            accumulate(values, &src[first..first + centre.len()], centre, weight);
        }
    }
}

/// One row of a vertical pass: `row` receives the smoothing of `temporary` at stage row `y` over
/// the columns from `x0`, each tap a held row clamped to the stage.
fn vertical(
    temporary: &[f32],
    geometry: Geometry,
    x0: u32,
    kernel: &Kernel,
    y: u32,
    row: &mut [f32],
) {
    let first = (x0 - geometry.held.x0) as usize;
    let columns = first..first + row.len();
    let line = |offset: i64| &geometry.row(temporary, i64::from(y) + offset)[columns.clone()];
    let centre = line(0);
    row.copy_from_slice(centre);
    for &(offset, weight) in &kernel.taps {
        accumulate(row, line(offset), centre, weight);
    }
}

/// One tap of a smoothing pass over a run of pixels: `value += weight * (tap - centre)` for each.
fn accumulate(values: &mut [f32], taps: &[f32], centre: &[f32], weight: f32) {
    for ((value, tap), centre) in values.iter_mut().zip(taps).zip(centre) {
        *value += weight * (tap - centre);
    }
}

pub(super) fn take<'a>(scratch: &mut &'a mut [f32], len: usize) -> Result<&'a mut [f32], Error> {
    if scratch.len() < len {
        return Err(Error::internal(
            "Detail declared less tile scratch than it uses",
        ));
    }
    let (front, rest) = std::mem::take(scratch).split_at_mut(len);
    *scratch = rest;
    Ok(front)
}

/// The Oklab of `input` over the held rectangle into `buffer`, planar L, a and b. The held rectangle
/// lies inside the stage, so each row of the input is read as slices with no clamp.
pub(super) fn lab(
    input: &Planes<'_>,
    buffer: &mut [f32],
    geometry: Geometry,
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    cancel.check()?;
    let held = geometry.held;
    let columns = input_columns(input, held.x0, held.x1());
    let mut planes = PlanesMut::new(geometry.stage, held, buffer)?;
    planes.for_rows(parallelism, |y, lightness, a, b| {
        if cancel.is_cancelled() {
            return;
        }
        let n = lightness.len();
        let rgb = input_row(input, i64::from(y), &columns, n);
        let (a, b) = (&mut a[..n], &mut b[..n]);
        for column in 0..n {
            let lab = oklab::to_oklab([rgb[0][column], rgb[1][column], rgb[2][column]]);
            [lightness[column], a[column], b[column]] = [lab.l, lab.a, lab.b];
        }
    });
    cancel.check()
}

pub(super) fn reconstruct(input: [f32; 3], lab: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
    if delta.iter().all(|v| *v == 0.0) {
        return input;
    }
    let [l, a, b] = std::array::from_fn(|c| lab[c] + delta[c]);
    if a.abs() <= NEUTRAL_CHROMA_SNAP && b.abs() <= NEUTRAL_CHROMA_SNAP {
        [l * l * l; 3]
    } else {
        oklab::from_oklab(oklab::Oklab { l, a, b })
    }
}
