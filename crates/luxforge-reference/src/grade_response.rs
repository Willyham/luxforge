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
use super::preview_error::{ciede2000, lab_from_linear};

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

/// The CIEDE2000 difference between two responses, each applied to the same neutral patch: the
/// colours both editors would reach from one common starting point, through the preview-error
/// measure's CIELAB and CIEDE2000 ([`crate::preview_error`]).
pub fn response_difference(neutral: [f64; 3], first: Response, second: Response) -> f64 {
    let base = colour::to_oklab(neutral);
    let reach = |response: Response| {
        colour::from_oklab(Oklab {
            l: base.l + response.dl,
            a: base.a + response.da,
            b: base.b + response.db,
        })
    };
    ciede2000(
        lab_from_linear(reach(first)),
        lab_from_linear(reach(second)),
    )
}

/// The median of a set of differences by nearest rank, the definition every figure in the
/// workspace reads (`luxforge_testbase::Distribution`, which this crate may not depend on): the
/// `ceil(n / 2)`th smallest. `NaN` for none.
pub fn median(values: &[f64]) -> f64 {
    median_in_place(&mut values.to_vec())
}

/// [`median`] of a buffer the caller owns, reordered in place rather than copied.
fn median_in_place(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let rank = values.len().div_ceil(2) - 1;
    *values.select_nth_unstable_by(rank, f64::total_cmp).1
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
        let mut responses = Vec::new();
        self.at_into(value, &mut responses).then_some(responses)
    }

    /// [`Self::at`] into a buffer the caller reuses: `false`, with `out` cleared, outside the
    /// sampled span.
    fn at_into(&self, value: f64, out: &mut Vec<Response>) -> bool {
        out.clear();
        let Some(upper) = self
            .samples
            .iter()
            .position(|(sampled, _)| *sampled >= value)
        else {
            return false;
        };
        let (high, high_responses) = &self.samples[upper];
        if *high == value {
            out.extend_from_slice(high_responses);
            return true;
        }
        if upper == 0 {
            return false;
        }
        let (low, low_responses) = &self.samples[upper - 1];
        let t = (value - low) / (high - low);
        out.extend(
            low_responses
                .iter()
                .zip(high_responses)
                .map(|(a, b)| Response {
                    dl: a.dl + t * (b.dl - a.dl),
                    da: a.da + t * (b.da - a.da),
                    db: a.db + t * (b.db - a.db),
                }),
        );
        true
    }
}

/// One sample a fit matches: its responses and the patches it is judged on, those it affects (its
/// own response at least [`AFFECTED`] from neutral) or every patch when it affects none, so a
/// setting that reaches only the shadows is judged on the shadows, not drowned by the unchanged
/// patches around them. The patches are found once per sample, not once per candidate.
struct Target<'a> {
    responses: &'a [Response],
    patches: Vec<usize>,
}

impl<'a> Target<'a> {
    fn new(neutrals: &[[f64; 3]], responses: &'a [Response]) -> Self {
        let affected: Vec<usize> = (0..responses.len().min(neutrals.len()))
            .filter(|&patch| {
                response_difference(neutrals[patch], Response::default(), responses[patch])
                    >= AFFECTED
            })
            .collect();
        let patches = if affected.is_empty() {
            (0..responses.len().min(neutrals.len())).collect()
        } else {
            affected
        };
        Self { responses, patches }
    }

    /// The median CIEDE2000 between `responses`, in the same patch order, and this target's over
    /// its patches, through a `scratch` buffer the caller reuses; infinite for `None`, a value
    /// outside the sampled span.
    fn distance(
        &self,
        neutrals: &[[f64; 3]],
        responses: Option<&[Response]>,
        scratch: &mut Vec<f64>,
    ) -> f64 {
        let Some(responses) = responses else {
            return f64::INFINITY;
        };
        scratch.clear();
        scratch.extend(self.patches.iter().map(|&patch| {
            response_difference(neutrals[patch], responses[patch], self.responses[patch])
        }));
        median_in_place(scratch)
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
    // Luxforge's responses at every candidate, interpolated once for all the targets, and each
    // target's patches, judged against Luxforge's neutrals as every difference is.
    let neutrals = &luxforge.neutrals;
    let at_candidates: Vec<Option<Vec<Response>>> =
        candidates.iter().map(|value| luxforge.at(*value)).collect();
    let targets: Vec<Target> = lightroom
        .samples
        .iter()
        .map(|(_, responses)| Target::new(neutrals, responses))
        .collect();
    let mut scratch = Vec::new();
    // Each target's distance at every candidate, searched once.
    let distances: Vec<Vec<f64>> = targets
        .iter()
        .map(|target| {
            at_candidates
                .iter()
                .map(|responses| target.distance(neutrals, responses.as_deref(), &mut scratch))
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
    // A pooled value may lie between candidates: its responses are interpolated again.
    let mut responses = Vec::new();
    let mut distance_at = |value: f64, target: &Target| {
        let inside = luxforge.at_into(value, &mut responses);
        target.distance(neutrals, inside.then_some(&responses[..]), &mut scratch)
    };
    lightroom
        .samples
        .iter()
        .zip(&targets)
        .zip(pooled)
        .zip(&distances)
        .map(|((((lightroom_value, _), target), value), row)| {
            let residual = distance_at(value, target);
            let at_end = value == low || value == high;
            let inward = if value == high {
                value - SEARCH_STEP
            } else {
                value + SEARCH_STEP
            };
            let shortfall =
                at_end && residual > SHORTFALL_RESIDUAL && distance_at(inward, target) > residual;
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
