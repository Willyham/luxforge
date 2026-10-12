//! The valid common coverage of an aligned bracket: the largest axis-aligned rectangle of
//! reference pixels that every input recorded, so the result has no filled border and no
//! upscaling.
//!
//! A reference pixel `p` is covered by an input when its [`RigidTransform`] maps it inside the
//! input's samples, `0 ≤ q_x ≤ W − 1` and `0 ≤ q_y ≤ H − 1`: then every bilinear tap the
//! resample weights above zero is a recorded pixel. The reference covers itself. The covered set
//! is convex, so each row's covered pixels are one run, found from the linear bounds and then
//! confirmed pixel by pixel with the same evaluation the resample uses.
//!
//! The rectangle is the largest by area over every span of rows, its columns the span's common
//! run; ties keep the smallest top row, then the smallest bottom row. The result's pixel `(i, j)`
//! is reference pixel `(rect.x + i, rect.y + j)`. A rectangle under half the frame on either side
//! is refused ([`Refusal::InsufficientCoverage`]).

use super::align::{Affine, PixelRect, RigidTransform};
use super::frame::Refusal;
use crate::{Cancel, Error};
use serde::{Deserialize, Serialize};

/// The common coverage and what it costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub frame_width: u32,
    pub frame_height: u32,
    /// The output rectangle in reference coordinates.
    pub rect: PixelRect,
    /// Reference pixels outside it.
    pub pixels_lost: u64,
}

/// Whether input point `q` lies where bilinear reads only recorded samples of a `width × height`
/// frame.
#[inline]
pub(crate) fn inside(q: (f64, f64), width: u32, height: u32) -> bool {
    q.0 >= 0.0 && q.1 >= 0.0 && q.0 <= f64::from(width) - 1.0 && q.1 <= f64::from(height) - 1.0
}

/// The coverage of a `width × height` bracket under `transforms`, one per input (the reference's
/// the identity).
pub fn common_coverage(
    width: u32,
    height: u32,
    transforms: &[RigidTransform],
    cancel: &Cancel,
) -> Result<Coverage, Error> {
    if width == 0 || height == 0 {
        return Err(Error::validation("HDR coverage needs a frame"));
    }
    let affines: Vec<Affine> = transforms
        .iter()
        .filter(|transform| !transform.is_identity())
        .map(RigidTransform::affine)
        .collect();
    let covered = |x: u32, y: u32| {
        affines
            .iter()
            .all(|affine| inside(affine.map(f64::from(x), f64::from(y)), width, height))
    };
    let mut runs: Vec<Option<(u32, u32)>> = Vec::with_capacity(height as usize);
    for y in 0..height {
        if y % 256 == 0 {
            cancel.check()?;
        }
        runs.push(row_run(&affines, width, height, y, &covered));
    }
    cancel.check()?;
    let rect = largest(&runs, width).unwrap_or_default();
    if u64::from(rect.width) * 2 < u64::from(width)
        || u64::from(rect.height) * 2 < u64::from(height)
    {
        return Err(Refusal::InsufficientCoverage {
            width: rect.width,
            height: rect.height,
        }
        .into());
    }
    Ok(Coverage {
        frame_width: width,
        frame_height: height,
        rect,
        pixels_lost: u64::from(width) * u64::from(height) - rect.pixels(),
    })
}

/// Row `y`'s covered run, inclusive.
fn row_run(
    affines: &[Affine],
    width: u32,
    height: u32,
    y: u32,
    covered: &impl Fn(u32, u32) -> bool,
) -> Option<(u32, u32)> {
    let (mut lo, mut hi) = (0.0_f64, f64::from(width) - 1.0);
    let y_f = f64::from(y);
    for affine in affines {
        // q_x and q_y are each `slope · x + intercept` along the row.
        for (slope, intercept, limit) in [
            (
                affine.m[0][0],
                affine.m[0][1] * y_f + affine.b[0],
                f64::from(width) - 1.0,
            ),
            (
                affine.m[1][0],
                affine.m[1][1] * y_f + affine.b[1],
                f64::from(height) - 1.0,
            ),
        ] {
            if slope == 0.0 {
                if !(0.0..=limit).contains(&intercept) {
                    return None;
                }
                continue;
            }
            let (a, b) = ((0.0 - intercept) / slope, (limit - intercept) / slope);
            lo = lo.max(a.min(b));
            hi = hi.min(a.max(b));
        }
    }
    if lo.is_nan() || hi.is_nan() || lo > hi {
        return None;
    }
    // The bounds are within a rounding of the true run: settle each end pixel by pixel.
    let last = width - 1;
    let mut left = (lo.ceil().max(0.0) as u32).min(last);
    let mut right = (hi.floor().max(0.0) as u32).min(last);
    while left > 0 && covered(left - 1, y) {
        left -= 1;
    }
    while left <= right && !covered(left, y) {
        left += 1;
    }
    if left > right {
        // The estimate missed a narrow run entirely: look across the row.
        let first = (0..width).find(|&x| covered(x, y))?;
        left = first;
        right = first;
    }
    while right < last && covered(right + 1, y) {
        right += 1;
    }
    while right > left && !covered(right, y) {
        right -= 1;
    }
    Some((left, right))
}

/// The largest rectangle under per-row runs: see the module documentation.
fn largest(runs: &[Option<(u32, u32)>], width: u32) -> Option<PixelRect> {
    let height = runs.len();
    let mut best: Option<(u64, PixelRect)> = None;
    for y0 in 0..height {
        let best_area = best.map_or(0, |(area, _)| area);
        if u64::from(width) * (height - y0) as u64 <= best_area {
            break;
        }
        let (mut left, mut right) = (0_u32, u32::MAX);
        for (y1, run) in runs.iter().enumerate().skip(y0) {
            let Some((l, r)) = *run else {
                break;
            };
            left = left.max(l);
            right = right.min(r);
            if left > right {
                break;
            }
            let run_width = u64::from(right - left + 1);
            let best_area = best.map_or(0, |(area, _)| area);
            // No taller span from this top row can be wider than this one.
            if run_width * (height - y0) as u64 <= best_area {
                break;
            }
            let area = run_width * (y1 - y0 + 1) as u64;
            if area > best_area {
                best = Some((
                    area,
                    PixelRect {
                        x: left,
                        y: y0 as u32,
                        width: right - left + 1,
                        height: (y1 - y0 + 1) as u32,
                    },
                ));
            }
        }
    }
    best.map(|(_, rect)| rect)
}
