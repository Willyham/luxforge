//! Linear radiance fusion of one rectangle in reference coordinates.
//!
//! For each pixel, input `i` with offset `o_i`, scale `s_i = 2^−o_i`, linear sample `x_i` and
//! sensor level fraction `f_i`:
//!
//! ```text
//! h(f)  = 1                       f ≤ a
//!       = u²(3 − 2u), u = (b − f)/(b − a)   a < f < b
//!       = 0                       f ≥ b, or clipped
//! w_i   = 2^o_i · h(f_i)
//! fused = Σ w_i · s_i · x_i / Σ w_i            (every channel, signs and headroom kept)
//! ```
//!
//! `a` and `b` are [`FusionParameters::rolloff_start`] and [`FusionParameters::rolloff_end`].
//! The `2^o_i` factor is the inverse variance of a shot-noise-limited normalized sample, so a
//! longer exposure dominates wherever its sensor recorded the light; `h` hands a highlight to the
//! shorter exposures before the longer one reaches its sensor ceiling. Weights are nonnegative
//! and the normalization is the explicit division. Where every weight is zero (every input at or
//! past the roll-off end), the pixel keeps the shortest exposure's normalized sample, the largest
//! lower bound any input recorded, and is flagged [`PixelLimits::ALL_INVALID`]: no black, no
//! infinity, no synthesized highlight.
//!
//! With a motion selection of input `k` at confidence `c`, the pixel is
//! `(1 − c) · fused + c · s_k · x_k`. The arithmetic is `f64` in input order, rounded once to
//! `f32`, so one rectangle's result is bit-identical on every run and independent of how the
//! frame is split into rectangles.

use super::exposure::ResolvedExposures;
use super::frame::{FrameRect, MAX_INPUTS, MIN_INPUTS, MotionSelection, SensorLevel, view_len};
use crate::Error;
use serde::{Deserialize, Serialize};

/// The most pixels one fusion rectangle may hold: a 1024 × 1024 chunk, whose nine inputs' samples
/// and levels (126 MiB) and output (12 MiB) stay inside the merge's 256 MiB scratch.
pub const MAX_FUSION_PIXELS: usize = 1 << 20;

/// The weight parameters a merge records with its result.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FusionParameters {
    /// The sensor-level fraction where a sample's weight starts to fall.
    pub rolloff_start: f64,
    /// The fraction where it reaches zero.
    pub rolloff_end: f64,
    /// A reference sample below this fraction is in the shadows, for the range diagnostics.
    pub shadow_level: f64,
}

impl Default for FusionParameters {
    fn default() -> Self {
        Self {
            rolloff_start: 0.8,
            rolloff_end: 0.95,
            shadow_level: 1.0 / 64.0,
        }
    }
}

impl FusionParameters {
    pub fn validate(&self) -> Result<(), Error> {
        let Self {
            rolloff_start: a,
            rolloff_end: b,
            shadow_level: s,
        } = *self;
        if [a, b, s].iter().all(|v| v.is_finite())
            && 0.0 < s
            && s < a
            && a < b
            && b <= SensorLevel::CLIP_FRACTION
        {
            return Ok(());
        }
        Err(Error::validation(
            "HDR fusion needs 0 < shadow level < roll-off start < roll-off end <= 0.999",
        ))
    }

    /// The highlight factor `h` of a sample at `level`.
    pub fn highlight_weight(&self, level: SensorLevel) -> f64 {
        if level.is_clipped() {
            return 0.0;
        }
        let f = level.fraction();
        if f <= self.rolloff_start {
            1.0
        } else if f >= self.rolloff_end {
            0.0
        } else {
            let u = (self.rolloff_end - f) / (self.rolloff_end - self.rolloff_start);
            u * u * (3.0 - 2.0 * u)
        }
    }
}

/// A pixel's limitations, a bit set.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelLimits(pub u8);

impl PixelLimits {
    /// No input recorded a usable sample: the shortest exposure's lower bound stands.
    pub const ALL_INVALID: u8 = 1;
    /// Motion selection feathered this pixel.
    pub const MOTION: u8 = 2;
    /// The motion selection chose a sample past the roll-off end.
    pub const MOTION_SELECTION_INVALID: u8 = 4;

    pub fn contains(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// Where fusion writes one rectangle: three float32 planes and, optionally, each pixel's
/// [`PixelLimits`], `width × height` at `stride`.
#[derive(Debug)]
pub struct FusedRect<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub rgb: [&'a mut [f32]; 3],
    pub limits: Option<&'a mut [PixelLimits]>,
}

/// Counts over the pixels fused so far. Integer counts only, so aggregation in any order is
/// exact.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FusionDiagnostics {
    pub pixels: u64,
    /// Pixels no input recorded usably.
    pub all_invalid: u64,
    /// Pixels where the reference is in its roll-off or clipped and a shorter exposure supplied
    /// a usable sample.
    pub highlight_extended: u64,
    /// Pixels where the reference is below the shadow level and a longer exposure supplied a
    /// usable sample above it.
    pub shadow_extended: u64,
    pub motion: u64,
    pub motion_selection_invalid: u64,
    /// Per input, the pixels where it carried weight.
    pub contributing: [u64; MAX_INPUTS],
    /// Per input, the motion pixels that selected it.
    pub motion_selected: [u64; MAX_INPUTS],
}

/// Whether the merge recorded any range the reference did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RangeOutcome {
    Extended,
    /// No pixel gained highlight or shadow range over the reference: the merge recovers nothing
    /// a single exposure lacked.
    NoAdditionalRange,
}

/// The limitations a merge reports for its whole result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limitations {
    /// Some pixels were saturated in every exposure.
    pub all_invalid: bool,
    /// Some pixels came from one exposure for motion and may lack HDR coverage.
    pub motion: bool,
    /// Some motion pixels selected a saturated sample.
    pub motion_selection_invalid: bool,
    pub no_additional_range: bool,
}

impl FusionDiagnostics {
    /// Add another rectangle's counts.
    pub fn add(&mut self, other: &Self) {
        self.pixels += other.pixels;
        self.all_invalid += other.all_invalid;
        self.highlight_extended += other.highlight_extended;
        self.shadow_extended += other.shadow_extended;
        self.motion += other.motion;
        self.motion_selection_invalid += other.motion_selection_invalid;
        for i in 0..MAX_INPUTS {
            self.contributing[i] += other.contributing[i];
            self.motion_selected[i] += other.motion_selected[i];
        }
    }

    pub fn outcome(&self) -> RangeOutcome {
        if self.highlight_extended + self.shadow_extended > 0 {
            RangeOutcome::Extended
        } else {
            RangeOutcome::NoAdditionalRange
        }
    }

    pub fn limitations(&self) -> Limitations {
        Limitations {
            all_invalid: self.all_invalid > 0,
            motion: self.motion > 0,
            motion_selection_invalid: self.motion_selection_invalid > 0,
            no_additional_range: self.outcome() == RangeOutcome::NoAdditionalRange,
        }
    }
}

/// Fuse one rectangle: `inputs` in request order, each aligned to the reference over the same
/// `width × height`, with `exposures` resolved for the same request. On an error (mismatched
/// views, a non-finite sample, an output beyond float32) the output is partially written and
/// must be discarded.
pub fn fuse_rect(
    exposures: &ResolvedExposures,
    parameters: &FusionParameters,
    inputs: &[FrameRect<'_>],
    motion: Option<&MotionSelection<'_>>,
    output: &mut FusedRect<'_>,
) -> Result<FusionDiagnostics, Error> {
    parameters.validate()?;
    let n = inputs.len();
    if !(MIN_INPUTS..=MAX_INPUTS).contains(&n) || exposures.inputs.len() != n {
        return Err(Error::validation(
            "HDR fusion takes one rectangle per resolved input",
        ));
    }
    let (width, height) = (output.width, output.height);
    if width
        .checked_mul(height)
        .is_none_or(|pixels| pixels > MAX_FUSION_PIXELS)
    {
        return Err(Error::resource_limit(
            "HDR fusion rectangle exceeds 1 Mi pixels",
        ));
    }
    for input in inputs {
        input.check()?;
        if (input.width, input.height) != (width, height) {
            return Err(Error::validation("HDR fusion rectangles differ in size"));
        }
    }
    let need = view_len(width, height, output.stride)
        .filter(|_| output.stride >= width)
        .ok_or_else(|| Error::validation("HDR output stride is narrower than a row"))?;
    if output.rgb.iter().any(|plane| plane.len() < need)
        || output
            .limits
            .as_ref()
            .is_some_and(|limits| limits.len() < need)
    {
        return Err(Error::validation(
            "HDR output slices are shorter than its dimensions",
        ));
    }
    if let Some(motion) = motion {
        if (motion.width, motion.height) != (width, height) {
            return Err(Error::validation(
                "motion selection differs from the rectangle",
            ));
        }
        motion.check(n)?;
    }
    let reference = exposures.reference;
    let mut exposure = [0.0_f64; MAX_INPUTS];
    let mut scale = [0.0_f64; MAX_INPUTS];
    for (i, resolved) in exposures.inputs.iter().enumerate() {
        exposure[i] = resolved.offset_ev.exp2();
        scale[i] = resolved.scale;
    }
    // The fallback: the shortest exposure, the lowest position among equals.
    let shortest = (0..n)
        .min_by(|&a, &b| {
            exposures.inputs[a]
                .offset_ev
                .total_cmp(&exposures.inputs[b].offset_ev)
                .then(a.cmp(&b))
        })
        .unwrap_or(reference);
    let mut diagnostics = FusionDiagnostics::default();
    let mut samples = [[0.0_f64; 3]; MAX_INPUTS];
    let mut weights = [0.0_f64; MAX_INPUTS];
    let mut highlight = [0.0_f64; MAX_INPUTS];
    let mut fractions = [0.0_f64; MAX_INPUTS];
    for y in 0..height {
        for x in 0..width {
            let mut limits = 0_u8;
            let mut sum = 0.0_f64;
            let mut acc = [0.0_f64; 3];
            for (i, input) in inputs.iter().enumerate() {
                let (rgb, level) = input.at(x, y);
                if !rgb.iter().all(|value| value.is_finite()) {
                    return Err(Error::validation("HDR input sample is not finite"));
                }
                let normalized = rgb.map(|value| scale[i] * f64::from(value));
                highlight[i] = parameters.highlight_weight(level);
                let w = exposure[i] * highlight[i];
                samples[i] = normalized;
                weights[i] = w;
                fractions[i] = level.fraction();
                if w > 0.0 {
                    sum += w;
                    for c in 0..3 {
                        acc[c] += w * normalized[c];
                    }
                    diagnostics.contributing[i] += 1;
                }
            }
            let mut fused = if sum > 0.0 {
                acc.map(|value| value / sum)
            } else {
                limits |= PixelLimits::ALL_INVALID;
                diagnostics.all_invalid += 1;
                samples[shortest]
            };
            if highlight[reference] < 1.0
                && (0..n).any(|i| exposures.inputs[i].offset_ev < 0.0 && weights[i] > 0.0)
            {
                diagnostics.highlight_extended += 1;
            }
            if fractions[reference] < parameters.shadow_level
                && (0..n).any(|i| {
                    exposures.inputs[i].offset_ev > 0.0
                        && weights[i] > 0.0
                        && fractions[i] >= parameters.shadow_level
                })
            {
                diagnostics.shadow_extended += 1;
            }
            if let Some(motion) = motion {
                let m = y * motion.stride + x;
                let c = f64::from(motion.confidence[m]);
                if c > 0.0 {
                    let k = usize::from(motion.selected[m]);
                    limits |= PixelLimits::MOTION;
                    diagnostics.motion += 1;
                    diagnostics.motion_selected[k] += 1;
                    if weights[k] == 0.0 {
                        limits |= PixelLimits::MOTION_SELECTION_INVALID;
                        diagnostics.motion_selection_invalid += 1;
                    }
                    for (channel, value) in fused.iter_mut().enumerate() {
                        *value = (1.0 - c) * *value + c * samples[k][channel];
                    }
                }
            }
            let o = y * output.stride + x;
            for (channel, plane) in output.rgb.iter_mut().enumerate() {
                let value = fused[channel] as f32;
                if !value.is_finite() {
                    return Err(Error::validation("HDR fused value exceeds float32"));
                }
                plane[o] = value;
            }
            if let Some(out) = output.limits.as_deref_mut() {
                out[o] = PixelLimits(limits);
            }
            diagnostics.pixels += 1;
        }
    }
    Ok(diagnostics)
}
