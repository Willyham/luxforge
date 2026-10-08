//! Deterministic Auto tone statistics and solver. No image IO, catalog or renderer ownership.
//! Callers supply the actual compiled units ([`super::ForwardModel`]) and account the bounded
//! scratch.
use crate::{
    AnalysisRefusal, Cancel, ColorOperation, Error,
    colour::{oklab::to_oklab, srgb::encode},
    tiles::SampleGrid,
};
use rayon::prelude::*;
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

fn finite(rgb: [f32; 3]) -> bool {
    rgb.iter().all(|c| c.is_finite())
}

/// The nearest-rank percentiles at ascending `fractions` of `values`: the inverse empirical CDF,
/// with equal weight for every grid point. Each is the value a selection of the whole slice at its
/// rank finds, but every selection after the first runs only within the part the one before it
/// partitioned off, about twice the slice's length in all rather than once per percentile.
fn percentiles<const N: usize>(values: &mut [f64], fractions: [f64; N]) -> [f64; N] {
    let ranks = fractions.map(|fraction| {
        ((values.len() as f64 * fraction).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)
    });
    let mut found = [0.; N];
    select_ranks(values, 0, &ranks, &mut found);
    found
}

/// Select the ascending `ranks` of the slice `values` begins at rank `offset` of, into `found`.
fn select_ranks(values: &mut [f64], offset: usize, ranks: &[usize], found: &mut [f64]) {
    let Some(&rank) = ranks.get(ranks.len() / 2) else {
        return;
    };
    let (below, value, above) = values.select_nth_unstable_by(rank - offset, f64::total_cmp);
    let (lower, upper) = (
        ranks.partition_point(|&r| r < rank),
        ranks.partition_point(|&r| r <= rank),
    );
    found[lower..upper].fill(*value);
    select_ranks(below, offset, &ranks[..lower], &mut found[..lower]);
    select_ranks(above, rank + 1, &ranks[upper..], &mut found[upper..]);
}

/// About how many points the reduced sample a search locates its answer on holds: one of every
/// [`stride`] grid points in scan order ([`View`]).
const REDUCED_POINTS: usize = 32_768;

/// The reduced sample's stride for a grid of `points`: 1, the whole sample, up to twice
/// [`REDUCED_POINTS`].
fn stride(points: usize) -> usize {
    (points / REDUCED_POINTS).max(1)
}

/// A fixed integer mixer (SplitMix64's finalizer): where in its run of points the reduction reads.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// The bytes a solve of a grid of `points` holds besides the grid, which its caller charges: one
/// `f64` luminance per usable point for the whole sample and for its reduction, and one RGB chunk
/// for each thread of the shared pool evaluating a chunk at once.
pub fn scratch_bytes(points: usize) -> usize {
    (points + points.div_ceil(stride(points))) * size_of::<f64>()
        + rayon::current_num_threads() * CHUNK * size_of::<[f32; 3]>()
}

/// The points of a sample an evaluation reads: the whole sample at `stride` 1; otherwise one point
/// of each run of `stride` consecutive points in scan order, at a position within the run the run's
/// number alone decides ([`mix`]): a jittered stratified sample, so a regular pattern in the
/// photograph cannot alias with the reduction as a plain stride would. Each chunk of [`CHUNK`]
/// selected points is evaluated as one task of the shared pool, and writes its finite points'
/// luminance from `offsets[chunk]`, so where each value lands, and the order in which chunks' sums
/// are added, are fixed by the sample alone, whatever the pool's size.
struct View<'a> {
    sample: &'a SampleGrid,
    stride: usize,
    /// For each chunk, the number of finite points among the selected points before it; then
    /// their total.
    offsets: Vec<usize>,
}

impl<'a> View<'a> {
    fn new(sample: &'a SampleGrid, stride: usize) -> Self {
        let selected = sample.rgb.len().div_ceil(stride);
        let mut offsets = Vec::with_capacity(selected.div_ceil(CHUNK) + 1);
        let mut usable = 0;
        offsets.push(0);
        for start in (0..selected).step_by(CHUNK) {
            usable += (start..(start + CHUNK).min(selected))
                .filter(|&point| finite(sample.rgb[point_of(sample, stride, point)]))
                .count();
            offsets.push(usable);
        }
        Self {
            sample,
            stride,
            offsets,
        }
    }

    /// The grid index of the `selected`th point read.
    fn point(&self, selected: usize) -> usize {
        point_of(self.sample, self.stride, selected)
    }

    fn usable(&self) -> usize {
        self.offsets[self.offsets.len() - 1]
    }
}

/// The grid index of the `selected`th point of `sample` a view at `stride` reads.
fn point_of(sample: &SampleGrid, stride: usize, selected: usize) -> usize {
    if stride == 1 {
        return selected;
    }
    let start = selected * stride;
    let run = stride.min(sample.rgb.len() - start);
    start + (mix(selected as u64) % run as u64) as usize
}

/// What an evaluation computes: the clipping fractions alone; every statistic but chroma; or all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Need {
    Clipping,
    Tone,
    Chroma,
}

/// One chunk's counts, added in chunk order.
#[derive(Default)]
struct Counts {
    bright: usize,
    dark: usize,
    near_white: usize,
    highlights: usize,
    shadows: usize,
    included: usize,
    chroma_count: usize,
    chroma_sum: f64,
}

/// Evaluate `view` through `operations`: counts in chunk order and, from [`Need::Tone`] on, every
/// finite point's luminance in scan order. `linear_input` selects the refusal statistics; output
/// statistics use clamped encoded channels. Independent of the pool's thread count.
fn counts(
    view: &View<'_>,
    operations: &[ColorOperation],
    linear_input: bool,
    need: Need,
    cancel: &Cancel,
) -> Result<(Counts, Vec<f64>), Error> {
    let tone = need >= Need::Tone;
    let mut ys = vec![0.; if tone { view.usable() } else { 0 }];
    let mut pieces = Vec::with_capacity(view.offsets.len() - 1);
    let mut rest = ys.as_mut_slice();
    for window in view.offsets.windows(2) {
        let (piece, after) = rest.split_at_mut(if tone { window[1] - window[0] } else { 0 });
        pieces.push(piece);
        rest = after;
    }
    let sample = view.sample;
    let selected = sample.rgb.len().div_ceil(view.stride);
    let chunks = pieces
        .into_par_iter()
        .enumerate()
        .map(|(chunk, ys)| -> Result<Counts, Error> {
            cancel.check()?;
            let points = chunk * CHUNK..((chunk + 1) * CHUNK).min(selected);
            let mut row: Vec<[f32; 3]> = points
                .clone()
                .map(|point| sample.rgb[view.point(point)])
                .collect();
            for operation in operations {
                for unit in operation.units() {
                    unit.apply_row(0, 0, &mut row);
                }
            }
            let mut counts = Counts::default();
            let mut written = 0;
            for (point, rgb) in points.zip(row) {
                let index = view.point(point);
                if !finite(sample.rgb[index]) {
                    continue;
                }
                if !finite(rgb) {
                    return Err(Error::render(
                        "Auto tone forward model produced a non-finite value",
                    ));
                }
                let clamped = rgb.map(|c| c.clamp(0., 1.));
                if tone {
                    let y = if linear_input {
                        luminance(rgb)
                    } else {
                        let [r, g, b] = clamped.map(|c| encode(f64::from(c)));
                        0.2126 * r + 0.7152 * g + 0.0722 * b
                    };
                    ys[written] = y;
                    written += 1;
                    counts.bright += usize::from(y > 0.8);
                    counts.dark += usize::from(y < 0.2);
                    counts.near_white += usize::from(y >= 0.98);
                }
                if sample.source_clipped[index] {
                    continue;
                }
                counts.included += 1;
                counts.highlights += usize::from(clamped.contains(&1.));
                counts.shadows += usize::from(clamped.contains(&0.));
                if need == Need::Chroma && luminance(clamped) > 1e-6 {
                    let lab = to_oklab(clamped);
                    counts.chroma_sum += f64::from(lab.a).hypot(f64::from(lab.b));
                    counts.chroma_count += 1;
                }
            }
            Ok(counts)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut total = Counts::default();
    for counts in chunks {
        total.bright += counts.bright;
        total.dark += counts.dark;
        total.near_white += counts.near_white;
        total.highlights += counts.highlights;
        total.shadows += counts.shadows;
        total.included += counts.included;
        total.chroma_count += counts.chroma_count;
        total.chroma_sum += counts.chroma_sum;
    }
    Ok((total, ys))
}

/// The clipping fractions of `view` through `operations`: highlight, then shadow.
fn clipping(
    view: &View<'_>,
    operations: &[ColorOperation],
    cancel: &Cancel,
) -> Result<(f64, f64), Error> {
    let (counts, _) = counts(view, operations, false, Need::Clipping, cancel)?;
    let included = counts.included.max(1) as f64;
    Ok((
        counts.highlights as f64 / included,
        counts.shadows as f64 / included,
    ))
}

/// Every statistic of `view` through `operations`, chroma when `chroma` is set.
fn view_statistics(
    view: &View<'_>,
    operations: &[ColorOperation],
    linear_input: bool,
    chroma: bool,
    cancel: &Cancel,
) -> Result<Statistics, Error> {
    let need = if chroma { Need::Chroma } else { Need::Tone };
    let (counts, mut ys) = counts(view, operations, linear_input, need, cancel)?;
    if ys.is_empty() {
        return Err(refusal(
            "too-few-samples",
            "fewer than 1,024 finite samples",
        ));
    }
    let count = ys.len() as f64;
    let fraction = |n: usize| n as f64 / counts.included.max(1) as f64;
    let quantiles = percentiles(&mut ys, [0.01, 0.10, 0.50, 0.90, 0.99]);
    Ok(Statistics {
        p01: quantiles[0],
        p10: quantiles[1],
        p50: quantiles[2],
        p90: quantiles[3],
        p99: quantiles[4],
        bright: counts.bright as f64 / count,
        dark: counts.dark as f64 / count,
        near_white: counts.near_white as f64 / count,
        highlight_clip: fraction(counts.highlights),
        shadow_clip: fraction(counts.shadows),
        mean_chroma: counts.chroma_sum / counts.chroma_count.max(1) as f64,
    })
}

/// Statistics of the whole sample in f64, independent of the renderer's or worker pool's thread
/// count. `linear_input` selects the refusal statistics; output statistics use clamped encoded
/// channels.
fn statistics(
    sample: &SampleGrid,
    operations: &[ColorOperation],
    linear_input: bool,
    chroma: bool,
    cancel: &Cancel,
) -> Result<Statistics, Error> {
    view_statistics(
        &View::new(sample, 1),
        operations,
        linear_input,
        chroma,
        cancel,
    )
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

/// [`last_true`] started from `guess`: it gallops from the guess to a bracket — a value that meets
/// the predicate, or `low`, and the next that fails it — and searches that bracket. For a monotone
/// predicate its answer is exactly [`last_true`]'s; a guess near the answer takes two or three
/// tests rather than about `log2(high - low)`. Any answer meets the predicate, or is `low`, and
/// fails it one step further, or is `high`.
fn guided_last_true(
    low: i32,
    high: i32,
    guess: i32,
    mut test: impl FnMut(i32) -> Result<bool, Error>,
) -> Result<i32, Error> {
    let guess = guess.clamp(low, high);
    let mut step = 1;
    if guess == low || test(guess)? {
        let mut met = guess;
        while met < high {
            let probe = (met + step).min(high);
            if !test(probe)? {
                return last_true(met, probe - 1, test);
            }
            met = probe;
            step *= 2;
        }
        return Ok(high);
    }
    let mut failed = guess;
    loop {
        let probe = (failed - step).max(low);
        if probe == low || test(probe)? {
            return last_true(probe, failed - 1, test);
        }
        failed = probe;
        step *= 2;
    }
}

/// A layer after Basic whose luminance response is not monotonic, such as an extrapolated Look,
/// can reverse luminance as Exposure rises, so the median is not monotonic in Exposure either.
/// Such a model needs the median searched over the whole range, coarse to fine; a model whose
/// every later unit declares a monotonic response permits the monotone search
/// ([`crate::ToolModule::monotonic_luminance`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExposureSearch {
    Monotone,
    CoarseToFine,
}

/// Every evaluation one solve makes, through the forward model `compile` builds: of the whole
/// sample or of its reduction, kept for the solve so a value is never evaluated twice.
struct Evaluator<'a, F> {
    full: View<'a>,
    reduced: View<'a>,
    compile: F,
    cancel: &'a Cancel,
    clippings: Vec<(bool, AutoToneValues, (f64, f64))>,
    tones: Vec<(bool, AutoToneValues, bool, Statistics)>,
}

impl<F: Fn(AutoToneValues) -> Result<Vec<ColorOperation>, Error>> Evaluator<'_, F> {
    fn view(&self, reduced: bool) -> &View<'_> {
        if reduced { &self.reduced } else { &self.full }
    }

    /// The clipping fractions at `values`, of the reduction when `reduced` is set.
    fn clipping(&mut self, reduced: bool, values: AutoToneValues) -> Result<(f64, f64), Error> {
        if let Some((.., held)) = self
            .clippings
            .iter()
            .find(|(r, v, _)| *r == reduced && *v == values)
        {
            return Ok(*held);
        }
        let fractions = clipping(self.view(reduced), &(self.compile)(values)?, self.cancel)?;
        self.clippings.push((reduced, values, fractions));
        Ok(fractions)
    }

    /// Every statistic at `values`, with chroma when `chroma` is set, of the reduction when
    /// `reduced` is set.
    fn statistics(
        &mut self,
        reduced: bool,
        values: AutoToneValues,
        chroma: bool,
    ) -> Result<Statistics, Error> {
        if let Some((.., held)) = self
            .tones
            .iter()
            .find(|(r, v, c, _)| *r == reduced && *v == values && *c == chroma)
        {
            return Ok(*held);
        }
        let statistics = view_statistics(
            self.view(reduced),
            &(self.compile)(values)?,
            false,
            chroma,
            self.cancel,
        )?;
        self.tones.push((reduced, values, chroma, statistics));
        Ok(statistics)
    }

    /// The last of `low..=high` whose `measure` is at most `limit`, for a measure that rises
    /// with the step: located on the reduced sample from `hint`, moved by at most two secant
    /// steps of the whole sample's measure, and decided on the whole sample from there
    /// ([`guided_last_true`]), so the answer is exactly [`last_true`]'s whatever the reduction
    /// located. A guess within a step or two of the answer takes two evaluations of the whole
    /// sample.
    fn search(
        &mut self,
        (low, high): (i32, i32),
        hint: i32,
        limit: f64,
        mut measure: impl FnMut(&mut Self, bool, i32) -> Result<f64, Error>,
    ) -> Result<i32, Error> {
        let mut guess = guided_last_true(low, high, hint, |step| {
            Ok(measure(self, true, step)? <= limit)
        })?;
        for _ in 0..2 {
            if guess == high {
                break;
            }
            let (here, next) = (
                measure(self, false, guess)?,
                measure(self, false, guess + 1)?,
            );
            if (here <= limit || guess == low) && next > limit {
                break;
            }
            let rise = next - here;
            if rise.is_nan() || rise <= 0. {
                break;
            }
            let moved = (guess as f64 + ((limit - here) / rise).floor())
                .clamp(f64::from(low), f64::from(high)) as i32;
            if moved == guess {
                break;
            }
            guess = moved;
        }
        guided_last_true(low, high, guess, |step| {
            Ok(measure(self, false, step)? <= limit)
        })
    }

    /// The Exposure step whose median is nearest `target` through a model not monotonic in
    /// Exposure, from `values`. The error every 0.10 EV on the reduction chooses its two least
    /// local minima — a reversal crosses the target at most twice. Each is found on the whole
    /// sample: at the crossing of the target its bracket reaches, or, where the median turns
    /// without crossing, at the local minimum the reduction's least step descends to. The least
    /// of those, the lower on a tie.
    fn coarse_to_fine(&mut self, values: AutoToneValues, target: f64) -> Result<i32, Error> {
        let model = self;
        let median = |model: &mut Self, reduced: bool, step: i32| {
            Ok::<f64, Error>(model.statistics(reduced, exposed(values, step), false)?.p50)
        };
        let error = |model: &mut Self, reduced: bool, step: i32| {
            Ok::<f64, Error>((median(model, reduced, step)? - target).abs())
        };
        let coarse = (-400..=400)
            .step_by(COARSE as usize)
            .map(|step| Ok((step, error(model, true, step)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let mut minima: Vec<usize> = (0..coarse.len())
            .filter(|&i| {
                (i == 0 || coarse[i].1 <= coarse[i - 1].1)
                    && (i + 1 == coarse.len() || coarse[i].1 <= coarse[i + 1].1)
            })
            .collect();
        minima.sort_by(|&a, &b| coarse[a].1.total_cmp(&coarse[b].1).then(a.cmp(&b)));
        minima.truncate(2);
        let mut best: Option<(i32, f64)> = None;
        for index in minima {
            let centre = coarse[index].0;
            let (low, high) = ((centre - COARSE).max(-400), (centre + COARSE).min(400));
            let above = |model: &mut Self, step: i32| {
                Ok::<bool, Error>(median(model, false, step)? > target)
            };
            // On the whole sample, from the basin's coarse bracket, walk a coarse step at
            // a time towards the end nearer the target while it comes nearer, to a
            // bracket the whole sample's median crosses the target in: the reduction may
            // place the basin off the whole sample's crossing.
            let (mut start, mut end) = (low, high);
            let mut toward: Option<bool> = None;
            let crossed = loop {
                if above(model, start)? != above(model, end)? {
                    break true;
                }
                let (from, to) = (error(model, false, start)?, error(model, false, end)?);
                let up = to < from;
                if from == to || toward.is_some_and(|toward| toward != up) {
                    break false;
                }
                toward = Some(up);
                (start, end) = if up && end < 400 {
                    (end, (end + COARSE).min(400))
                } else if !up && start > -400 {
                    ((start - COARSE).max(-400), start)
                } else {
                    break false;
                };
            };
            if crossed {
                let side = above(model, start)?;
                // The whole sample's median crosses the target within the bracket:
                // bisect to the crossing and take its nearer side.
                let (mut below, mut beyond) = (start, end);
                while beyond - below > 1 {
                    let middle = below + (beyond - below) / 2;
                    if above(model, middle)? == side {
                        below = middle;
                    } else {
                        beyond = middle;
                    }
                }
                let (near, far) = (error(model, false, below)?, error(model, false, beyond)?);
                let crossing = if far < near {
                    (beyond, far)
                } else {
                    (below, near)
                };
                if best.is_none_or(|(held, error)| {
                    crossing.1 < error || (crossing.1 == error && crossing.0 < held)
                }) {
                    best = Some(crossing);
                }
                continue;
            }
            // No crossing: the least error on the reduction, then descended on the whole
            // sample.
            let mut fine = (centre, coarse[index].1);
            for step in low..=high {
                let candidate = error(model, true, step)?;
                if candidate < fine.1 || (candidate == fine.1 && step < fine.0) {
                    fine = (step, candidate);
                }
            }
            let (mut step, mut least) = (fine.0, error(model, false, fine.0)?);
            for _ in 0..2 * COARSE {
                let left = if step > -400 {
                    error(model, false, step - 1)?
                } else {
                    f64::INFINITY
                };
                let right = if step < 400 {
                    error(model, false, step + 1)?
                } else {
                    f64::INFINITY
                };
                if left < least && left <= right {
                    (step, least) = (step - 1, left);
                } else if right < least {
                    (step, least) = (step + 1, right);
                } else {
                    break;
                }
            }
            if best.is_none_or(|(held, error)| least < error || (least == error && step < held)) {
                best = Some((step, least));
            }
        }
        Ok(best.expect("the coarse error has a least value").0)
    }
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
    solve_reduced(
        sample,
        targets,
        compile,
        search,
        stride(sample.rgb.len()),
        cancel,
    )
}

/// The solve, its searches located on one of every `stride` points of the sample ([`View`]).
/// Every value it commits and every statistic it reports is the whole sample's; the reduction
/// only decides where a search looks first.
fn solve_reduced(
    sample: &SampleGrid,
    targets: AutoToneTargets,
    compile: impl Fn(AutoToneValues) -> Result<Vec<ColorOperation>, Error>,
    search: ExposureSearch,
    stride: usize,
    cancel: &Cancel,
) -> Result<AutoToneReport, Error> {
    sample.validate()?;
    targets.validate()?;
    cancel.check()?;
    let full = View::new(sample, 1);
    let usable = full.usable();
    if usable < 1024 {
        return Err(refusal(
            "too-few-samples",
            "fewer than 1,024 finite samples",
        ));
    }
    let input = view_statistics(&full, &[], true, false, cancel)?;
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
    // A reduction too small to locate anything is the whole sample.
    let reduced = Some(View::new(sample, stride))
        .filter(|reduced| stride > 1 && reduced.usable() >= 1024)
        .unwrap_or_else(|| View::new(sample, 1));
    let mut model = Evaluator {
        full,
        reduced,
        compile,
        cancel,
        clippings: Vec::new(),
        tones: Vec::new(),
    };
    let exposure = |model: &mut Evaluator<'_, _>, values: AutoToneValues| -> Result<f64, Error> {
        let median = |model: &mut Evaluator<'_, _>, reduced: bool, step: i32| {
            Ok::<f64, Error>(model.statistics(reduced, exposed(values, step), false)?.p50)
        };
        let best = match search {
            // The last step whose median is at most the target, then the nearer of it and the
            // next (the lower on a tie).
            ExposureSearch::Monotone => {
                let hint = (values.exposure * 100.).round() as i32;
                let lower = model.search((-400, 400), hint, targets.median, median)?;
                let upper = (lower + 1).min(400);
                let below = median(model, false, lower)?;
                let above = median(model, false, upper)?;
                if (above - targets.median).abs() < (below - targets.median).abs() {
                    upper
                } else {
                    lower
                }
            }
            // Coarse to fine ([`Evaluator::coarse_to_fine`]).
            ExposureSearch::CoarseToFine => model.coarse_to_fine(values, targets.median)?,
        };
        // Then the highest step at or below it within the bright guard.
        let guarded = model.search(
            (-400, best),
            best,
            targets.bright_guard,
            |model, reduced, step| {
                Ok(model
                    .statistics(reduced, exposed(values, step), false)?
                    .near_white)
            },
        )?;
        Ok(f64::from(guarded) / 100.)
    };
    let endpoints = |model: &mut Evaluator<'_, _>,
                     mut values: AutoToneValues|
     -> Result<AutoToneValues, Error> {
        values.whites = f64::from(model.search(
            (-60, 60),
            values.whites as i32,
            targets.clipping,
            |model, reduced, step| {
                let candidate = AutoToneValues {
                    whites: f64::from(step),
                    ..values
                };
                Ok(model.clipping(reduced, candidate)?.0)
            },
        )?);
        // Negating the grid turns the smallest acceptable Blacks into a last-true search.
        values.blacks = -f64::from(model.search(
            (-60, 60),
            -values.blacks as i32,
            targets.clipping,
            |model, reduced, step| {
                let candidate = AutoToneValues {
                    blacks: -f64::from(step),
                    ..values
                };
                Ok(model.clipping(reduced, candidate)?.1)
            },
        )?);
        Ok(values)
    };
    let mut values = AutoToneValues::default();
    values.exposure = exposure(&mut model, values)?;
    let bands = model.statistics(false, values, false)?;
    values.highlights = (-targets.highlights_scale * bands.bright)
        .clamp(-100., 0.)
        .round();
    values.shadows = (targets.shadows_scale * bands.dark).clamp(0., 60.).round();
    let contrast = model.statistics(false, values, false)?;
    values.contrast = (100. * (targets.spread - (contrast.p90 - contrast.p10)))
        .clamp(-50., 50.)
        .round();
    values = endpoints(&mut model, values)?;
    values.exposure = exposure(&mut model, values)?;
    values = endpoints(&mut model, values)?;
    let tone = model.statistics(false, values, true)?;
    values.vibrance = (60. * (1. - tone.mean_chroma / targets.vibrance_chroma))
        .clamp(0., 25.)
        .round();
    values.saturation = (-60. * (tone.mean_chroma / targets.saturation_chroma - 1.))
        .clamp(-15., 0.)
        .round();
    let output = model.statistics(false, values, true)?;
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

/// `values` with Exposure at `step` hundredths of an EV.
fn exposed(values: AutoToneValues, step: i32) -> AutoToneValues {
    AutoToneValues {
        exposure: f64::from(step) / 100.,
        ..values
    }
}

/// The coarse-to-fine Exposure search's coarse step: 0.10 EV, ten of the field's steps.
const COARSE: i32 = 10;

#[cfg(test)]
mod tests;
