//! The stitch's band-limited blend (`docs/design/panorama-merge.md`, "Exposure, colour and
//! seams"): two bands, split at the plan grid, in the manner of Burt and Adelson's multiresolution
//! spline.
//!
//! On the plan grid each frame `i` has its gained colour samples `Sᵢ` where it is eligible, a low
//! band `Lᵢ` (the normalized Gaussian blur of `Sᵢ` over its eligible cells, σ =
//! [`BLEND_SIGMA_CELLS`]) and a seam mask `Mᵢ` (1 where the seam map takes `i`). Its wide weight
//! `Wᵢ` is the blurred mask normalized by the blurred labelled region, so `Σ Wᵢ = 1` wherever a
//! cell is labelled. The output is
//!
//! `out = Σ Mᵢ·Fᵢ + Σ (Wᵢ − Mᵢ)·Lᵢ`
//!
//! with `Fᵢ` frame `i`'s full-resolution sample: the detail band switches at the seam, the low band
//! fades across it over the blur's reach. The second sum is the **correction field** `C`, zero
//! wherever one frame alone is labelled within the blur's reach, held per cell and bilinear
//! between cell centres; the masks are the seam map's labels, bilinear between cell centres, so
//! the seam's transition is one cell wide. Each output point therefore reads only its frames'
//! samples at its own position and the plan grid around it: the widest band is the plan grid
//! itself, and a chunk reads bounded windows. `C` lies between `−max Lᵢ` and `max Lᵢ` of the
//! frames within the blur's reach of a seam: an overshoot past the frames' own samples is bounded
//! by their low bands' spread there, and is recorded per chunk.

use super::seams::NONE;
use crate::{Cancel, Error};
use rayon::prelude::*;

/// The low band's Gaussian, in plan cells.
pub const BLEND_SIGMA_CELLS: f32 = 4.0;
/// Its reach: three sigmas.
pub const BLEND_RADIUS_CELLS: usize = 12;

fn kernel() -> [f32; 2 * BLEND_RADIUS_CELLS + 1] {
    std::array::from_fn(|k| {
        let x = k as f32 - BLEND_RADIUS_CELLS as f32;
        (-(x * x) / (2.0 * BLEND_SIGMA_CELLS * BLEND_SIGMA_CELLS)).exp()
    })
}

/// Blur `planes` (each `width × height`, row-major) by the low band's Gaussian, zero outside.
pub(crate) fn blur(planes: &mut [&mut Vec<f32>], width: usize, height: usize) {
    let k = kernel();
    let r = BLEND_RADIUS_CELLS as isize;
    for plane in planes.iter_mut() {
        let rows: Vec<f32> = plane
            .par_chunks(width)
            .flat_map_iter(|row| {
                (0..width).map(move |x| {
                    let mut sum = 0.0;
                    for (t, w) in k.iter().enumerate() {
                        let xx = x as isize + t as isize - r;
                        if xx >= 0 && (xx as usize) < width {
                            sum += w * row[xx as usize];
                        }
                    }
                    sum
                })
            })
            .collect();
        let columns: Vec<f32> = (0..height)
            .into_par_iter()
            .flat_map_iter(|y| {
                let rows = &rows;
                (0..width).map(move |x| {
                    let mut sum = 0.0;
                    for (t, w) in k.iter().enumerate() {
                        let yy = y as isize + t as isize - r;
                        if yy >= 0 && (yy as usize) < height {
                            sum += w * rows[yy as usize * width + x];
                        }
                    }
                    sum
                })
            })
            .collect();
        **plane = columns;
    }
}

/// The correction field `C` (three planes over the `gw × gh` grid). `labels` is the seam map,
/// `eligible` each cell's eligible frames, `bounds` each frame's cells `[x₀, y₀, x₁, y₁)`, and
/// `sample(frame, cell)` a frame's gained linear colour at an eligible cell. Checks `cancel` per
/// frame.
pub(crate) fn correction_field(
    gw: usize,
    gh: usize,
    labels: &[u8],
    eligible: &[u32],
    bounds: &[[usize; 4]],
    sample: &(dyn Fn(usize, usize) -> Option<[f32; 3]> + Sync),
    cancel: &Cancel,
) -> Result<[Vec<f32>; 3], Error> {
    let n = gw * gh;
    let mut field = [vec![0.0_f32; n], vec![0.0_f32; n], vec![0.0_f32; n]];
    let mut labelled: Vec<f32> = labels
        .iter()
        .map(|&l| if l == NONE { 0.0 } else { 1.0 })
        .collect();
    blur(&mut [&mut labelled], gw, gh);
    for (frame, &[bx0, by0, bx1, by1]) in bounds.iter().enumerate() {
        cancel.check()?;
        if !labels.iter().any(|&l| usize::from(l) == frame) {
            continue;
        }
        let r = BLEND_RADIUS_CELLS;
        let (x0, y0) = (bx0.saturating_sub(r), by0.saturating_sub(r));
        let (x1, y1) = ((bx1 + r).min(gw), (by1 + r).min(gh));
        let (w, h) = (x1 - x0, y1 - y0);
        let global = |i: usize| (y0 + i / w) * gw + x0 + i % w;
        let bit = 1_u32 << frame;
        let samples: Vec<Option<[f32; 3]>> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let cell = global(i);
                (eligible[cell] & bit != 0)
                    .then(|| sample(frame, cell))
                    .flatten()
            })
            .collect();
        let mut planes: [Vec<f32>; 5] = [
            samples.iter().map(|s| s.map_or(0.0, |s| s[0])).collect(),
            samples.iter().map(|s| s.map_or(0.0, |s| s[1])).collect(),
            samples.iter().map(|s| s.map_or(0.0, |s| s[2])).collect(),
            samples
                .iter()
                .map(|s| f32::from(u8::from(s.is_some())))
                .collect(),
            (0..w * h)
                .map(|i| f32::from(u8::from(usize::from(labels[global(i)]) == frame)))
                .collect(),
        ];
        drop(samples);
        {
            let [a, b, c, d, e] = &mut planes;
            blur(&mut [a, b, c, d, e], w, h);
        }
        #[allow(clippy::needless_range_loop)]
        for i in 0..w * h {
            let cell = global(i);
            let label = labels[cell];
            if label == NONE {
                continue;
            }
            let mask = f32::from(u8::from(usize::from(label) == frame));
            let weight = if labelled[cell] > 0.0 {
                planes[4][i] / labelled[cell]
            } else {
                0.0
            };
            if weight == 0.0 && mask == 0.0 {
                continue;
            }
            let support = planes[3][i];
            if support <= 1e-6 {
                continue;
            }
            for (c, plane) in field.iter_mut().enumerate() {
                plane[cell] += (weight - mask) * (planes[c][i] / support);
            }
        }
    }
    Ok(field)
}
