//! Bounded, fixed-order Detail passes. Planes borrow the tile's declared scratch.

use crate::{
    Cancel, Error,
    colour::oklab,
    modules::{Parallelism, Planes, PlanesMut, Region, SamplingScale, Stage},
};
use rayon::prelude::*;

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
    let y0 = out.y0.saturating_sub(kernels[1].radius);
    let y1 = out
        .y1()
        .saturating_add(kernels[1].radius)
        .min(geometry.stage.height);
    let horizontal = Region {
        x0: out.x0,
        y0,
        width: out.width,
        height: y1 - y0,
    };
    rows(
        temporary,
        geometry,
        horizontal,
        parallelism,
        cancel,
        |y, row| {
            for (column, value) in row.iter_mut().enumerate() {
                let x = i64::from(out.x0) + column as i64;
                let y = i64::from(y);
                let center = geometry.sample(src, x, y);
                let mut result = center;
                for &(offset, weight) in &kernels[0].taps {
                    result += weight * (geometry.sample(src, x + offset, y) - center);
                }
                *value = result;
            }
        },
    )?;
    rows(dst, geometry, out, parallelism, cancel, |y, row| {
        for (column, value) in row.iter_mut().enumerate() {
            let x = i64::from(out.x0) + column as i64;
            let y = i64::from(y);
            let center = geometry.sample(temporary, x, y);
            let mut result = center;
            for &(offset, weight) in &kernels[1].taps {
                result += weight * (geometry.sample(temporary, x, y + offset) - center);
            }
            *value = result;
        }
    })
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

pub(super) fn lab(
    input: &Planes<'_>,
    buffer: &mut [f32],
    geometry: Geometry,
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    cancel.check()?;
    let mut planes = PlanesMut::new(geometry.stage, geometry.held, buffer)?;
    planes.for_rows(parallelism, |y, lightness, a, b| {
        if cancel.is_cancelled() {
            return;
        }
        for column in 0..geometry.held.width as usize {
            let x = geometry.held.x0 + column as u32;
            let lab = oklab::to_oklab(input.sample(i64::from(x), i64::from(y)));
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
