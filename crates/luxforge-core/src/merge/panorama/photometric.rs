//! The stitch's photometric fit (`docs/design/panorama-merge.md`, "Exposure, colour and seams"):
//! one gain a frame (RAW) or a channel (JPEG), anchored on the reference at gain 1.
//!
//! **Samples.** On a sub-lattice of the stitch's plan grid (at most [`GAIN_LATTICE_CELLS`]
//! cells), every pair among the first [`MAX_CELL_FRAMES`] frames eligible at a cell contributes the
//! `log2` ratio of their colour-proxy samples there, when both are usable: no tap clipped, every
//! channel above [`GAIN_FLOOR`] and the luminance gradient at most [`GAIN_MAX_GRADIENT`] stops a
//! proxy pixel, so edges, misregistration and noise floors stay out.
//!
//! **Fit.** Each pair with at least [`MIN_GAIN_SAMPLES`] samples gives its median ratio `rᵢⱼ`
//! (per channel, or of the Rec. 709 luminance for one gain a frame), robust to a moving subject,
//! weighted by its sample count `wᵢⱼ` (at most [`MAX_PAIR_WEIGHT`]). The `log2` gains `aᵢ`
//! minimize `Σ wᵢⱼ·(aᵢ − aⱼ + rᵢⱼ)² + λ·Σ aᵢ²` with `a_ref = 0` and a vanishing `λ`
//! ([`GAIN_PRIOR`] of the largest weight) that keeps a frame with no usable overlap at gain 1.
//! Each frame records its gain, its pairs' weighted RMS residual in stops, its sample count and
//! the standard error of its `log2` gain, `σ·√(A⁻¹)ᵢᵢ` with `σ` the pairs' pooled robust spread.

use super::linalg;
use crate::merge::hdr::exposure::median_of_sorted;
use serde::{Deserialize, Serialize};

/// Plan cells sampled for gains, at most.
pub const GAIN_LATTICE_CELLS: usize = 1 << 16;
/// Frames compared at one cell, at most.
pub const MAX_CELL_FRAMES: usize = 4;
/// A sample's channels must exceed this linear value.
pub const GAIN_FLOOR: f32 = 1.0 / 4096.0;
/// A sample's luminance gradient, in stops a proxy pixel, may be at most this.
pub const GAIN_MAX_GRADIENT: f32 = 0.1;
/// A pair needs at least this many samples to count.
pub const MIN_GAIN_SAMPLES: usize = 16;
/// A pair weighs at most this many samples.
pub const MAX_PAIR_WEIGHT: f64 = 4096.0;
/// The prior that holds an unconstrained frame at gain 1, relative to the largest pair weight.
pub const GAIN_PRIOR: f64 = 1e-6;

/// How many gains a frame takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GainMode {
    /// One gain a frame: RAW frames share their white balance.
    #[default]
    PerFrame,
    /// One gain a channel: a JPEG's in-camera white balance may vary.
    PerChannel,
}

/// One frame's fitted gain and how well it is determined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameGain {
    /// Linear multipliers, red, green and blue; all equal for one gain a frame.
    pub gain: [f64; 3],
    /// The weighted RMS of its pairs' residuals, in stops.
    pub residual_stops: f64,
    /// Samples over its pairs.
    pub samples: u64,
    /// The standard error of its `log2` gain, in stops; 0 for the reference.
    pub uncertainty_stops: f64,
}

/// A usable sample of one frame at one cell: its linear RGB.
pub(crate) type GainSample = [f32; 3];

/// The pairs' collected `log2` ratios.
pub(crate) struct GainSamples {
    frames: usize,
    mode: GainMode,
    /// Per pair `(a < b)`, in pair order, its ratios: three per sample per channel, or one.
    ratios: Vec<Vec<f32>>,
}

/// The index of pair `(a, b)`, `a < b`, of `n` frames.
fn pair_index(n: usize, a: usize, b: usize) -> usize {
    a * n - a * (a + 1) / 2 + (b - a - 1)
}

fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

impl GainSamples {
    pub(crate) fn new(frames: usize, mode: GainMode) -> Self {
        Self {
            frames,
            mode,
            ratios: vec![Vec::new(); frames * frames.saturating_sub(1) / 2],
        }
    }

    /// Add one cell's usable samples, by frame in ascending order.
    pub(crate) fn add(&mut self, samples: &[(usize, GainSample)]) {
        let samples = &samples[..samples.len().min(MAX_CELL_FRAMES)];
        for (k, &(a, sa)) in samples.iter().enumerate() {
            for &(b, sb) in &samples[k + 1..] {
                let ratios = &mut self.ratios[pair_index(self.frames, a, b)];
                match self.mode {
                    GainMode::PerFrame => {
                        ratios.push((luminance(sa) / luminance(sb)).log2());
                    }
                    GainMode::PerChannel => {
                        for c in 0..3 {
                            ratios.push((sa[c] / sb[c]).log2());
                        }
                    }
                }
            }
        }
    }

    /// Fit the gains, anchored on `reference`. See the module documentation.
    pub(crate) fn solve(self, reference: usize) -> Vec<FrameGain> {
        let n = self.frames;
        let channels = match self.mode {
            GainMode::PerFrame => 1,
            GainMode::PerChannel => 3,
        };
        // Each usable pair: its frames, weight, median ratio and robust spread per channel.
        struct Pair {
            a: usize,
            b: usize,
            weight: f64,
            samples: u64,
            ratio: [f64; 3],
            spread: [f64; 3],
        }
        let mut pairs = Vec::new();
        for a in 0..n {
            for b in a + 1..n {
                let ratios = &self.ratios[pair_index(n, a, b)];
                let count = ratios.len() / channels;
                if count < MIN_GAIN_SAMPLES {
                    continue;
                }
                let mut ratio = [0.0; 3];
                let mut spread = [0.0; 3];
                for c in 0..channels {
                    let mut values: Vec<f64> = ratios
                        .iter()
                        .skip(c)
                        .step_by(channels)
                        .map(|&v| f64::from(v))
                        .collect();
                    values.sort_unstable_by(f64::total_cmp);
                    let centre = median_of_sorted(&values);
                    let mut deviations: Vec<f64> =
                        values.iter().map(|v| (v - centre).abs()).collect();
                    deviations.sort_unstable_by(f64::total_cmp);
                    ratio[c] = centre;
                    spread[c] = 1.4826 * median_of_sorted(&deviations);
                }
                pairs.push(Pair {
                    a,
                    b,
                    weight: (count as f64).min(MAX_PAIR_WEIGHT),
                    samples: count as u64,
                    ratio,
                    spread,
                });
            }
        }
        let largest = pairs.iter().map(|p| p.weight).fold(0.0, f64::max);
        let prior = GAIN_PRIOR * largest.max(1.0);
        // The unknowns are every frame but the reference.
        let unknown: Vec<Option<usize>> = {
            let mut next = 0;
            (0..n)
                .map(|i| {
                    (i != reference).then(|| {
                        next += 1;
                        next - 1
                    })
                })
                .collect()
        };
        let m = n - 1;
        let mut normal = vec![0.0; m * m];
        for p in &pairs {
            for x in [p.a, p.b] {
                if let Some(i) = unknown[x] {
                    normal[i * m + i] += p.weight;
                }
            }
            if let (Some(i), Some(j)) = (unknown[p.a], unknown[p.b]) {
                normal[i * m + j] -= p.weight;
                normal[j * m + i] -= p.weight;
            }
        }
        for i in 0..m {
            normal[i * m + i] += prior;
        }
        let mut log_gain = vec![[0.0_f64; 3]; n];
        #[allow(clippy::needless_range_loop)]
        for c in 0..channels {
            let mut rhs = vec![0.0; m];
            for p in &pairs {
                // aₐ − a_b = −r: the gradient of w·(aₐ − a_b + r)².
                if let Some(i) = unknown[p.a] {
                    rhs[i] -= p.weight * p.ratio[c];
                }
                if let Some(j) = unknown[p.b] {
                    rhs[j] += p.weight * p.ratio[c];
                }
            }
            let mut a = normal.clone();
            if let Some(x) = linalg::cholesky_solve(&mut a, &rhs, m) {
                for i in 0..n {
                    if let Some(k) = unknown[i] {
                        log_gain[i][c] = x[k];
                    }
                }
            }
        }
        // The inverse's diagonal, for each frame's standard error.
        let inverse_diagonal: Vec<f64> = (0..m)
            .map(|k| {
                let mut e = vec![0.0; m];
                e[k] = 1.0;
                let mut a = normal.clone();
                linalg::cholesky_solve(&mut a, &e, m).map_or(0.0, |x| x[k])
            })
            .collect();
        let pooled = {
            let (sum, weight) = pairs.iter().fold((0.0, 0.0), |(s, w), p| {
                let spread = p.spread[..channels].iter().sum::<f64>() / channels as f64;
                (s + p.weight * spread * spread, w + p.weight)
            });
            if weight > 0.0 {
                (sum / weight).sqrt()
            } else {
                0.0
            }
        };
        (0..n)
            .map(|i| {
                let (mut sum, mut weight, mut samples) = (0.0, 0.0, 0_u64);
                for p in pairs.iter().filter(|p| p.a == i || p.b == i) {
                    #[allow(clippy::needless_range_loop)]
                    for c in 0..channels {
                        let e = log_gain[p.a][c] - log_gain[p.b][c] + p.ratio[c];
                        sum += p.weight * e * e;
                        weight += p.weight;
                    }
                    samples += p.samples;
                }
                let gain = match self.mode {
                    GainMode::PerFrame => [log_gain[i][0].exp2(); 3],
                    GainMode::PerChannel => log_gain[i].map(f64::exp2),
                };
                FrameGain {
                    gain,
                    residual_stops: if weight > 0.0 {
                        (sum / weight).sqrt()
                    } else {
                        0.0
                    },
                    samples,
                    uncertainty_stops: unknown[i]
                        .map_or(0.0, |k| pooled * inverse_diagonal[k].max(0.0).sqrt()),
                }
            })
            .collect()
    }
}
