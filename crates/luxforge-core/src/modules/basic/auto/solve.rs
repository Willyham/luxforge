//! Deterministic Auto tone statistics and solver. No image IO, catalog or renderer ownership.
//! Callers supply the actual compiled units ([`super::ForwardModel`]) and account the bounded
//! scratch.
use crate::{
    AnalysisRefusal, Cancel, ColorOperation, Error,
    colour::{oklab::to_oklab, srgb::encode},
    tiles::SampleGrid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const ALGORITHM: &str = "auto-tone/1";
/// The eight Basic fields Auto sets, which Basic's descriptor declares as what it writes.
pub const FIELDS: [&str; 8] = [
    "exposure",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "vibrance",
    "saturation",
];
const BOUNDS: [(f64, f64); 8] = [
    (-4., 4.),
    (-50., 50.),
    (-100., 0.),
    (0., 60.),
    (-60., 60.),
    (-60., 60.),
    (0., 25.),
    (-15., 0.),
];
const CHUNK: usize = 4096;

/// Values are ordinary Basic settings, including explicit zeros; white balance is absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutoToneValues {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub vibrance: f64,
    pub saturation: f64,
}

impl AutoToneValues {
    pub fn fields(self) -> Map<String, Value> {
        FIELDS
            .into_iter()
            .zip(self.array())
            .map(|(name, value)| (name.into(), json!(value)))
            .collect()
    }

    fn array(self) -> [f64; 8] {
        [
            self.exposure,
            self.contrast,
            self.highlights,
            self.shadows,
            self.whites,
            self.blacks,
            self.vibrance,
            self.saturation,
        ]
    }
}

/// All tunable picture targets live together. Bounds and field steps remain the Auto contract.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoToneTargets {
    pub median: f64,
    pub bright_guard: f64,
    pub highlights_scale: f64,
    pub shadows_scale: f64,
    pub spread: f64,
    pub clipping: f64,
    pub vibrance_chroma: f64,
    pub saturation_chroma: f64,
}

impl Default for AutoToneTargets {
    fn default() -> Self {
        Self {
            median: 0.46,
            bright_guard: 0.02,
            highlights_scale: 250.,
            shadows_scale: 200.,
            spread: 0.60,
            clipping: 0.0005,
            vibrance_chroma: 0.07,
            saturation_chroma: 0.14,
        }
    }
}

impl AutoToneTargets {
    pub fn validate(self) -> Result<(), Error> {
        let values = [
            self.median,
            self.bright_guard,
            self.highlights_scale,
            self.shadows_scale,
            self.spread,
            self.clipping,
            self.vibrance_chroma,
            self.saturation_chroma,
        ];
        if values.iter().any(|n| !n.is_finite() || *n <= 0.)
            || self.median >= 1.
            || self.bright_guard >= 1.
            || self.spread >= 1.
            || self.clipping >= 1.
        {
            return Err(Error::validation("invalid Auto tone targets"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Statistics {
    pub p01: f64,
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub bright: f64,
    pub dark: f64,
    pub near_white: f64,
    pub highlight_clip: f64,
    pub shadow_clip: f64,
    pub mean_chroma: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampleReport {
    pub grid: [u32; 2],
    pub count: usize,
    pub usable: usize,
    /// Points whose source was already clipped: in the statistics, out of the clipping fractions.
    pub source_clipped: usize,
    pub non_finite: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoToneReport {
    pub algorithm: String,
    pub exposure_search: ExposureSearch,
    pub values: AutoToneValues,
    pub sample: SampleReport,
    pub input: Statistics,
    pub bands: Statistics,
    pub contrast: Statistics,
    pub tone: Statistics,
    pub output: Statistics,
    pub bounded: Vec<String>,
    pub unmet: Vec<String>,
}

fn refusal(reason: &str, message: &str) -> Error {
    AnalysisRefusal::new(reason, format!("Auto tone: {message}")).into()
}

fn luminance(rgb: [f32; 3]) -> f64 {
    0.2126 * f64::from(rgb[0]) + 0.7152 * f64::from(rgb[1]) + 0.0722 * f64::from(rgb[2])
}

fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    // Inverse empirical CDF, i.e. nearest rank, with equal weight for every grid point.
    let rank = ((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    *values
        .select_nth_unstable_by(rank.min(values.len() - 1), f64::total_cmp)
        .1
}

/// Statistics in scan order and f64, independent of the renderer's or worker pool's thread count.
/// `linear_input` selects the refusal statistics; output statistics use clamped encoded channels.
fn statistics(
    sample: &SampleGrid,
    operations: &[ColorOperation],
    linear_input: bool,
    chroma: bool,
    cancel: &Cancel,
) -> Result<Statistics, Error> {
    let mut ys = Vec::with_capacity(sample.rgb.len());
    let mut row = Vec::with_capacity(CHUNK);
    let (
        mut bright,
        mut dark,
        mut near_white,
        mut highlights,
        mut shadows,
        mut included,
        mut chroma_count,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut chroma_sum = 0.;
    for (chunk_index, chunk) in sample.rgb.chunks(CHUNK).enumerate() {
        cancel.check()?;
        row.clear();
        row.extend_from_slice(chunk);
        for operation in operations {
            for unit in operation.units() {
                unit.apply_row(0, 0, &mut row);
            }
        }
        for (offset, (&input, &rgb)) in chunk.iter().zip(&row).enumerate() {
            if !input.iter().all(|c| c.is_finite()) {
                continue;
            }
            if !rgb.iter().all(|c| c.is_finite()) {
                return Err(Error::render(
                    "Auto tone forward model produced a non-finite value",
                ));
            }
            let clamped = rgb.map(|c| c.clamp(0., 1.));
            let y = if linear_input {
                luminance(rgb)
            } else {
                let [r, g, b] = clamped.map(|c| encode(f64::from(c)));
                0.2126 * r + 0.7152 * g + 0.0722 * b
            };
            ys.push(y);
            bright += usize::from(y > 0.8);
            dark += usize::from(y < 0.2);
            near_white += usize::from(y >= 0.98);
            if sample.source_clipped[chunk_index * CHUNK + offset] {
                continue;
            }
            included += 1;
            highlights += usize::from(clamped.contains(&1.));
            shadows += usize::from(clamped.contains(&0.));
            if chroma && luminance(clamped) > 1e-6 {
                let lab = to_oklab(clamped);
                chroma_sum += f64::from(lab.a).hypot(f64::from(lab.b));
                chroma_count += 1;
            }
        }
    }
    if ys.is_empty() {
        return Err(refusal(
            "too-few-samples",
            "fewer than 1,024 finite samples",
        ));
    }
    let count = ys.len() as f64;
    let fraction = |n: usize| n as f64 / included.max(1) as f64;
    Ok(Statistics {
        p01: percentile(&mut ys, 0.01),
        p10: percentile(&mut ys, 0.10),
        p50: percentile(&mut ys, 0.50),
        p90: percentile(&mut ys, 0.90),
        p99: percentile(&mut ys, 0.99),
        bright: bright as f64 / count,
        dark: dark as f64 / count,
        near_white: near_white as f64 / count,
        highlight_clip: fraction(highlights),
        shadow_clip: fraction(shadows),
        mean_chroma: chroma_sum / chroma_count.max(1) as f64,
    })
}

/// Statistics for the fitting rig's rendered samples, through the same output boundary.
pub fn picture_statistics(sample: &SampleGrid, cancel: &Cancel) -> Result<Statistics, Error> {
    sample.validate()?;
    statistics(sample, &[], false, true, cancel)
}

/// The last grid value that meets a monotone upper-bound predicate. `low` is returned when the
/// entire bounded range is infeasible; the report then names the unmet constraint.
fn last_true(
    mut low: i32,
    mut high: i32,
    mut test: impl FnMut(i32) -> Result<bool, Error>,
) -> Result<i32, Error> {
    while low < high {
        let middle = low + (high - low + 1) / 2;
        if test(middle)? {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(low)
}

/// A layer after Basic whose luminance response is not monotonic, such as an extrapolated Look,
/// can reverse luminance as Exposure rises. Such a model needs the whole committed grid; a model
/// whose every later unit declares a monotonic response permits the monotone search
/// ([`crate::ToolModule::monotonic_luminance`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExposureSearch {
    Monotone,
    Exhaustive,
}

/// Solve through the modules' compiled pointwise units with the monotone Exposure search. The
/// compile closure preserves Basic's white balance but replaces all eight Auto fields, as
/// [`super::ForwardModel::compile`] does.
pub fn solve(
    sample: &SampleGrid,
    targets: AutoToneTargets,
    compile: impl Fn(AutoToneValues) -> Result<Vec<ColorOperation>, Error>,
    cancel: &Cancel,
) -> Result<AutoToneReport, Error> {
    solve_with_exposure_search(sample, targets, compile, ExposureSearch::Monotone, cancel)
}

/// Solve with an explicit Exposure search contract, which [`super::ForwardModel`] chooses.
pub fn solve_with_exposure_search(
    sample: &SampleGrid,
    targets: AutoToneTargets,
    compile: impl Fn(AutoToneValues) -> Result<Vec<ColorOperation>, Error>,
    search: ExposureSearch,
    cancel: &Cancel,
) -> Result<AutoToneReport, Error> {
    sample.validate()?;
    targets.validate()?;
    cancel.check()?;
    let usable = sample
        .rgb
        .iter()
        .filter(|rgb| rgb.iter().all(|c| c.is_finite()))
        .count();
    if usable < 1024 {
        return Err(refusal(
            "too-few-samples",
            "fewer than 1,024 finite samples",
        ));
    }
    let input = statistics(sample, &[], true, false, cancel)?;
    if input.p50 <= 1e-6 {
        return Err(refusal(
            "near-black",
            "the input median is at or below 1e-6",
        ));
    }
    if input.p99 <= 0. || (input.p99 / input.p01.max(f64::MIN_POSITIVE)).log2() < 0.5 {
        return Err(refusal(
            "no-tonal-range",
            "the input range is less than half a stop",
        ));
    }
    let evaluate = |values, chroma| statistics(sample, &compile(values)?, false, chroma, cancel);
    let exposure = |mut values: AutoToneValues| -> Result<f64, Error> {
        if search == ExposureSearch::Exhaustive {
            let mut admissible = [false; 801];
            let (mut best, mut error) = (0usize, f64::INFINITY);
            for (index, admissible) in admissible.iter_mut().enumerate() {
                values.exposure = (index as f64 - 400.) / 100.;
                let stats = evaluate(values, false)?;
                let difference = (stats.p50 - targets.median).abs();
                if difference < error {
                    best = index;
                    error = difference;
                }
                *admissible = stats.near_white <= targets.bright_guard;
            }
            let guarded = (0..=best)
                .rev()
                .find(|&index| admissible[index])
                .unwrap_or(0);
            return Ok((guarded as f64 - 400.) / 100.);
        }
        let lower = last_true(-400, 400, |step| {
            values.exposure = f64::from(step) / 100.;
            Ok(evaluate(values, false)?.p50 <= targets.median)
        })?;
        values.exposure = f64::from(lower) / 100.;
        let below = evaluate(values, false)?.p50;
        let upper = (lower + 1).min(400);
        values.exposure = f64::from(upper) / 100.;
        let above = evaluate(values, false)?.p50;
        let best = if (above - targets.median).abs() < (below - targets.median).abs() {
            upper
        } else {
            lower
        };
        Ok(f64::from(last_true(-400, best, |step| {
            values.exposure = f64::from(step) / 100.;
            Ok(evaluate(values, false)?.near_white <= targets.bright_guard)
        })?) / 100.)
    };
    let endpoints = |mut values: AutoToneValues| -> Result<AutoToneValues, Error> {
        values.whites = f64::from(last_true(-60, 60, |step| {
            let mut candidate = values;
            candidate.whites = f64::from(step);
            Ok(evaluate(candidate, false)?.highlight_clip <= targets.clipping)
        })?);
        // Negating the grid turns the smallest acceptable Blacks into a last-true search.
        values.blacks = -f64::from(last_true(-60, 60, |step| {
            let mut candidate = values;
            candidate.blacks = -f64::from(step);
            Ok(evaluate(candidate, false)?.shadow_clip <= targets.clipping)
        })?);
        Ok(values)
    };
    let mut values = AutoToneValues::default();
    values.exposure = exposure(values)?;
    let bands = evaluate(values, false)?;
    values.highlights = (-targets.highlights_scale * bands.bright)
        .clamp(-100., 0.)
        .round();
    values.shadows = (targets.shadows_scale * bands.dark).clamp(0., 60.).round();
    let contrast = evaluate(values, false)?;
    values.contrast = (100. * (targets.spread - (contrast.p90 - contrast.p10)))
        .clamp(-50., 50.)
        .round();
    values = endpoints(values)?;
    values.exposure = exposure(values)?;
    values = endpoints(values)?;
    let tone = evaluate(values, true)?;
    values.vibrance = (60. * (1. - tone.mean_chroma / targets.vibrance_chroma))
        .clamp(0., 25.)
        .round();
    values.saturation = (-60. * (tone.mean_chroma / targets.saturation_chroma - 1.))
        .clamp(-15., 0.)
        .round();
    let output = evaluate(values, true)?;
    let bounded = FIELDS
        .into_iter()
        .zip(values.array())
        .zip(BOUNDS)
        .filter(|((_, value), (low, high))| value == low || value == high)
        .map(|((name, _), _)| name.to_owned())
        .collect();
    let unmet = [
        ("bright-guard", tone.near_white > targets.bright_guard),
        ("highlight-clipping", tone.highlight_clip > targets.clipping),
        ("shadow-clipping", tone.shadow_clip > targets.clipping),
    ]
    .into_iter()
    .filter(|(_, failed)| *failed)
    .map(|(name, _)| name.to_owned())
    .collect();
    Ok(AutoToneReport {
        algorithm: ALGORITHM.into(),
        exposure_search: search,
        values,
        input,
        bands,
        contrast,
        tone,
        output,
        bounded,
        unmet,
        sample: SampleReport {
            grid: sample.grid,
            count: sample.rgb.len(),
            usable,
            source_clipped: sample.source_clipped.iter().filter(|&&flag| flag).count(),
            non_finite: sample.rgb.len() - usable,
        },
    })
}

#[cfg(test)]
mod tests;
