//! The robust bundle adjustment of every accepted pair's inliers: Levenberg–Marquardt over one
//! rotation a frame (all but the anchor, which fixes the gauge), the shared focal length (as its
//! logarithm) and, when fitted, the shared radial term.
//!
//! Each inlier contributes its transfer residual both ways: the pixel of one frame carried
//! through the lens, the two rotations and the lens again into the other, minus the matched pixel
//! there, in full-resolution pixels. A residual of length `e` costs Huber's `e²/2` up to the
//! scale `c` and `c·e − c²/2` beyond it, so a few wrong matches pull the fit by a bounded amount,
//! times the match's weight: `4^−ℓ` for a match found on pyramid level `ℓ`, whose position is
//! known `2^ℓ` times less precisely.
//! Rotations update on the left, `R ← exp([δ]×)·R`; their derivatives are analytic, the focal
//! length's and radial term's central differences.
//!
//! Bounds: the caller supplies at most [`super::registration::MAX_BUNDLE_MATCHES`] inliers a pair
//! over at most 496 pairs; the normal equations are dense, at most 98 × 98. At most
//! `max_iterations` linearizations, each trying at most [`MAX_DAMPING_TRIES`] dampings; `cancel` is
//! checked before each linearization and each damping, so it ends the adjustment within one pass
//! over the observations. Each pair's contribution is computed independently (on the
//! shared pool) and summed in pair order, so the result does not depend on the thread count.

use super::camera::{Lens, MAX_RADIAL, Rotation, rotation_from};
use super::linalg::{self, V3};
use super::matching::Correspondence;
use crate::{Cancel, Error};
use rayon::prelude::*;

/// Dampings one iteration tries before the fit is taken as converged.
pub const MAX_DAMPING_TRIES: usize = 10;
/// An iteration whose accepted step lowers the cost by less than this fraction ends the fit.
const CONVERGED: f64 = 1e-10;
/// Central-difference steps for the logarithm of the focal length and for the radial term.
const LOG_FOCAL_STEP: f64 = 1e-6;
const RADIAL_STEP: f64 = 1e-6;

/// One accepted pair's inliers: the first frame's pixel and the second's, and each match's
/// weight.
#[derive(Clone, Debug)]
pub(crate) struct PairObservations {
    pub(crate) a: usize,
    pub(crate) b: usize,
    pub(crate) points: Vec<Correspondence>,
    pub(crate) weights: Vec<f64>,
}

/// What one adjustment solved.
#[derive(Clone, Debug)]
pub(crate) struct Adjusted {
    pub(crate) lens: Lens,
    pub(crate) rotations: Vec<Rotation>,
    pub(crate) iterations: usize,
}

/// The adjustment's unknowns: which frames have rotation parameters, and where.
struct Layout {
    slot: Vec<Option<usize>>,
    focal: usize,
    radial: Option<usize>,
    len: usize,
}

impl Layout {
    fn new(solved: &[bool], anchor: usize, fit_radial: bool) -> Self {
        let mut slot = vec![None; solved.len()];
        let mut next = 0;
        for (frame, &is_solved) in solved.iter().enumerate() {
            if is_solved && frame != anchor {
                slot[frame] = Some(next);
                next += 3;
            }
        }
        let focal = next;
        let radial = fit_radial.then_some(focal + 1);
        let len = focal + 1 + usize::from(fit_radial);
        Self {
            slot,
            focal,
            radial,
            len,
        }
    }
}

/// Huber's cost and weight of a residual of length `e` at scale `c`.
#[inline]
fn huber(e: f64, c: f64) -> (f64, f64) {
    if e <= c {
        (0.5 * e * e, 1.0)
    } else {
        (c * e - 0.5 * c * c, c / e)
    }
}

/// The residual of one direction of one match: `from` in frame `src` carried into frame `dst`,
/// minus `to`; `None` when it lands behind `dst`.
#[inline]
fn residual(
    lens: &Lens,
    relative: &Rotation,
    from: (f64, f64),
    to: (f64, f64),
) -> Option<[f64; 2]> {
    let p = lens.project(relative.to_camera(lens.ray(from)))?;
    Some([p.0 - to.0, p.1 - to.1])
}

/// A pair's contribution to the normal equations over its local unknowns
/// `[δ_a (3), δ_b (3), log f, k]`: the 8 × 8 block, the gradient and the cost.
struct Block {
    h: [[f64; 8]; 8],
    g: [f64; 8],
    cost: f64,
}

/// The derivative of the recorded pixel with respect to the camera direction `b`.
#[inline]
fn projection_jacobian(lens: &Lens, b: V3) -> [[f64; 3]; 2] {
    let f = lens.focal_px;
    let (iz, iz2) = (1.0 / b[2], 1.0 / (b[2] * b[2]));
    let q = [f * b[0] * iz, f * b[1] * iz];
    let dq = [
        [f * iz, 0.0, -f * b[0] * iz2],
        [0.0, f * iz, -f * b[1] * iz2],
    ];
    let r2 = lens.radial_scale().powi(2);
    let s = 1.0 + lens.radial * (q[0] * q[0] + q[1] * q[1]) / r2;
    let t = 2.0 * lens.radial / r2;
    let d = [
        [s + t * q[0] * q[0], t * q[0] * q[1]],
        [t * q[1] * q[0], s + t * q[1] * q[1]],
    ];
    std::array::from_fn(|r| std::array::from_fn(|c| d[r][0] * dq[0][c] + d[r][1] * dq[1][c]))
}

fn lens_with(lens: &Lens, log_focal_step: f64, radial_step: f64) -> Lens {
    Lens {
        focal_px: lens.focal_px * log_focal_step.exp(),
        radial: lens.radial + radial_step,
        ..*lens
    }
}

fn block(
    lens: &Lens,
    rotations: &[Rotation],
    pair: &PairObservations,
    huber_scale: f64,
    fit_radial: bool,
) -> Block {
    let mut out = Block {
        h: [[0.0; 8]; 8],
        g: [0.0; 8],
        cost: 0.0,
    };
    let lenses = [
        lens_with(lens, LOG_FOCAL_STEP, 0.0),
        lens_with(lens, -LOG_FOCAL_STEP, 0.0),
        lens_with(lens, 0.0, RADIAL_STEP),
        lens_with(lens, 0.0, -RADIAL_STEP),
    ];
    // Both directions: (source, destination, the source's local offset, the destination's).
    for (src, dst, so, do_) in [(pair.a, pair.b, 0usize, 3usize), (pair.b, pair.a, 3, 0)] {
        let relative = rotations[dst].relative_to(&rotations[src]);
        for (&(pa, pb), &weight) in pair.points.iter().zip(&pair.weights) {
            let (from, to) = if src == pair.a { (pa, pb) } else { (pb, pa) };
            let ray = lens.ray(from);
            let b = relative.to_camera(ray);
            let Some(r) = residual(lens, &relative, from, to) else {
                continue;
            };
            let e = (r[0] * r[0] + r[1] * r[1]).sqrt();
            let (cost, w) = huber(e, huber_scale);
            out.cost += weight * cost;
            let w = weight * w;
            let jp = projection_jacobian(lens, b);
            // ∂b/∂δ_dst = −[b]×, ∂b/∂δ_src = R_rel·[ray]×.
            let neg_skew_b = linalg::skew(linalg::scale(b, -1.0));
            let src_term = linalg::mul(&relative.0, &linalg::skew(ray));
            let mut j = [[0.0f64; 8]; 2];
            for row in 0..2 {
                for c in 0..3 {
                    j[row][do_ + c] = (0..3).map(|k| jp[row][k] * neg_skew_b[k][c]).sum();
                    j[row][so + c] = (0..3).map(|k| jp[row][k] * src_term[k][c]).sum();
                }
            }
            let central = |plus: &Lens, minus: &Lens, step: f64| -> [f64; 2] {
                match (
                    residual(plus, &relative, from, to),
                    residual(minus, &relative, from, to),
                ) {
                    (Some(p), Some(m)) => {
                        [(p[0] - m[0]) / (2.0 * step), (p[1] - m[1]) / (2.0 * step)]
                    }
                    _ => [0.0, 0.0],
                }
            };
            let df = central(&lenses[0], &lenses[1], LOG_FOCAL_STEP);
            j[0][6] = df[0];
            j[1][6] = df[1];
            if fit_radial {
                let dk = central(&lenses[2], &lenses[3], RADIAL_STEP);
                j[0][7] = dk[0];
                j[1][7] = dk[1];
            }
            for row in 0..2 {
                for p in 0..8 {
                    out.g[p] += w * j[row][p] * r[row];
                    for q in 0..8 {
                        out.h[p][q] += w * j[row][p] * j[row][q];
                    }
                }
            }
        }
    }
    out
}

/// The robust cost of the whole problem at `lens` and `rotations`.
fn total_cost(
    lens: &Lens,
    rotations: &[Rotation],
    pairs: &[PairObservations],
    huber_scale: f64,
) -> f64 {
    let costs: Vec<f64> = pairs
        .par_iter()
        .map(|pair| {
            let mut cost = 0.0;
            for (src, dst) in [(pair.a, pair.b), (pair.b, pair.a)] {
                let relative = rotations[dst].relative_to(&rotations[src]);
                for (&(pa, pb), &weight) in pair.points.iter().zip(&pair.weights) {
                    let (from, to) = if src == pair.a { (pa, pb) } else { (pb, pa) };
                    cost += weight
                        * match residual(lens, &relative, from, to) {
                            Some(r) => huber((r[0] * r[0] + r[1] * r[1]).sqrt(), huber_scale).0,
                            // A point carried behind the other camera costs as a far outlier.
                            None => huber(1e6, huber_scale).0,
                        };
                }
            }
            cost
        })
        .collect();
    costs.iter().sum()
}

/// Adjust `rotations` (of the `solved` frames, `anchor` fixed) and `lens` to the pairs' inliers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn adjust(
    lens: Lens,
    rotations: Vec<Rotation>,
    solved: &[bool],
    anchor: usize,
    pairs: &[PairObservations],
    fit_radial: bool,
    huber_scale: f64,
    max_iterations: usize,
    cancel: &Cancel,
) -> Result<Adjusted, Error> {
    let layout = Layout::new(solved, anchor, fit_radial);
    let n = layout.len;
    let mut lens = lens;
    let mut rotations = rotations;
    let mut cost = total_cost(&lens, &rotations, pairs, huber_scale);
    let mut lambda = 1e-3;
    let mut iterations = 0;
    while iterations < max_iterations {
        cancel.check()?;
        iterations += 1;
        let blocks: Vec<Block> = pairs
            .par_iter()
            .map(|pair| block(&lens, &rotations, pair, huber_scale, fit_radial))
            .collect();
        let mut h = vec![0.0f64; n * n];
        let mut g = vec![0.0f64; n];
        for (pair, block) in pairs.iter().zip(&blocks) {
            let index = |local: usize| -> Option<usize> {
                match local {
                    0..=2 => layout.slot[pair.a].map(|s| s + local),
                    3..=5 => layout.slot[pair.b].map(|s| s + local - 3),
                    6 => Some(layout.focal),
                    _ => layout.radial,
                }
            };
            for p in 0..8 {
                let Some(gp) = index(p) else { continue };
                g[gp] += block.g[p];
                for q in 0..8 {
                    if let Some(gq) = index(q) {
                        h[gp * n + gq] += block.h[p][q];
                    }
                }
            }
        }
        let mut accepted = None;
        for _ in 0..MAX_DAMPING_TRIES {
            cancel.check()?;
            let mut damped = h.clone();
            for i in 0..n {
                damped[i * n + i] += lambda * h[i * n + i].max(1e-9);
            }
            let rhs: Vec<f64> = g.iter().map(|v| -v).collect();
            let Some(step) = linalg::cholesky_solve(&mut damped, &rhs, n) else {
                lambda *= 4.0;
                continue;
            };
            let mut next_rotations = rotations.clone();
            for (frame, slot) in layout.slot.iter().enumerate() {
                if let Some(s) = slot {
                    let update = linalg::exp_so3([step[*s], step[s + 1], step[s + 2]]);
                    next_rotations[frame] =
                        rotation_from(linalg::mul(&update, &rotations[frame].0));
                }
            }
            let next_lens = Lens {
                focal_px: lens.focal_px * step[layout.focal].exp(),
                radial: match layout.radial {
                    Some(k) => (lens.radial + step[k]).clamp(-MAX_RADIAL, MAX_RADIAL),
                    None => lens.radial,
                },
                ..lens
            };
            let next_cost = total_cost(&next_lens, &next_rotations, pairs, huber_scale);
            if next_cost < cost {
                accepted = Some((next_lens, next_rotations, next_cost));
                lambda = (lambda / 3.0).max(1e-12);
                break;
            }
            lambda *= 4.0;
        }
        let Some((next_lens, next_rotations, next_cost)) = accepted else {
            break;
        };
        let decrease = (cost - next_cost) / cost.max(f64::MIN_POSITIVE);
        lens = next_lens;
        rotations = next_rotations;
        cost = next_cost;
        if decrease < CONVERGED {
            break;
        }
    }
    Ok(Adjusted {
        lens,
        rotations,
        iterations,
    })
}
