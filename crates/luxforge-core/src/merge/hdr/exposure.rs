//! Exposure resolution: one deterministic reference frame and every input's effective EV offset
//! from it, each with its origin.
//!
//! An offset `o` is in stops of exposure: `+1` recorded twice the light of the reference, and
//! its samples are multiplied by `scale = 2^−o` to reach the reference's brightness.
//!
//! - **Metadata.** The settings exposure is `log2(t)` (shutter time; aperture and ISO are equal
//!   across a valid bracket, so their terms cancel), used when every input records a time. Else
//!   the recorded bias, when every input records one. Never the sum of the two. Settings and bias
//!   are contradictory when both are complete and the bias varies but differs from the settings
//!   by more than [`CONTRADICTION_EV`] for some input (or the settings do not vary at all).
//! - **Reference.** The requested input; else the unique input with a zero recorded bias; else
//!   the median metadata exposure (the lower middle for an even count), capture order breaking
//!   ties; else, when every input has an override, the one input overridden to 0 EV; else a
//!   provisional reference, the capture-order middle, which a measured fit replaces with the
//!   median measured exposure.
//! - **Overrides** are offsets from the reference, one optional value per input; the reference's
//!   own must be 0.
//! - **Measured fit** ([`FitPlan`]). For absent or contradictory metadata, every input without an
//!   override is measured from bounded aligned samples ([`FitSamples`]).
//!
//! Every resolved bracket has offsets within [`MAX_OFFSET_EV`] of the reference and at least
//! [`MIN_DISTINCT_EV`] apart.

use super::frame::{
    FrameMetadata, FrameRect, MAX_INPUTS, Refusal, SensorLevel, capture_order, validate_bracket,
};
use crate::Error;
use serde::{Deserialize, Serialize};

/// The smallest step between two inputs' effective exposures: a third-stop bracket's nominal
/// step less the 0.13 EV rounding cameras record (as bracket detection allows).
pub const MIN_DISTINCT_EV: f64 = 0.2;
/// The largest offset from the reference.
pub const MAX_OFFSET_EV: f64 = 16.0;
/// How far the bias may disagree with the settings before the metadata is contradictory: well
/// beyond the nominal rounding of either.
pub const CONTRADICTION_EV: f64 = 0.5;
/// A recorded bias this close to zero is the metered frame's.
const ZERO_BIAS_EV: f64 = 1e-3;
/// An override this close to zero names the reference.
const ZERO_OVERRIDE_EV: f64 = 1e-9;

/// The most sample points a fit reads: 64 Ki points of up to nine luminances and levels each,
/// 3.4 MiB at most.
pub const MAX_FIT_SAMPLES: usize = 65_536;
/// The fewest shared usable points that measure one pair of inputs.
pub const MIN_PAIR_SAMPLES: usize = 64;
/// A sample is usable for a fit between these sensor-level fractions: above the noise floor and
/// below the fusion's highlight roll-off, so neither noise nor near-clipping biases a ratio.
pub const FIT_FLOOR: f64 = 1.0 / 256.0;
pub const FIT_CEILING: f64 = 0.8;
/// A pair whose log ratios spread by more than this robust deviation, or a solution whose
/// residual exceeds [`MAX_FIT_RESIDUAL_EV`], is ambiguous.
pub const AMBIGUOUS_SPREAD_EV: f64 = 0.25;
pub const MAX_FIT_RESIDUAL_EV: f64 = 0.1;

/// The person's optional choices: a reference and per-input offsets from it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExposureOptions {
    pub reference: Option<usize>,
    /// Empty for none, else one optional offset per input.
    #[serde(default)]
    pub overrides: Vec<Option<f64>>,
}

/// Which metadata encoded the bracket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetadataSource {
    /// Shutter time, with equal aperture and ISO.
    Settings,
    /// The recorded exposure bias.
    Bias,
}

/// Where an offset came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ExposureOrigin {
    /// The reference itself, at 0 EV by definition.
    Reference,
    Metadata {
        source: MetadataSource,
    },
    /// Measured from aligned samples: the input's RMS pairwise residual and largest pair spread
    /// in EV, a confidence in `[0, 1]` and how many pairwise sample observations it rests on.
    Fit {
        residual_ev: f64,
        spread_ev: f64,
        confidence: f64,
        samples: u32,
    },
    Override,
}

/// How the reference was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceOrigin {
    Requested,
    ZeroBias,
    MedianExposure,
    ZeroOverride,
    MedianFit,
}

/// One input's resolved exposure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedExposure {
    /// Stops of exposure relative to the reference.
    pub offset_ev: f64,
    /// `2^−offset_ev`: the factor bringing its samples to the reference's brightness.
    pub scale: f64,
    pub origin: ExposureOrigin,
}

/// The resolved bracket: the reference position and one exposure per input, in request order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedExposures {
    pub reference: usize,
    pub reference_origin: ReferenceOrigin,
    pub inputs: Vec<ResolvedExposure>,
}

impl ResolvedExposures {
    /// Build from offsets, checking range and distinctness. `origins[reference]` is replaced with
    /// [`ExposureOrigin::Reference`].
    fn new(
        reference: usize,
        reference_origin: ReferenceOrigin,
        offsets: Vec<f64>,
        mut origins: Vec<ExposureOrigin>,
    ) -> Result<Self, Refusal> {
        origins[reference] = ExposureOrigin::Reference;
        for (input, offset) in offsets.iter().enumerate() {
            if !offset.is_finite() || offset.abs() > MAX_OFFSET_EV {
                return Err(Refusal::ExposureOutOfRange { input });
            }
        }
        for first in 0..offsets.len() {
            for second in first + 1..offsets.len() {
                if (offsets[first] - offsets[second]).abs() < MIN_DISTINCT_EV {
                    return Err(Refusal::IndistinctExposures { first, second });
                }
            }
        }
        let inputs = offsets
            .into_iter()
            .zip(origins)
            .enumerate()
            .map(|(input, (offset_ev, origin))| ResolvedExposure {
                offset_ev: if input == reference { 0.0 } else { offset_ev },
                scale: if input == reference {
                    1.0
                } else {
                    (-offset_ev).exp2()
                },
                origin,
            })
            .collect();
        Ok(Self {
            reference,
            reference_origin,
            inputs,
        })
    }

    /// The lowest and highest offsets.
    pub fn range_ev(&self) -> (f64, f64) {
        self.inputs
            .iter()
            .fold((0.0_f64, 0.0_f64), |(lo, hi), input| {
                (lo.min(input.offset_ev), hi.max(input.offset_ev))
            })
    }
}

/// What the metadata says about the bracket's exposures, in stops of exposure on a common scale
/// (only differences mean anything).
#[derive(Clone, Debug, PartialEq)]
pub enum MetadataExposure {
    Usable {
        source: MetadataSource,
        values: Vec<f64>,
    },
    Absent,
    Contradictory,
}

impl MetadataExposure {
    /// Classify the metadata of a validated bracket.
    pub fn of(frames: &[FrameMetadata]) -> Self {
        let settings: Option<Vec<f64>> = frames
            .iter()
            .map(|frame| frame.exposure.usable_shutter().map(f64::log2))
            .collect();
        let bias: Option<Vec<f64>> = frames
            .iter()
            .map(|frame| frame.exposure.usable_bias())
            .collect();
        let varies = |values: &[f64]| values.iter().any(|v| (v - values[0]).abs() > ZERO_BIAS_EV);
        match (settings, bias) {
            (Some(settings), Some(bias)) if varies(&bias) => {
                let disagree = settings
                    .iter()
                    .zip(&bias)
                    .any(|(s, b)| ((s - settings[0]) - (b - bias[0])).abs() > CONTRADICTION_EV);
                if !varies(&settings) || disagree {
                    Self::Contradictory
                } else {
                    Self::Usable {
                        source: MetadataSource::Settings,
                        values: settings,
                    }
                }
            }
            (Some(settings), _) => Self::Usable {
                source: MetadataSource::Settings,
                values: settings,
            },
            (None, Some(bias)) => Self::Usable {
                source: MetadataSource::Bias,
                values: bias,
            },
            (None, None) => Self::Absent,
        }
    }
}

/// The result of [`resolve_exposures`]: complete, or waiting for a measured fit.
#[derive(Clone, Debug, PartialEq)]
pub enum ExposureResolution {
    Resolved(ResolvedExposures),
    NeedsFit(FitPlan),
}

/// The median-ranked position of `values` (the lower middle for an even count), capture order
/// breaking ties.
fn median_ranked(values: &[f64], order: &[usize]) -> usize {
    let mut rank = vec![0; values.len()];
    for (position, &input) in order.iter().enumerate() {
        rank[input] = position;
    }
    let mut ranked: Vec<usize> = (0..values.len()).collect();
    ranked.sort_by(|&a, &b| values[a].total_cmp(&values[b]).then(rank[a].cmp(&rank[b])));
    ranked[(ranked.len() - 1) / 2]
}

/// Validate `frames` and resolve their exposures with `options`, or say what must be measured.
pub fn resolve_exposures(
    frames: &[FrameMetadata],
    options: &ExposureOptions,
) -> Result<ExposureResolution, Refusal> {
    validate_bracket(frames)?;
    let n = frames.len();
    let overrides = if options.overrides.is_empty() {
        vec![None; n]
    } else if options.overrides.len() == n {
        options.overrides.clone()
    } else {
        return Err(Refusal::OverrideCount {
            count: options.overrides.len(),
            inputs: n,
        });
    };
    for (input, value) in overrides.iter().enumerate() {
        if value.is_some_and(|value| !value.is_finite() || value.abs() > MAX_OFFSET_EV) {
            return Err(Refusal::InvalidOverride { input });
        }
    }
    if let Some(reference) = options.reference.filter(|reference| *reference >= n) {
        return Err(Refusal::InvalidReference { reference });
    }
    let order = capture_order(frames);
    let metadata = MetadataExposure::of(frames);
    let zero_bias: Vec<usize> = (0..n)
        .filter(|&input| {
            frames[input]
                .exposure
                .usable_bias()
                .is_some_and(|bias| bias.abs() <= ZERO_BIAS_EV)
        })
        .collect();
    let zero_overrides: Vec<usize> = (0..n)
        .filter(|&input| overrides[input].is_some_and(|value| value.abs() <= ZERO_OVERRIDE_EV))
        .collect();
    let fixed = if let Some(reference) = options.reference {
        Some((reference, ReferenceOrigin::Requested))
    } else if let [reference] = zero_bias[..] {
        Some((reference, ReferenceOrigin::ZeroBias))
    } else if let MetadataExposure::Usable { values, .. } = &metadata {
        Some((
            median_ranked(values, &order),
            ReferenceOrigin::MedianExposure,
        ))
    } else if overrides.iter().all(Option::is_some) {
        match zero_overrides[..] {
            [reference] => Some((reference, ReferenceOrigin::ZeroOverride)),
            _ => return Err(Refusal::OverridesNeedReference),
        }
    } else {
        None
    };
    let Some((reference, reference_origin)) = fixed else {
        if overrides.iter().any(Option::is_some) {
            return Err(Refusal::OverridesNeedReference);
        }
        return Ok(ExposureResolution::NeedsFit(FitPlan {
            reference: order[(n - 1) / 2],
            reference_origin: None,
            overrides,
            order,
        }));
    };
    if overrides[reference].is_some_and(|value| value.abs() > ZERO_OVERRIDE_EV) {
        return Err(Refusal::InvalidOverride { input: reference });
    }
    match metadata {
        MetadataExposure::Usable { source, values } => {
            let mut offsets = Vec::with_capacity(n);
            let mut origins = Vec::with_capacity(n);
            for input in 0..n {
                match overrides[input] {
                    Some(value) => {
                        offsets.push(value);
                        origins.push(ExposureOrigin::Override);
                    }
                    None => {
                        offsets.push(values[input] - values[reference]);
                        origins.push(ExposureOrigin::Metadata { source });
                    }
                }
            }
            ResolvedExposures::new(reference, reference_origin, offsets, origins)
                .map(ExposureResolution::Resolved)
        }
        MetadataExposure::Absent | MetadataExposure::Contradictory => {
            let plan = FitPlan {
                reference,
                reference_origin: Some(reference_origin),
                overrides,
                order,
            };
            if plan.measured().next().is_none() {
                // Every input but the reference is overridden: nothing to measure.
                return plan
                    .finish(vec![None; n], vec![0.0; n])
                    .map(ExposureResolution::Resolved);
            }
            Ok(ExposureResolution::NeedsFit(plan))
        }
    }
}

/// Exposures that must be measured: the reference the fit is relative to, whether it is final
/// (`reference_origin`) or provisional (`None`: the capture-order middle, replaced after the fit
/// by the median measured exposure), and the overrides that stand without measurement. Align
/// every input to [`Self::reference`] before sampling; when the reference is provisional and the
/// fit moves it, alignment is re-anchored on the final reference.
#[derive(Clone, Debug, PartialEq)]
pub struct FitPlan {
    reference: usize,
    reference_origin: Option<ReferenceOrigin>,
    overrides: Vec<Option<f64>>,
    /// Capture order, for ties when a provisional reference is replaced.
    order: Vec<usize>,
}

impl FitPlan {
    /// The reference the fit's samples are aligned to.
    pub fn reference(&self) -> usize {
        self.reference
    }

    /// Whether [`Self::reference`] is provisional.
    pub fn is_provisional(&self) -> bool {
        self.reference_origin.is_none()
    }

    /// The inputs the fit measures.
    pub fn measured(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.overrides.len())
            .filter(|&input| input != self.reference && self.overrides[input].is_none())
    }

    /// Measure the offsets from `samples` and resolve the bracket, or refuse an insufficient or
    /// ambiguous fit.
    pub fn complete(&self, samples: &FitSamples) -> Result<ResolvedExposures, Refusal> {
        let fit = ExposureFit::measure(samples, self.reference)?;
        for input in self
            .measured()
            .chain(self.is_provisional().then_some(self.reference))
        {
            let quality = &fit.quality[input];
            if quality.spread_ev > AMBIGUOUS_SPREAD_EV || quality.residual_ev > MAX_FIT_RESIDUAL_EV
            {
                return Err(Refusal::AmbiguousExposure {
                    input,
                    spread_ev: quality.spread_ev,
                    residual_ev: quality.residual_ev,
                });
            }
        }
        let origins = (0..self.overrides.len())
            .map(|input| {
                (input != self.reference || self.is_provisional())
                    .then(|| fit.quality[input].origin())
            })
            .collect();
        self.finish(origins, fit.offsets)
    }

    /// Resolve with measured `fitted` offsets relative to [`Self::reference`] and their origins
    /// (`None` where nothing was measured).
    fn finish(
        &self,
        measured: Vec<Option<ExposureOrigin>>,
        fitted: Vec<f64>,
    ) -> Result<ResolvedExposures, Refusal> {
        let n = self.overrides.len();
        let mut offsets = Vec::with_capacity(n);
        let mut origins = Vec::with_capacity(n);
        for (input, origin) in measured.into_iter().enumerate() {
            let (offset, origin) = match (self.overrides[input], origin) {
                (Some(value), _) => (value, ExposureOrigin::Override),
                // A provisional reference keeps its own measured quality: after rebasing it is
                // an ordinary measured input unless it stays the median.
                (None, Some(origin)) => (fitted[input], origin),
                (None, None) => (0.0, ExposureOrigin::Reference),
            };
            offsets.push(offset);
            origins.push(origin);
        }
        match self.reference_origin {
            Some(origin) => ResolvedExposures::new(self.reference, origin, offsets, origins),
            None => {
                // The median measured exposure becomes the reference, capture order breaking ties.
                let reference = median_ranked(&offsets, &self.order);
                let base = offsets[reference];
                let offsets = offsets.iter().map(|offset| offset - base).collect();
                ResolvedExposures::new(reference, ReferenceOrigin::MedianFit, offsets, origins)
            }
        }
    }
}

/// Bounded aligned samples for a fit: per point, every input's Rec. 709 luminance and
/// [`SensorLevel`], at most [`MAX_FIT_SAMPLES`] points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitSamples {
    inputs: usize,
    luminance: Vec<f32>,
    levels: Vec<SensorLevel>,
}

/// Rec. 709 luminance of linear sRGB.
fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

impl FitSamples {
    pub fn new(inputs: usize) -> Self {
        Self {
            inputs,
            luminance: Vec::new(),
            levels: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        if self.inputs == 0 {
            0
        } else {
            self.luminance.len() / self.inputs
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether another point fits.
    pub fn is_full(&self) -> bool {
        self.len() >= MAX_FIT_SAMPLES
    }

    /// Add one point: every input's aligned linear RGB and level there. Returns `false`, adding
    /// nothing, once full.
    pub fn push(&mut self, rgb: &[[f32; 3]], levels: &[SensorLevel]) -> Result<bool, Error> {
        if rgb.len() != self.inputs || levels.len() != self.inputs {
            return Err(Error::validation("fit sample names every input once"));
        }
        if self.is_full() {
            return Ok(false);
        }
        self.luminance.extend(rgb.iter().map(|rgb| luminance(*rgb)));
        self.levels.extend_from_slice(levels);
        Ok(true)
    }

    /// Add every `step`-th pixel of every `step`-th row of one rectangle of aligned inputs, in
    /// row order, until full.
    pub fn gather(&mut self, frames: &[FrameRect<'_>], step: usize) -> Result<(), Error> {
        if frames.len() != self.inputs || step == 0 || frames.len() > MAX_INPUTS {
            return Err(Error::validation("fit gather names every input once"));
        }
        for frame in frames {
            frame.check()?;
            if (frame.width, frame.height) != (frames[0].width, frames[0].height) {
                return Err(Error::validation("fit gather rectangles differ in size"));
            }
        }
        let mut rgb = [[0.0; 3]; MAX_INPUTS];
        let mut levels = [SensorLevel(0); MAX_INPUTS];
        for y in (0..frames[0].height).step_by(step) {
            for x in (0..frames[0].width).step_by(step) {
                for (i, frame) in frames.iter().enumerate() {
                    (rgb[i], levels[i]) = frame.at(x, y);
                }
                if !self.push(&rgb[..frames.len()], &levels[..frames.len()])? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Input `input`'s usable luminance at point `point`.
    fn usable(&self, point: usize, input: usize) -> Option<f64> {
        let index = point * self.inputs + input;
        let level = self.levels[index];
        let value = f64::from(self.luminance[index]);
        (!level.is_clipped()
            && (FIT_FLOOR..=FIT_CEILING).contains(&level.fraction())
            && value.is_finite()
            && value > 0.0)
            .then_some(value)
    }
}

/// One input's measured quality.
#[derive(Clone, Debug, PartialEq)]
struct FitQuality {
    residual_ev: f64,
    spread_ev: f64,
    samples: u64,
}

impl FitQuality {
    /// Confidence falls linearly to zero at either refusal threshold.
    fn origin(&self) -> ExposureOrigin {
        let confidence = (1.0
            - (self.spread_ev / AMBIGUOUS_SPREAD_EV).max(self.residual_ev / MAX_FIT_RESIDUAL_EV))
        .clamp(0.0, 1.0);
        ExposureOrigin::Fit {
            residual_ev: self.residual_ev,
            spread_ev: self.spread_ev,
            confidence,
            samples: u32::try_from(self.samples).unwrap_or(u32::MAX),
        }
    }
}

/// A measured fit: offsets relative to the fit's reference and each input's quality.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposureFit {
    pub offsets: Vec<f64>,
    quality: Vec<FitQuality>,
}

/// One measured pair: `offset[a] − offset[b] ≈ median`.
struct Pair {
    a: usize,
    b: usize,
    median: f64,
    spread: f64,
    count: usize,
}

fn median_of_sorted(values: &[f64]) -> f64 {
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

impl ExposureFit {
    /// Measure every input's offset from `reference`: for each pair of inputs the median of
    /// `log2(Y_a / Y_b)` over points usable in both, its robust spread `1.4826 · MAD`, then the
    /// least-squares offsets over all pairs weighted by their counts, with `offset[reference] =
    /// 0`. An input that no chain of measured pairs connects to the reference is refused.
    pub fn measure(samples: &FitSamples, reference: usize) -> Result<Self, Refusal> {
        let n = samples.inputs;
        let points = samples.len();
        let mut pairs = Vec::new();
        let mut ratios = Vec::with_capacity(points);
        for a in 0..n {
            for b in a + 1..n {
                ratios.clear();
                for point in 0..points {
                    if let (Some(ya), Some(yb)) =
                        (samples.usable(point, a), samples.usable(point, b))
                    {
                        ratios.push((ya / yb).log2());
                    }
                }
                if ratios.len() < MIN_PAIR_SAMPLES {
                    continue;
                }
                ratios.sort_by(f64::total_cmp);
                let median = median_of_sorted(&ratios);
                let mut deviations: Vec<f64> = ratios.iter().map(|r| (r - median).abs()).collect();
                deviations.sort_by(f64::total_cmp);
                pairs.push(Pair {
                    a,
                    b,
                    median,
                    spread: 1.4826 * median_of_sorted(&deviations),
                    count: ratios.len(),
                });
            }
        }
        // Connectivity to the reference, in input order.
        let mut reached = vec![false; n];
        reached[reference] = true;
        loop {
            let mut grew = false;
            for pair in &pairs {
                if reached[pair.a] != reached[pair.b] {
                    reached[pair.a] = true;
                    reached[pair.b] = true;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        // An isolated reference is the input lacking overlap; otherwise the first unreached.
        let isolated = !pairs.iter().any(|p| p.a == reference || p.b == reference);
        if let Some(input) = reached.iter().position(|reached| !reached) {
            let input = if isolated { reference } else { input };
            return Err(Refusal::InsufficientOverlap { input });
        }
        // Normal equations over the unknowns other than the reference.
        let unknowns: Vec<usize> = (0..n).filter(|&input| input != reference).collect();
        let slot = |input: usize| unknowns.iter().position(|&u| u == input);
        let m = unknowns.len();
        let mut matrix = vec![vec![0.0_f64; m + 1]; m];
        for pair in &pairs {
            let w = pair.count as f64;
            let (sa, sb) = (slot(pair.a), slot(pair.b));
            if let Some(i) = sa {
                matrix[i][i] += w;
                matrix[i][m] += w * pair.median;
            }
            if let Some(j) = sb {
                matrix[j][j] += w;
                matrix[j][m] -= w * pair.median;
            }
            if let (Some(i), Some(j)) = (sa, sb) {
                matrix[i][j] -= w;
                matrix[j][i] -= w;
            }
        }
        let solution = solve(matrix).ok_or(Refusal::InsufficientOverlap {
            input: unknowns.first().copied().unwrap_or(reference),
        })?;
        let mut offsets = vec![0.0; n];
        for (i, &input) in unknowns.iter().enumerate() {
            offsets[input] = solution[i];
        }
        let quality = (0..n)
            .map(|input| {
                let (mut squares, mut weight, mut spread, mut samples) = (0.0, 0.0, 0.0_f64, 0);
                for pair in pairs.iter().filter(|p| p.a == input || p.b == input) {
                    let w = pair.count as f64;
                    let residual = offsets[pair.a] - offsets[pair.b] - pair.median;
                    squares += w * residual * residual;
                    weight += w;
                    spread = spread.max(pair.spread);
                    samples += pair.count as u64;
                }
                FitQuality {
                    residual_ev: if weight > 0.0 {
                        (squares / weight).sqrt()
                    } else {
                        0.0
                    },
                    spread_ev: spread,
                    samples,
                }
            })
            .collect();
        Ok(Self { offsets, quality })
    }
}

/// Solve the augmented system by Gaussian elimination with partial pivoting, in a fixed order.
/// `None` when singular.
fn solve(mut matrix: Vec<Vec<f64>>) -> Option<Vec<f64>> {
    let m = matrix.len();
    for column in 0..m {
        let pivot = (column..m).max_by(|&a, &b| {
            matrix[a][column]
                .abs()
                .total_cmp(&matrix[b][column].abs())
                .then(b.cmp(&a))
        })?;
        if matrix[pivot][column].abs() < 1e-9 {
            return None;
        }
        matrix.swap(column, pivot);
        let (above, below) = matrix.split_at_mut(column + 1);
        let pivot_row = &above[column];
        for row in below {
            let factor = row[column] / pivot_row[column];
            for (value, pivot) in row.iter_mut().zip(pivot_row).skip(column) {
                *value -= factor * pivot;
            }
        }
    }
    let mut x = vec![0.0; m];
    for row in (0..m).rev() {
        let mut sum = matrix[row][m];
        for k in row + 1..m {
            sum -= matrix[row][k] * x[k];
        }
        x[row] = sum / matrix[row][row];
    }
    Some(x)
}
