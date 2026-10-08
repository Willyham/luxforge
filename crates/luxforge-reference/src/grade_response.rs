//! Response measurement and fitting for the colour-grading alignment slice
//! (`docs/design/colour-grading.md`, "Final reference analysis and Lightroom refinement";
//! `docs/design/lightroom-alignment.md`, "Measuring a response, not a look").
//!
//! Every comparison is of **responses**: the change one setting makes to a patch, relative to the
//! same editor's rendering of the same patch at the setting's neutral value, so each editor's base
//! rendering cancels to first order. A response is measured in Oklab (`ΔL`, `Δa`, `Δb`) and two
//! responses are compared by CIEDE2000 between the colours they reach from one common neutral
//! patch, which is what the alignment programme's threshold is stated in.
//!
//! The fits are the alignment programme's level A translations for the grading fields: for each
//! Lightroom value, the Luxforge value whose response is closest over the round's patches, then
//! made monotone (for a magnitude) or kept circular (for a hue). Each reports its residual and any
//! range shortfall, never clamping. Pure functions of their inputs, with no rendering: the xtask
//! `grade-align` command renders and reads files, and calls these.

use super::colour::{self, Oklab};

/// One patch's change from its neutral rendering, in Oklab.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Response {
    pub dl: f64,
    pub da: f64,
    pub db: f64,
}

/// The change from `neutral` to `adjusted`, both linear sRGB patch means.
pub fn response(neutral: [f64; 3], adjusted: [f64; 3]) -> Response {
    let (n, a) = (colour::to_oklab(neutral), colour::to_oklab(adjusted));
    Response {
        dl: a.l - n.l,
        da: a.a - n.a,
        db: a.b - n.b,
    }
}

/// The mean of a patch's linear sRGB pixels.
pub fn patch_mean(pixels: impl IntoIterator<Item = [f64; 3]>) -> [f64; 3] {
    let (mut sum, mut count) = ([0.0; 3], 0usize);
    for pixel in pixels {
        for channel in 0..3 {
            sum[channel] += pixel[channel];
        }
        count += 1;
    }
    sum.map(|value| value / count.max(1) as f64)
}

// -------------------------------------------------------------------------------------------
// CIEDE2000
// -------------------------------------------------------------------------------------------

/// CIE `L*a*b*` (D65) of a linear sRGB colour.
pub fn lab_d65(rgb: [f64; 3]) -> [f64; 3] {
    let x = 0.412_456_4 * rgb[0] + 0.357_576_1 * rgb[1] + 0.180_437_5 * rgb[2];
    let y = 0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175 * rgb[2];
    let z = 0.019_333_9 * rgb[0] + 0.119_192 * rgb[1] + 0.950_304_1 * rgb[2];
    let white = [0.950_47, 1.0, 1.088_83];
    let f = |t: f64| {
        let delta: f64 = 6.0 / 29.0;
        if t > delta.powi(3) {
            t.cbrt()
        } else {
            t / (3.0 * delta * delta) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / white[0]), f(y / white[1]), f(z / white[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// The CIEDE2000 colour difference between two `L*a*b*` colours (Sharma, Wu and Dalal 2005), with
/// unit weighting factors.
pub fn delta_e_2000(first: [f64; 3], second: [f64; 3]) -> f64 {
    let [l1, a1, b1] = first;
    let [l2, a2, b2] = second;
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_bar = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (c_bar.powi(7) / (c_bar.powi(7) + 25f64.powi(7))).sqrt());
    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = a1p.hypot(b1);
    let c2p = a2p.hypot(b2);
    let hue = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        }
    };
    let h1p = hue(b1, a1p);
    let h2p = hue(b2, a2p);
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dhp = 2.0 * (c1p * c2p).sqrt() * (dh.to_radians() / 2.0).sin();
    let l_bar = (l1 + l2) / 2.0;
    let c_bar_p = (c1p + c2p) / 2.0;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (c_bar_p.powi(7) / (c_bar_p.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar_p;
    let sh = 1.0 + 0.015 * c_bar_p * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhp / sh).powi(2) + rt * (dc / sc) * (dhp / sh))
        .sqrt()
}

/// The CIEDE2000 difference between two responses, each applied to the same neutral patch: the
/// colours both editors would reach from one common starting point.
pub fn response_difference(neutral: [f64; 3], first: Response, second: Response) -> f64 {
    let base = colour::to_oklab(neutral);
    let reach = |response: Response| {
        colour::from_oklab(Oklab {
            l: base.l + response.dl,
            a: base.a + response.da,
            b: base.b + response.db,
        })
    };
    delta_e_2000(lab_d65(reach(first)), lab_d65(reach(second)))
}

/// The median of a set of differences; `NaN` for none.
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

// -------------------------------------------------------------------------------------------
// Fitting
// -------------------------------------------------------------------------------------------

/// One editor's responses to one setting: at each sampled value, one response per patch, in the
/// round's patch order, with each patch's neutral colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Sampled {
    pub neutrals: Vec<[f64; 3]>,
    /// `(value, responses)`, values strictly increasing.
    pub samples: Vec<(f64, Vec<Response>)>,
}

impl Sampled {
    /// The responses at `value`, linearly interpolated between the two sampled values around it;
    /// `None` outside the sampled span.
    pub fn at(&self, value: f64) -> Option<Vec<Response>> {
        let upper = self
            .samples
            .iter()
            .position(|(sampled, _)| *sampled >= value)?;
        let (high, high_responses) = &self.samples[upper];
        if *high == value || upper == 0 {
            return (*high == value).then(|| high_responses.clone());
        }
        let (low, low_responses) = &self.samples[upper - 1];
        let t = (value - low) / (high - low);
        Some(
            low_responses
                .iter()
                .zip(high_responses)
                .map(|(a, b)| Response {
                    dl: a.dl + t * (b.dl - a.dl),
                    da: a.da + t * (b.da - a.da),
                    db: a.db + t * (b.db - a.db),
                })
                .collect(),
        )
    }

    /// The median CIEDE2000 between these responses at `value` and `target`'s, over the patches
    /// the target affects (its own response at least [`AFFECTED`] from neutral), or over every patch
    /// when it affects none: a setting that reaches only the shadows is judged on the shadows, not
    /// drowned by the unchanged patches around them.
    fn distance(&self, value: f64, target: &[Response]) -> f64 {
        let Some(responses) = self.at(value) else {
            return f64::INFINITY;
        };
        let differences = |only_affected: bool| -> Vec<f64> {
            responses
                .iter()
                .zip(target)
                .zip(&self.neutrals)
                .filter(|((_, other), neutral)| {
                    !only_affected
                        || response_difference(**neutral, Response::default(), **other) >= AFFECTED
                })
                .map(|((own, other), neutral)| response_difference(*neutral, *own, *other))
                .collect()
        };
        let affected = differences(true);
        if affected.is_empty() {
            median(&differences(false))
        } else {
            median(&affected)
        }
    }
}

/// The CIEDE2000 from neutral at and above which a patch counts as affected by a setting.
pub const AFFECTED: f64 = 0.5;

/// One fitted point: a Lightroom value, the Luxforge value with the closest response, the median
/// CIEDE2000 left at that value, and whether the best value lies at the end of Luxforge's range,
/// still improving towards it and further than [`SHORTFALL_RESIDUAL`] from the target (a range
/// shortfall, reported and never clamped).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitPoint {
    pub lightroom: f64,
    pub luxforge: f64,
    pub residual: f64,
    pub shortfall: bool,
    /// The Luxforge values whose responses the measurement cannot tell from the best one (within
    /// [`TIE`] CIEDE2000, or [`HUE_TIE`] degrees for a hue): the fit's resolution on this round.
    pub span: [f64; 2],
}

/// How close, in CIEDE2000, a candidate's distance must be to the best one's to be
/// indistinguishable from it: far below a visible difference.
pub const TIE: f64 = 0.1;

/// The hue fit's counterpart of [`TIE`], in degrees.
pub const HUE_TIE: f64 = 0.5;

/// The step the fit searches Luxforge's range at.
const SEARCH_STEP: f64 = 0.25;

/// The median CIEDE2000 below which a best value at the end of Luxforge's range is a match, not a
/// shortfall: a quarter of the alignment programme's level C threshold of 2.
pub const SHORTFALL_RESIDUAL: f64 = 0.5;

/// The best Luxforge value for each Lightroom sample, searched over `luxforge`'s sampled span at
/// [`SEARCH_STEP`], then made non-decreasing by pooling adjacent violators (a magnitude's map is
/// monotone), with each point's residual re-measured at its pooled value.
pub fn fit_monotone(luxforge: &Sampled, lightroom: &Sampled) -> Vec<FitPoint> {
    let (Some((low, _)), Some((high, _))) = (luxforge.samples.first(), luxforge.samples.last())
    else {
        return Vec::new();
    };
    let (low, high) = (*low, *high);
    let steps = ((high - low) / SEARCH_STEP).round() as usize;
    let candidates: Vec<f64> = (0..=steps)
        .map(|step| (low + step as f64 * SEARCH_STEP).min(high))
        .collect();
    // Each target's distance at every candidate, searched once.
    let distances: Vec<Vec<f64>> = lightroom
        .samples
        .iter()
        .map(|(_, target)| {
            candidates
                .iter()
                .map(|value| luxforge.distance(*value, target))
                .collect()
        })
        .collect();
    let best: Vec<f64> = distances
        .iter()
        .map(|row| {
            row.iter()
                .zip(&candidates)
                .min_by(|a, b| a.0.total_cmp(b.0))
                .map_or(low, |(_, value)| *value)
        })
        .collect();
    // Pool adjacent violators: each block takes the mean of its values.
    let mut blocks: Vec<(f64, usize)> = Vec::new();
    for value in &best {
        blocks.push((*value, 1));
        while blocks.len() > 1 {
            let (last, count) = blocks[blocks.len() - 1];
            let (previous, previous_count) = blocks[blocks.len() - 2];
            if previous <= last {
                break;
            }
            blocks.pop();
            let merged = (previous * previous_count as f64 + last * count as f64)
                / (previous_count + count) as f64;
            *blocks.last_mut().expect("a block") = (merged, previous_count + count);
        }
    }
    let pooled = blocks
        .iter()
        .flat_map(|(value, count)| std::iter::repeat_n(*value, *count));
    lightroom
        .samples
        .iter()
        .zip(pooled)
        .zip(&distances)
        .map(|(((lightroom_value, target), value), row)| {
            let residual = luxforge.distance(value, target);
            let at_end = value == low || value == high;
            let inward = if value == high {
                value - SEARCH_STEP
            } else {
                value + SEARCH_STEP
            };
            let shortfall = at_end
                && residual > SHORTFALL_RESIDUAL
                && luxforge.distance(inward, target) > residual;
            let nearest = row.iter().copied().fold(f64::INFINITY, f64::min);
            let within: Vec<f64> = row
                .iter()
                .zip(&candidates)
                .filter(|(distance, _)| **distance <= nearest + TIE)
                .map(|(_, value)| *value)
                .collect();
            FitPoint {
                lightroom: *lightroom_value,
                luxforge: value,
                residual,
                shortfall,
                span: [
                    within.iter().copied().fold(value, f64::min),
                    within.iter().copied().fold(value, f64::max),
                ],
            }
        })
        .collect()
}

/// The direction, in degrees, of a tint response's `(Δa, Δb)`, averaged over the patches as a
/// vector so opposite patches do not cancel through the seam; `None` for no measurable tint.
pub fn tint_angle(responses: &[Response]) -> Option<f64> {
    let (a, b) = responses.iter().fold((0.0, 0.0), |(a, b), response| {
        (a + response.da, b + response.db)
    });
    (a.hypot(b) > 1e-9).then(|| b.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The signed circular difference `to - from` in degrees, in `(-180, 180]`.
pub fn circular_difference(from: f64, to: f64) -> f64 {
    let difference = (to - from).rem_euclid(360.0);
    if difference > 180.0 {
        difference - 360.0
    } else {
        difference
    }
}

/// A hue fit: for each Lightroom hue, the Luxforge hue whose tint points the same way, found
/// circularly over Luxforge's sampled hues, and the angle left between them. A hue map is not
/// made monotone across the seam; it is reported as measured.
pub fn fit_hue(luxforge: &Sampled, lightroom: &Sampled) -> Vec<FitPoint> {
    let angles: Vec<(f64, f64)> = luxforge
        .samples
        .iter()
        .filter_map(|(value, responses)| tint_angle(responses).map(|angle| (*value, angle)))
        .collect();
    lightroom
        .samples
        .iter()
        .filter_map(|(value, responses)| {
            let target = tint_angle(responses)?;
            let (best, angle) = angles.iter().min_by(|a, b| {
                circular_difference(a.1, target)
                    .abs()
                    .total_cmp(&circular_difference(b.1, target).abs())
            })?;
            let residual = circular_difference(*angle, target).abs();
            // The span runs both ways round from the best hue through every hue as close.
            let close: Vec<f64> = angles
                .iter()
                .filter(|(_, other)| {
                    circular_difference(*other, target).abs() <= residual + HUE_TIE
                })
                .map(|(hue, _)| circular_difference(*best, *hue))
                .collect();
            let (behind, ahead) = close
                .iter()
                .fold((0.0f64, 0.0f64), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
            Some(FitPoint {
                lightroom: *value,
                luxforge: *best,
                residual,
                shortfall: false,
                span: [best + behind, best + ahead],
            })
        })
        .collect()
}
