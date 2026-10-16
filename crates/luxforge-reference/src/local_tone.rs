//! Study prototypes for a content-adaptive Basic tone (`docs/design/basic-local-tone.md`): the
//! candidate operators the Highlights and Shadows study compares against Lightroom Classic's
//! rendered outputs, each a pure `f64` function of a whole linear-sRGB frame.
//!
//! Nothing in production calls these, and none is frozen: they exist so the study can measure each
//! candidate on the alignment rounds' charts and photographs. The candidate the owner chooses
//! becomes a frozen reference of its own, written for tile invariance as the Presence study's are
//! (direct summation in a fixed order, every reduced grid anchored at the frame origin); the
//! summed-area tables here are exact up to float summation order, which a study does not need to
//! hold bit for bit.
//!
//! Two operator shapes:
//!
//! - [`global`]: Basic's frozen tone curve ([`crate::tone`]) on every pixel, as the tone unit
//!   applies it today. Where it runs (before or after a RAW photograph's Look) is the caller's.
//! - [`local`]: a base and detail split. The encoded luminance `E` is smoothed by an edge-aware
//!   [`Smoother`] into a base `B`; the tone curve maps the base, and the detail `E - B` is added
//!   back scaled by `detail`: `E' = T(B) + detail * (E - B)`. A flat area's base is its own value,
//!   so there the operator is the global curve; a small region's base leans towards its
//!   surroundings as far as the smoother lets it, so its response follows them.
//!
//! The colour is rebuilt from the new luminance by one of two [`Reconstruction`]s: the tone
//! study's luminance ratio, which keeps chromaticity, or a ratio in Oklab lightness that keeps
//! Oklab `a` and `b`, so a darkened colour keeps its Oklab chroma.

use crate::colour::{Oklab, from_oklab, to_oklab};
use crate::srgb::{decode_encoded, encode_extended};
use crate::tone::{ToneParams, luminance, tone_curve};

/// Below this linear luminance (absolute value) the luminance ratio is replaced by the tone
/// study's additive rule.
pub const EPSILON_L: f64 = 1e-6;

/// A whole linear-sRGB frame, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f64; 3]>,
}

impl Frame {
    /// Each pixel's encoded luminance, `encode(L)` on the continued sRGB transfer.
    pub fn encoded_luminance(&self) -> Vec<f64> {
        self.rgb
            .iter()
            .map(|&p| encode_extended(luminance(p)))
            .collect()
    }
}

/// How a colour is rebuilt from its new luminance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconstruction {
    /// `rgb * L' / L`, the tone unit's rule: chromaticity kept, so Oklab chroma follows lightness.
    LuminanceRatio,
    /// Oklab lightness scaled by `(L' / L)^(1/3)`, `a` and `b` kept: a grey lands exactly on `L'`
    /// and a colour keeps its Oklab chroma.
    OklabChroma,
}

/// The colour `rgb` of luminance `l_in` rebuilt at luminance `l_out`.
pub fn reconstruct(rgb: [f64; 3], l_in: f64, l_out: f64, how: Reconstruction) -> [f64; 3] {
    if l_in.abs() < EPSILON_L {
        let delta = l_out - l_in;
        return rgb.map(|c| c + delta);
    }
    let ratio = l_out / l_in;
    match how {
        Reconstruction::LuminanceRatio => rgb.map(|c| c * ratio),
        Reconstruction::OklabChroma => {
            if ratio <= 0.0 {
                return rgb.map(|c| c * ratio);
            }
            let lab = to_oklab(rgb);
            from_oklab(Oklab {
                l: lab.l * ratio.cbrt(),
                a: lab.a,
                b: lab.b,
            })
        }
    }
}

/// The mean of `plane` over the square window of side `2 radius + 1` around each sample, the
/// window clipped at the frame's edges so each mean is over the samples it holds. `O(n)` through a
/// summed-area table, whatever the radius.
pub fn box_mean(plane: &[f64], width: usize, height: usize, radius: usize) -> Vec<f64> {
    assert_eq!(plane.len(), width * height, "a plane of another size");
    let stride = width + 1;
    let mut table = vec![0.0; stride * (height + 1)];
    for y in 0..height {
        let mut row = 0.0;
        for x in 0..width {
            row += plane[y * width + x];
            table[(y + 1) * stride + x + 1] = table[y * stride + x + 1] + row;
        }
    }
    let mut out = vec![0.0; width * height];
    for y in 0..height {
        let (y0, y1) = (y.saturating_sub(radius), (y + radius + 1).min(height));
        for x in 0..width {
            let (x0, x1) = (x.saturating_sub(radius), (x + radius + 1).min(width));
            let sum = table[y1 * stride + x1] - table[y0 * stride + x1] - table[y1 * stride + x0]
                + table[y0 * stride + x0];
            out[y * width + x] = sum / ((y1 - y0) * (x1 - x0)) as f64;
        }
    }
    out
}

/// He, Sun and Tang's guided filter with the plane as its own guide, over box windows of side
/// `2 radius + 1`: `a = var / (var + eps)`, `b = (1 - a) mean`, `q = mean(a) I + mean(b)`. A
/// window whose deviation is well above `sqrt(eps)` keeps its edge (`a` near 1); a flatter one is
/// smoothed to its mean.
pub fn guided_self(
    plane: &[f64],
    width: usize,
    height: usize,
    radius: usize,
    eps: f64,
) -> Vec<f64> {
    let (a, b) = guided_coefficients(plane, width, height, radius, eps);
    (0..plane.len()).map(|i| a[i] * plane[i] + b[i]).collect()
}

/// The self-guided filter's averaged linear coefficients `(mean(a), mean(b))`, so that the filter
/// is `mean(a) I + mean(b)`.
pub fn guided_coefficients(
    plane: &[f64],
    width: usize,
    height: usize,
    radius: usize,
    eps: f64,
) -> (Vec<f64>, Vec<f64>) {
    let mean = box_mean(plane, width, height, radius);
    let squares: Vec<f64> = plane.iter().map(|v| v * v).collect();
    let mean_sq = box_mean(&squares, width, height, radius);
    let mut a = vec![0.0; plane.len()];
    let mut b = vec![0.0; plane.len()];
    for i in 0..plane.len() {
        let var = (mean_sq[i] - mean[i] * mean[i]).max(0.0);
        a[i] = var / (var + eps);
        b[i] = (1.0 - a[i]) * mean[i];
    }
    (
        box_mean(&a, width, height, radius),
        box_mean(&b, width, height, radius),
    )
}

/// He and Sun's fast guided filter with the plane as its own guide: the coefficients computed on
/// the plane reduced so its long side is at most `long` samples, enlarged bilinearly and applied
/// to the full-resolution plane, `q = A I + B`. Edges the coefficients keep (`A` near 1) stay as
/// sharp as the plane's own, so a coarse grid makes no halo there; the coefficient planes are
/// small enough to compute once from a whole-frame reduction and read at any pixel.
pub fn fast_guided(
    plane: &[f64],
    width: usize,
    height: usize,
    long: usize,
    radius: f64,
    eps: f64,
) -> Vec<f64> {
    let factor = width.max(height).div_ceil(long.max(1)).max(1);
    let (small, rw, rh) = reduce(plane, width, height, factor);
    let r = radius_on(radius, width.max(height), factor);
    let (a, b) = guided_coefficients(&small, rw, rh, r, eps);
    let a = enlarge(&a, rw, rh, factor, width, height);
    let b = enlarge(&b, rw, rh, factor, width, height);
    (0..plane.len()).map(|i| a[i] * plane[i] + b[i]).collect()
}

/// `plane` reduced by `factor` on each axis: each reduced sample the mean of its block, a partial
/// block at the right or bottom edge averaged over its own samples. The reduced size.
pub fn reduce(
    plane: &[f64],
    width: usize,
    height: usize,
    factor: usize,
) -> (Vec<f64>, usize, usize) {
    assert!(factor >= 1, "a reduction by zero");
    let (rw, rh) = (width.div_ceil(factor), height.div_ceil(factor));
    let mut out = vec![0.0; rw * rh];
    for j in 0..rh {
        for i in 0..rw {
            let (x0, x1) = (i * factor, ((i + 1) * factor).min(width));
            let (y0, y1) = (j * factor, ((j + 1) * factor).min(height));
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += plane[y * width + x];
                }
            }
            out[j * rw + i] = sum / ((x1 - x0) * (y1 - y0)) as f64;
        }
    }
    (out, rw, rh)
}

/// A reduced plane enlarged bilinearly back to `width x height`: a sample at `x` reads reduced
/// coordinate `(x + 0.5) / factor - 0.5`, its two neighbours clamped to the reduced plane.
pub fn enlarge(
    reduced: &[f64],
    rw: usize,
    rh: usize,
    factor: usize,
    width: usize,
    height: usize,
) -> Vec<f64> {
    let coordinate = |x: usize, n: usize| -> (usize, usize, f64) {
        let u = ((x as f64 + 0.5) / factor as f64 - 0.5).max(0.0);
        let i = (u.floor() as usize).min(n - 1);
        let next = (i + 1).min(n - 1);
        (i, next, u - i as f64)
    };
    let mut out = vec![0.0; width * height];
    for y in 0..height {
        let (j0, j1, ty) = coordinate(y, rh);
        for x in 0..width {
            let (i0, i1, tx) = coordinate(x, rw);
            let top = reduced[j0 * rw + i0] * (1.0 - tx) + reduced[j0 * rw + i1] * tx;
            let bottom = reduced[j1 * rw + i0] * (1.0 - tx) + reduced[j1 * rw + i1] * tx;
            out[y * width + x] = top * (1.0 - ty) + bottom * ty;
        }
    }
    out
}

/// How a base is smoothed from the encoded luminance. Radii are fractions of the frame's long
/// side, so a base covers the same share of the picture at every size; each is at least one
/// sample of the grid it runs on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Smoother {
    /// The self-guided filter at `radius` with regularization `eps` (encoded units squared), run
    /// on the frame reduced by `reduction` and enlarged back.
    Guided {
        reduction: usize,
        radius: f64,
        eps: f64,
    },
    /// Two self-guided bases, fine and coarse, averaged with weight `coarse_weight` on the coarse:
    /// a base that follows small regions' surroundings less than large ones'.
    TwoScale {
        reduction: usize,
        fine: f64,
        coarse: f64,
        eps: f64,
        coarse_weight: f64,
    },
    /// A plain box mean at `radius` on the reduced frame: a base that follows no edge, the
    /// control the edge-aware ones are judged against.
    Box { reduction: usize, radius: f64 },
    /// [`fast_guided`]: coefficients on a grid of at most `long` samples on its long side.
    FastGuided { long: usize, radius: f64, eps: f64 },
}

/// A radius given as a fraction of `long` samples, on a grid reduced by `reduction`.
pub fn radius_on(fraction: f64, long: usize, reduction: usize) -> usize {
    ((fraction * long as f64 / reduction as f64).round() as usize).max(1)
}

/// The base of the encoded luminance `encoded` (`width x height`).
pub fn smooth(encoded: &[f64], width: usize, height: usize, smoother: Smoother) -> Vec<f64> {
    let long = width.max(height);
    match smoother {
        Smoother::Guided {
            reduction,
            radius,
            eps,
        } => {
            let (small, rw, rh) = reduce(encoded, width, height, reduction);
            let r = radius_on(radius, long, reduction);
            let base = guided_self(&small, rw, rh, r, eps);
            enlarge(&base, rw, rh, reduction, width, height)
        }
        Smoother::TwoScale {
            reduction,
            fine,
            coarse,
            eps,
            coarse_weight,
        } => {
            let (small, rw, rh) = reduce(encoded, width, height, reduction);
            let f = guided_self(&small, rw, rh, radius_on(fine, long, reduction), eps);
            let c = guided_self(&small, rw, rh, radius_on(coarse, long, reduction), eps);
            let base: Vec<f64> = f
                .iter()
                .zip(&c)
                .map(|(f, c)| (1.0 - coarse_weight) * f + coarse_weight * c)
                .collect();
            enlarge(&base, rw, rh, reduction, width, height)
        }
        Smoother::FastGuided { long, radius, eps } => {
            fast_guided(encoded, width, height, long, radius, eps)
        }
        Smoother::Box { reduction, radius } => {
            let (small, rw, rh) = reduce(encoded, width, height, reduction);
            let base = box_mean(&small, rw, rh, radius_on(radius, long, reduction));
            enlarge(&base, rw, rh, reduction, width, height)
        }
    }
}

/// The picture's own black and white points, in encoded luminance: the tone curve runs on
/// `(x - black) / (white - black)` and maps back, so its shape spans the picture's range rather
/// than `[0, 1]`. [`Levels::UNIT`] is today's curve exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    pub black: f64,
    pub white: f64,
}

impl Levels {
    pub const UNIT: Self = Self {
        black: 0.0,
        white: 1.0,
    };

    /// The levels at quantiles `black` and `white` of `encoded` (each `None` keeps 0 or 1). A
    /// span narrower than `MIN_SPAN` keeps the unit levels, so a flat frame is not stretched.
    pub fn of(encoded: &[f64], black: Option<f64>, white: Option<f64>) -> Self {
        const MIN_SPAN: f64 = 0.05;
        let mut sorted: Vec<f64> = encoded.iter().copied().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return Self::UNIT;
        }
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        let levels = Self {
            black: black.map_or(0.0, at),
            white: white.map_or(1.0, at),
        };
        if levels.white - levels.black < MIN_SPAN {
            Self::UNIT
        } else {
            levels
        }
    }

    /// The tone curve at `params` on `x`, within these levels.
    pub fn tone(self, x: f64, params: ToneParams) -> f64 {
        if self == Self::UNIT {
            return tone_curve(x, params);
        }
        let span = self.white - self.black;
        self.black + span * tone_curve((x - self.black) / span, params)
    }
}

/// Basic's frozen tone curve on every pixel, its colour rebuilt by `how`.
pub fn global(frame: &Frame, params: ToneParams, how: Reconstruction) -> Frame {
    global_within(frame, params, how, Levels::UNIT)
}

/// [`global`] within the picture's `levels`.
pub fn global_within(
    frame: &Frame,
    params: ToneParams,
    how: Reconstruction,
    levels: Levels,
) -> Frame {
    let rgb = frame
        .rgb
        .iter()
        .map(|&p| {
            let l = luminance(p);
            let out = decode_encoded(levels.tone(encode_extended(l), params));
            reconstruct(p, l, out, how)
        })
        .collect();
    Frame {
        width: frame.width,
        height: frame.height,
        rgb,
    }
}

/// A local operator's configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalConfig {
    pub smoother: Smoother,
    /// The gain on the detail `E - B`: 1 keeps local contrast as it was.
    pub detail: f64,
    pub reconstruction: Reconstruction,
}

/// The base and detail operator: `E' = T(B) + detail * (E - B)` with `T` Basic's tone curve at
/// `params` and `B` the base [`smooth`] gives, the colour rebuilt by the configuration's rule.
pub fn local(frame: &Frame, params: ToneParams, config: LocalConfig) -> Frame {
    let encoded = frame.encoded_luminance();
    let base = smooth(&encoded, frame.width, frame.height, config.smoother);
    local_over(frame, &encoded, &base, params, config)
}

/// [`local`] over a base already computed, so a sweep of strengths smooths once.
pub fn local_over(
    frame: &Frame,
    encoded: &[f64],
    base: &[f64],
    params: ToneParams,
    config: LocalConfig,
) -> Frame {
    local_within(frame, encoded, base, params, config, Levels::UNIT)
}

/// [`local_over`] within the picture's `levels`.
pub fn local_within(
    frame: &Frame,
    encoded: &[f64],
    base: &[f64],
    params: ToneParams,
    config: LocalConfig,
    levels: Levels,
) -> Frame {
    let rgb = frame
        .rgb
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let mapped = levels.tone(base[i], params) + config.detail * (encoded[i] - base[i]);
            reconstruct(
                p,
                luminance(p),
                decode_encoded(mapped),
                config.reconstruction,
            )
        })
        .collect();
    Frame {
        width: frame.width,
        height: frame.height,
        rgb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(width: usize, height: usize, f: impl Fn(usize, usize) -> f64) -> Frame {
        let mut rgb = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let v = f(x, y);
                rgb.push([v; 3]);
            }
        }
        Frame { width, height, rgb }
    }

    const HIGHLIGHTS: ToneParams = ToneParams {
        highlights: -100.0,
        ..ToneParams::NEUTRAL
    };

    #[test]
    fn box_mean_is_the_direct_mean_of_each_clipped_window() {
        let (w, h) = (7, 5);
        let plane: Vec<f64> = (0..w * h).map(|i| ((i * 37) % 11) as f64).collect();
        let fast = box_mean(&plane, w, h, 2);
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                let mut n = 0.0;
                for yy in y.saturating_sub(2)..(y + 3).min(h) {
                    for xx in x.saturating_sub(2)..(x + 3).min(w) {
                        sum += plane[yy * w + xx];
                        n += 1.0;
                    }
                }
                assert!((fast[y * w + x] - sum / n).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn the_guided_filter_keeps_a_strong_edge_and_flattens_a_weak_one() {
        let (w, h) = (40, 4);
        let step = |low: f64, high: f64| -> Vec<f64> {
            (0..w * h)
                .map(|i| if i % w < w / 2 { low } else { high })
                .collect()
        };
        // A step of 0.6 against eps 1e-4: the edge survives within a percent of the step.
        let strong = guided_self(&step(0.2, 0.8), w, h, 4, 1e-4);
        assert!((strong[h / 2 * w + w / 2 - 1] - 0.2).abs() < 0.006);
        assert!((strong[h / 2 * w + w / 2] - 0.8).abs() < 0.006);
        // A step of 0.02 against eps 1: `a` is near 0, so the filter is the mean of the box means.
        let weak = guided_self(&step(0.5, 0.52), w, h, 4, 1.0);
        let mean = box_mean(&box_mean(&step(0.5, 0.52), w, h, 4), w, h, 4);
        let worst = weak
            .iter()
            .zip(&mean)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(worst < 1e-3, "{worst}");
    }

    #[test]
    fn the_fast_guided_filter_without_reduction_is_the_guided_filter() {
        let (w, h) = (30, 20);
        let plane: Vec<f64> = (0..w * h)
            .map(|i| ((i * 13) % 17) as f64 / 17.0 + if i % w > 14 { 0.5 } else { 0.0 })
            .collect();
        let radius = 3.0 / 30.0;
        let direct = guided_self(&plane, w, h, radius_on(radius, w, 1), 0.01);
        let fast = fast_guided(&plane, w, h, 30, radius, 0.01);
        for (a, b) in direct.iter().zip(&fast) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn the_fast_guided_filter_keeps_a_strong_edge_at_full_resolution() {
        // A step of 0.6 between columns 63 and 64, filtered on a grid 8 times coarser. The
        // coefficients' enlargement mixes the two sides' offsets near the edge, so each side
        // moves a little towards the other, but the step stays a one-pixel step: most of it lies
        // between the two pixels, where a base enlarged from the coarse grid would spread it over
        // eight.
        let (w, h) = (128, 16);
        let plane: Vec<f64> = (0..w * h)
            .map(|i| if i % w < 64 { 0.2 } else { 0.8 })
            .collect();
        let out = fast_guided(&plane, w, h, 16, 0.1, 1e-4);
        let row = 8 * w;
        let step = out[row + 64] - out[row + 63];
        assert!(step > 0.8 * 0.6, "{step}");
        assert!((out[row + 56] - 0.2).abs() < 0.06 && (out[row + 71] - 0.8).abs() < 0.06);
    }

    #[test]
    fn reduce_and_enlarge_keep_a_constant_and_a_linear_ramp() {
        let (w, h) = (24, 16);
        let constant = vec![0.37; w * h];
        let (small, rw, rh) = reduce(&constant, w, h, 4);
        assert!(small.iter().all(|v| (v - 0.37).abs() < 1e-15));
        assert!(
            enlarge(&small, rw, rh, 4, w, h)
                .iter()
                .all(|v| (v - 0.37).abs() < 1e-15)
        );
        // A ramp along x: its block means are the ramp at block centres, and the bilinear
        // enlargement reproduces the ramp away from the clamped edges.
        let ramp: Vec<f64> = (0..w * h).map(|i| (i % w) as f64).collect();
        let (small, rw, rh) = reduce(&ramp, w, h, 4);
        let back = enlarge(&small, rw, rh, 4, w, h);
        for (x, v) in back.iter().enumerate().take(w - 2).skip(2) {
            assert!((v - x as f64).abs() < 1e-12, "x {x}: {v}");
        }
    }

    #[test]
    fn the_global_operator_is_the_tone_unit_on_every_pixel() {
        let frame = grey(16, 1, |x, _| x as f64 / 15.0);
        let out = global(&frame, HIGHLIGHTS, Reconstruction::LuminanceRatio);
        for (p, q) in frame.rgb.iter().zip(&out.rgb) {
            let expected = crate::tone::tone_pixel(*p, HIGHLIGHTS);
            for c in 0..3 {
                assert!((q[c] - expected[c]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn on_a_flat_frame_the_local_operator_is_the_global_one() {
        let frame = grey(32, 32, |_, _| 0.6);
        let config = LocalConfig {
            smoother: Smoother::Guided {
                reduction: 4,
                radius: 0.1,
                eps: 0.01,
            },
            detail: 1.0,
            reconstruction: Reconstruction::LuminanceRatio,
        };
        let local = local(&frame, HIGHLIGHTS, config);
        let global = global(&frame, HIGHLIGHTS, Reconstruction::LuminanceRatio);
        for (a, b) in local.rgb.iter().zip(&global.rgb) {
            assert!((a[0] - b[0]).abs() < 1e-12);
        }
    }

    #[test]
    fn neutral_parameters_leave_every_frame_unchanged() {
        let frame = grey(20, 12, |x, y| ((x * 7 + y * 3) % 10) as f64 / 10.0);
        for smoother in [
            Smoother::Guided {
                reduction: 2,
                radius: 0.2,
                eps: 0.01,
            },
            Smoother::Box {
                reduction: 2,
                radius: 0.2,
            },
        ] {
            let config = LocalConfig {
                smoother,
                detail: 1.0,
                reconstruction: Reconstruction::LuminanceRatio,
            };
            let out = local(&frame, ToneParams::NEUTRAL, config);
            for (a, b) in out.rgb.iter().zip(&frame.rgb) {
                assert!((a[0] - b[0]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn a_patch_follows_its_surround_through_a_box_base_and_keeps_its_own_through_an_edge_aware_one()
    {
        // A mid-grey patch on a dark and on a light surround. Highlights -100 lowers the light end:
        // through a base that follows no edge, the patch on the light surround is lowered more;
        // through a base that keeps the edge, both patches move alike.
        let patch = |surround: f64| {
            grey(64, 64, move |x, y| {
                if (24..40).contains(&x) && (24..40).contains(&y) {
                    0.2
                } else {
                    surround
                }
            })
        };
        let centre = 32 * 64 + 32;
        let response = |smoother: Smoother, surround: f64| {
            let frame = patch(surround);
            let config = LocalConfig {
                smoother,
                detail: 1.0,
                reconstruction: Reconstruction::LuminanceRatio,
            };
            local(&frame, HIGHLIGHTS, config).rgb[centre][0] - frame.rgb[centre][0]
        };
        let blur = Smoother::Box {
            reduction: 1,
            radius: 0.5,
        };
        assert!(response(blur, 0.9) < response(blur, 0.01) - 0.01);
        let edges = Smoother::Guided {
            reduction: 1,
            radius: 0.5,
            eps: 1e-5,
        };
        assert!((response(edges, 0.9) - response(edges, 0.01)).abs() < 2e-3);
    }

    #[test]
    fn unit_levels_are_the_curve_and_levels_map_their_ends_through_the_curves_ends() {
        let params = ToneParams {
            highlights: -100.0,
            shadows: 60.0,
            ..ToneParams::NEUTRAL
        };
        for x in [0.0, 0.2, 0.5, 0.9, 1.0] {
            assert_eq!(Levels::UNIT.tone(x, params), tone_curve(x, params));
        }
        // Highlights and Shadows hold 0 and 1, so within levels they hold the levels' ends; a
        // value halfway between them moves as the curve moves 0.5, scaled by the span.
        let levels = Levels {
            black: 0.1,
            white: 0.7,
        };
        assert!((levels.tone(0.1, params) - 0.1).abs() < 1e-12);
        assert!((levels.tone(0.7, params) - 0.7).abs() < 1e-12);
        let mid = levels.tone(0.4, params) - 0.4;
        assert!((mid - 0.6 * (tone_curve(0.5, params) - 0.5)).abs() < 1e-12);
    }

    #[test]
    fn levels_are_read_at_their_quantiles_and_a_flat_frame_keeps_the_unit_levels() {
        let ramp: Vec<f64> = (0..=100)
            .map(|i| f64::from(i) / 100.0 * 0.8 + 0.1)
            .collect();
        let levels = Levels::of(&ramp, Some(0.0), Some(1.0));
        assert!((levels.black - 0.1).abs() < 1e-12 && (levels.white - 0.9).abs() < 1e-12);
        let levels = Levels::of(&ramp, None, Some(0.5));
        assert_eq!(levels.black, 0.0);
        assert!((levels.white - 0.5).abs() < 1e-12);
        assert_eq!(Levels::of(&[0.4; 10], Some(0.0), Some(1.0)), Levels::UNIT);
    }

    #[test]
    fn the_oklab_reconstruction_keeps_chroma_and_lands_a_grey_on_its_luminance() {
        // Oklab's lightness of a grey is the cube root of its luminance to within its matrices'
        // rounding, so a grey lands on its new luminance to a part in ten thousand.
        let grey = [0.3; 3];
        let out = reconstruct(grey, 0.3, 0.15, Reconstruction::OklabChroma);
        assert!(
            (luminance(out) / 0.15 - 1.0).abs() < 1e-4,
            "{}",
            luminance(out)
        );
        let colour = [0.6, 0.2, 0.1];
        let l = luminance(colour);
        let out = reconstruct(colour, l, l * 0.5, Reconstruction::OklabChroma);
        let (a, b) = (to_oklab(colour), to_oklab(out));
        assert!(
            (a.a - b.a).abs() < 1e-6 && (a.b - b.b).abs() < 1e-6,
            "{a:?} {b:?}"
        );
    }
}
