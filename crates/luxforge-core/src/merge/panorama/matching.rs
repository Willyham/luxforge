//! Putative matches between two frames' features, and the robust fits that verify them.
//!
//! - **Putative matches.** Every feature of the first frame is compared with every feature of the
//!   second by the Hamming distance of their descriptors. A pair of features is a putative match
//!   when each is the other's nearest (ties to the lower index), the distance is at most
//!   [`MAX_HAMMING`], and the second-nearest feature of the first frame is further by the ratio
//!   [`RATIO`] (Lowe's test). At most `min(n_a, n_b)` matches a pair.
//! - **Homography.** RANSAC over 4-point homographies (direct linear transform in coordinates
//!   centred on the frame and scaled by its half-diagonal), at most [`MAX_RANSAC_ITERATIONS`]
//!   draws from a generator seeded by the pair, stopping early once [`RANSAC_CONFIDENCE`] of
//!   drawing an all-inlier sample is reached. A match is an inlier when it transfers within the
//!   threshold both ways. The best consensus is refitted by least squares twice.
//! - **Verification.** In the manner of Brown and Lowe: with `n_f` putative matches in the
//!   model's overlap and `n_i` of them inliers, the pair is a match when `n_i` is at least
//!   [`MIN_PAIR_INLIERS`] and the binomial posterior that the frames match exceeds
//!   [`VERIFY_POSTERIOR`], an inlier being [`P_INLIER_MATCH`] likely for frames that match and
//!   [`P_INLIER_FALSE`] for frames that do not, under a prior of [`P_MATCH_PRIOR`]: a linear test
//!   `n_i > α + β·n_f` ([`verified`]).
//!
//! `cancel` is checked every 256 features of the first frame while matching and every 64 RANSAC
//! draws.

use super::features::{Descriptor, FrameFeatures};
use super::linalg::{self, Draws};
use crate::{Cancel, Error};

/// The largest descriptor distance of a putative match, of 256 bits.
pub const MAX_HAMMING: u32 = 64;
/// Lowe's ratio: the nearest distance must be under this fraction of the second-nearest.
pub const RATIO: f64 = 0.8;
/// The most RANSAC draws one fit makes.
pub const MAX_RANSAC_ITERATIONS: usize = 512;
/// RANSAC stops once an all-inlier draw is this likely.
pub const RANSAC_CONFIDENCE: f64 = 0.999;
/// The fewest inliers a verified pair has.
pub const MIN_PAIR_INLIERS: usize = 12;
/// Verification: an inlier's probability for frames that match...
pub const P_INLIER_MATCH: f64 = 0.6;
/// ...and for frames that do not...
pub const P_INLIER_FALSE: f64 = 0.1;
/// ...the prior that two frames match...
pub const P_MATCH_PRIOR: f64 = 1e-6;
/// ...and the posterior a match must exceed.
pub const VERIFY_POSTERIOR: f64 = 0.999;
/// Draws between cancellation checks.
const CHECK_DRAWS: usize = 64;
/// First-frame features between cancellation checks while matching.
const CHECK_FEATURES: usize = 256;

/// The verification's linear test: `(α, β)` with `n_i > α + β·n_f`.
pub fn verification_line() -> (f64, f64) {
    let log_odds = |p1: f64, p0: f64| (p1 / p0).ln();
    let inlier = log_odds(P_INLIER_MATCH, P_INLIER_FALSE);
    let outlier = log_odds(1.0 - P_INLIER_MATCH, 1.0 - P_INLIER_FALSE);
    let threshold = (VERIFY_POSTERIOR / (1.0 - VERIFY_POSTERIOR)).ln()
        - (P_MATCH_PRIOR / (1.0 - P_MATCH_PRIOR)).ln();
    (
        threshold / (inlier - outlier),
        -outlier / (inlier - outlier),
    )
}

/// Whether `inliers` of `overlap` putative matches verify a pair.
pub fn verified(inliers: usize, overlap: usize) -> bool {
    let (alpha, beta) = verification_line();
    inliers >= MIN_PAIR_INLIERS && inliers as f64 > alpha + beta * overlap as f64
}

#[inline]
fn hamming(a: &Descriptor, b: &Descriptor) -> u32 {
    (a[0] ^ b[0]).count_ones()
        + (a[1] ^ b[1]).count_ones()
        + (a[2] ^ b[2]).count_ones()
        + (a[3] ^ b[3]).count_ones()
}

/// The putative matches of two frames, as feature index pairs `(a, b)` in increasing `a`.
pub fn putative_matches(
    a: &FrameFeatures,
    b: &FrameFeatures,
    cancel: &Cancel,
) -> Result<Vec<(u32, u32)>, Error> {
    let (da, db) = (&a.descriptors, &b.descriptors);
    let mut row_best = vec![(u32::MAX, u32::MAX, u32::MAX); da.len()];
    let mut column_best = vec![(u32::MAX, u32::MAX); db.len()];
    for (i, descriptor) in da.iter().enumerate() {
        if i % CHECK_FEATURES == 0 {
            cancel.check()?;
        }
        let (mut best, mut second, mut index) = (u32::MAX, u32::MAX, u32::MAX);
        for (j, other) in db.iter().enumerate() {
            let d = hamming(descriptor, other);
            if d < best {
                second = best;
                best = d;
                index = j as u32;
            } else if d < second {
                second = d;
            }
            if d < column_best[j].0 {
                column_best[j] = (d, i as u32);
            }
        }
        row_best[i] = (best, second, index);
    }
    Ok(row_best
        .iter()
        .enumerate()
        .filter(|&(i, &(best, second, j))| {
            j != u32::MAX
                && best <= MAX_HAMMING
                && f64::from(best) < RATIO * f64::from(second)
                && column_best[j as usize].1 == i as u32
        })
        .map(|(i, &(_, _, j))| (i as u32, j))
        .collect())
}

/// A 3 × 3 homography acting on full-resolution pixels, row-major.
pub type Homography = [[f64; 3]; 3];

/// Apply a homography; `None` at or beyond the line at infinity.
#[inline]
pub fn map(h: &Homography, p: (f64, f64)) -> Option<(f64, f64)> {
    let w = h[2][0] * p.0 + h[2][1] * p.1 + h[2][2];
    if w.abs() < 1e-12 {
        return None;
    }
    Some((
        (h[0][0] * p.0 + h[0][1] * p.1 + h[0][2]) / w,
        (h[1][0] * p.0 + h[1][1] * p.1 + h[1][2]) / w,
    ))
}

/// The inverse of a homography, or `None` when it is singular.
pub fn invert(h: &Homography) -> Option<Homography> {
    let m = h;
    let cof =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let adj = [
        [cof(1, 2, 1, 2), -cof(0, 2, 1, 2), cof(0, 1, 1, 2)],
        [-cof(1, 2, 0, 2), cof(0, 2, 0, 2), -cof(0, 1, 0, 2)],
        [cof(1, 2, 0, 1), -cof(0, 2, 0, 1), cof(0, 1, 0, 1)],
    ];
    let det = m[0][0] * adj[0][0] + m[0][1] * adj[1][0] + m[0][2] * adj[2][0];
    if det.abs() < 1e-300 || !det.is_finite() {
        return None;
    }
    Some(adj.map(|row| row.map(|v| v / det)))
}

#[inline]
fn distance(p: (f64, f64), q: (f64, f64)) -> f64 {
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt()
}

/// The larger of a match's transfer errors both ways, or infinity.
#[inline]
pub fn transfer_error(h: &Homography, inverse: &Homography, p: (f64, f64), q: (f64, f64)) -> f64 {
    match (map(h, p), map(inverse, q)) {
        (Some(hp), Some(iq)) => distance(hp, q).max(distance(iq, p)),
        _ => f64::INFINITY,
    }
}

/// The normalization of a frame's pixels: centred on the frame and scaled by its half-diagonal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Normalization {
    pub(crate) cx: f64,
    pub(crate) cy: f64,
    pub(crate) scale: f64,
}

impl Normalization {
    pub(crate) fn of(width: u32, height: u32) -> Self {
        Self {
            cx: (f64::from(width) - 1.0) / 2.0,
            cy: (f64::from(height) - 1.0) / 2.0,
            scale: (f64::from(width).powi(2) + f64::from(height).powi(2)).sqrt() / 2.0,
        }
    }

    fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        ((p.0 - self.cx) / self.scale, (p.1 - self.cy) / self.scale)
    }

    /// `T` with `T·p` the normalized point.
    fn matrix(&self) -> Homography {
        let s = 1.0 / self.scale;
        [
            [s, 0.0, -self.cx * s],
            [0.0, s, -self.cy * s],
            [0.0, 0.0, 1.0],
        ]
    }

    fn inverse(&self) -> Homography {
        [
            [self.scale, 0.0, self.cx],
            [0.0, self.scale, self.cy],
            [0.0, 0.0, 1.0],
        ]
    }
}

fn mul(a: &Homography, b: &Homography) -> Homography {
    linalg::mul(a, b)
}

/// The least-squares homography with `h₃₃ = 1` through normalized correspondences; `None` when
/// they do not fix one.
fn solve_homography(points: impl Iterator<Item = ((f64, f64), (f64, f64))>) -> Option<Homography> {
    let mut ata = [[0.0f64; 8]; 8];
    let mut atb = [0.0f64; 8];
    let mut count = 0;
    for ((x, y), (u, v)) in points {
        let rows = [
            ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
            ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
        ];
        for (row, rhs) in rows {
            for r in 0..8 {
                atb[r] += row[r] * rhs;
                for c in 0..8 {
                    ata[r][c] += row[r] * row[c];
                }
            }
        }
        count += 1;
    }
    if count < 4 {
        return None;
    }
    let h = linalg::solve(ata, atb)?;
    Some([[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]])
}

/// A verified-or-not homography fit of one pair's putative matches.
#[derive(Clone, Debug)]
pub(crate) struct HomographyFit {
    /// Maps the first frame's pixels to the second's.
    pub(crate) h: Homography,
    /// Indices into the putative matches.
    pub(crate) inliers: Vec<u32>,
    pub(crate) draws: usize,
}

/// RANSAC a homography through `points` (first frame's pixel, second's), both frames of the
/// normalization `norm`, with inliers within `threshold` pixels both ways. `None` when fewer than
/// four matches or no draw fits.
pub(crate) fn fit_homography(
    points: &[((f64, f64), (f64, f64))],
    norm: Normalization,
    threshold: f64,
    seed: u64,
    cancel: &Cancel,
) -> Result<Option<HomographyFit>, Error> {
    let n = points.len();
    if n < 4 {
        return Ok(None);
    }
    let normalized: Vec<_> = points
        .iter()
        .map(|&(p, q)| (norm.apply(p), norm.apply(q)))
        .collect();
    let to_pixels = |hn: &Homography| mul(&norm.inverse(), &mul(hn, &norm.matrix()));
    let count = |h: &Homography| -> (usize, f64) {
        let Some(inverse) = invert(h) else {
            return (0, 0.0);
        };
        let mut inliers = 0;
        let mut cost = 0.0;
        for &(p, q) in points {
            let e = transfer_error(h, &inverse, p, q);
            if e <= threshold {
                inliers += 1;
                cost += e * e;
            }
        }
        (inliers, cost)
    };
    let mut draws = Draws(seed);
    let mut best: Option<(Homography, usize, f64)> = None;
    let mut needed = MAX_RANSAC_ITERATIONS;
    let mut iteration = 0;
    while iteration < needed.min(MAX_RANSAC_ITERATIONS) {
        if iteration % CHECK_DRAWS == 0 {
            cancel.check()?;
        }
        iteration += 1;
        let mut sample = [0usize; 4];
        let mut k = 0;
        while k < 4 {
            let candidate = draws.below(n);
            if !sample[..k].contains(&candidate) {
                sample[k] = candidate;
                k += 1;
            }
        }
        let Some(hn) = solve_homography(sample.iter().map(|&i| normalized[i])) else {
            continue;
        };
        let h = to_pixels(&hn);
        let (inliers, cost) = count(&h);
        let better = match &best {
            None => inliers >= 4,
            Some((_, best_inliers, best_cost)) => {
                inliers > *best_inliers || (inliers == *best_inliers && cost < *best_cost)
            }
        };
        if better {
            best = Some((h, inliers, cost));
            let w = inliers as f64 / n as f64;
            let all = w.powi(4);
            needed = if all >= 1.0 {
                iteration
            } else if all <= 0.0 {
                MAX_RANSAC_ITERATIONS
            } else {
                ((1.0 - RANSAC_CONFIDENCE).ln() / (1.0 - all).ln())
                    .ceil()
                    .max(1.0) as usize
            };
        }
    }
    let Some((mut h, _, _)) = best else {
        return Ok(None);
    };
    let inliers_of = |h: &Homography| -> Vec<u32> {
        let Some(inverse) = invert(h) else {
            return Vec::new();
        };
        (0..n)
            .filter(|&i| transfer_error(h, &inverse, points[i].0, points[i].1) <= threshold)
            .map(|i| i as u32)
            .collect()
    };
    let mut inliers = inliers_of(&h);
    for _ in 0..2 {
        let Some(hn) = solve_homography(inliers.iter().map(|&i| normalized[i as usize])) else {
            break;
        };
        let refit = to_pixels(&hn);
        let refit_inliers = inliers_of(&refit);
        if refit_inliers.len() < inliers.len() {
            break;
        }
        h = refit;
        inliers = refit_inliers;
    }
    Ok(Some(HomographyFit {
        h,
        inliers,
        draws: iteration,
    }))
}

/// The focal lengths a homography between two frames of one camera implies, in its frames'
/// pixels, from the orthogonality of the rotation `K⁻¹·H·K` (Szeliski and Shum): the second
/// frame's from the columns, the first's from the rows, each where its equations allow one. `h`
/// acts on pixels centred on the principal point.
pub(crate) fn focals_from_homography(h: &Homography) -> (Option<f64>, Option<f64>) {
    let [[h0, h1, h2], [h3, h4, h5], [h6, h7, _]] = *h;
    let pick = |d1: f64, v1: f64, d2: f64, v2: f64| {
        let (v1, v2) = if v1 < v2 { (v2, v1) } else { (v1, v2) };
        let f2 = if v1 > 0.0 && v2 > 0.0 {
            if d1.abs() > d2.abs() {
                Some(v1)
            } else {
                Some(v2)
            }
        } else if v1 > 0.0 {
            Some(v1)
        } else {
            None
        };
        f2.map(f64::sqrt).filter(|f| f.is_finite() && *f > 0.0)
    };
    let (d1, d2) = (h6 * h7, (h7 - h6) * (h7 + h6));
    let second = pick(
        d1,
        -(h0 * h1 + h3 * h4) / d1,
        d2,
        (h0 * h0 + h3 * h3 - h1 * h1 - h4 * h4) / d2,
    );
    let (d1, d2) = (h0 * h3 + h1 * h4, h0 * h0 + h1 * h1 - h3 * h3 - h4 * h4);
    let first = pick(d1, -h2 * h5 / d1, d2, (h5 * h5 - h2 * h2) / d2);
    (first, second)
}

/// `h`, which acts on pixels, as it acts on pixels centred on `(cx, cy)` in both frames.
pub(crate) fn centred(h: &Homography, cx: f64, cy: f64) -> Homography {
    let to = [[1.0, 0.0, -cx], [0.0, 1.0, -cy], [0.0, 0.0, 1.0]];
    let from = [[1.0, 0.0, cx], [0.0, 1.0, cy], [0.0, 0.0, 1.0]];
    mul(&to, &mul(h, &from))
}
