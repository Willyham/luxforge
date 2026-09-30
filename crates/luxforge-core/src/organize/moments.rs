//! Moments (P5): the bursts and brackets of one body's frames of one day, in view order.
//!
//! - **Runs.** Consecutive frames less than `run_gap_ms` apart are one run, and so are frames less
//!   than `matched_run_gap_ms` apart whose aperture, ISO and focal length match
//!   ([`Exposure::settings_match`](crate::catalog_types::Exposure::settings_match)), since a
//!   bracket at slow shutter speeds spaces its frames further. Many cameras record capture time to
//!   the second only (no `SubSecTimeOriginal`: most DNGs, Olympus ORF); when both instants are whole
//!   seconds a gap equal to the threshold joins too, so a 10 fps burst that crosses a second
//!   boundary, which such a clock shows as exactly 1000 ms, stays one run.
//! - **Brackets from metadata.** A run of `bracket_min_frames..=bracket_max_frames` whose
//!   [`metadata_steps`] are known is a bracket when its exposures are distinct, each at least
//!   `metadata_bracket_step_ev` from the next once sorted, less [`NOMINAL_TOLERANCE_EV`]. A longer
//!   run (or one whose values repeat) that repeats one bracket's pattern with a period in the frame
//!   range is that many brackets, as an HDR panorama shoots them one after another.
//! - **Brackets from previews.** When the metadata cannot say, the [`BracketProbe`] measures the
//!   run (in the frame range) from its decoded previews; distinct measured steps at least
//!   `preview_bracket_step_ev` apart, less [`PREVIEW_TOLERANCE_EV`], make a bracket.
//! - Any other run of two or more frames is a **burst**; a frame alone is a single, with no moment.
//!
//! A bracket's steps are relative to its metered frame: the frame with no bias from metadata
//! ([`metadata_steps`]), the median-ranked frame from previews.
use super::{metadata_steps, middle_ranked};
use crate::catalog_types::{
    BracketEvidence, BracketProbe, Exposure, FrameFacts, Moment, MomentKind, Thresholds, ViewItem,
};

/// How far below the metadata step a nominal exposure step may fall and still count. Cameras record
/// nominal values, rounded from the true third-stop series: 1/200 → 1/250 s records 0.32 EV,
/// 1/50 → 1/60 s and f/3.2 → f/3.5 0.26 EV, and 1/13 → 1/15 s, the tightest, 0.21 EV, for a true
/// ⅓ EV each (1/12.7 → 1/16 s). 0.13 EV admits all of them against the default ⅓ EV step (at least
/// 0.20 EV), while a sixth of a stop (0.17 EV) still does not count. Never more than half the step.
pub(super) const NOMINAL_TOLERANCE_EV: f32 = 0.13;
/// How far below the preview step a measured step may fall and still count: a preview's brightness
/// is measured through the camera's tone curve and clipping, which compress a bracket's outer
/// steps, so the default ⅔ EV means "about ⅔": at least 0.5 EV measured. Never more than half the
/// step. To be confirmed with lane B's measurements on a labelled corpus.
pub(super) const PREVIEW_TOLERANCE_EV: f32 = 1.0 / 6.0;

/// Finds moments, reusing its buffers from one run to the next.
pub(super) struct Finder<'a> {
    thresholds: &'a Thresholds,
    probe: &'a dyn BracketProbe,
    exposures: Vec<Exposure>,
    items: Vec<ViewItem>,
}

impl<'a> Finder<'a> {
    pub(super) fn new(thresholds: &'a Thresholds, probe: &'a dyn BracketProbe) -> Self {
        Self {
            thresholds,
            probe,
            exposures: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Appends to `moments` the bursts and brackets of `frames`, one body's frames of one day in
    /// view order, which start at `offset` in the view.
    pub(super) fn find(&mut self, frames: &[FrameFacts], offset: usize, moments: &mut Vec<Moment>) {
        let mut start = 0;
        for at in 1..=frames.len() {
            if at == frames.len() || !linked(&frames[at - 1], &frames[at], self.thresholds) {
                if at - start >= 2 {
                    self.classify(&frames[start..at], offset + start, moments);
                }
                start = at;
            }
        }
    }

    /// The moments of one run of two or more frames.
    fn classify(&mut self, run: &[FrameFacts], offset: usize, moments: &mut Vec<Moment>) {
        let thresholds = self.thresholds;
        let fits = (thresholds.bracket_min_frames as usize
            ..=thresholds.bracket_max_frames as usize)
            .contains(&run.len());
        self.exposures.clear();
        self.exposures
            .extend(run.iter().map(|frame| frame.exposure));
        let step = Step::new(thresholds.metadata_bracket_step_ev, NOMINAL_TOLERANCE_EV);
        if let Some(steps) = metadata_steps(&self.exposures) {
            if fits && step.apart(&steps) {
                moments.push(moment(
                    run,
                    offset,
                    bracket(BracketEvidence::Metadata, steps),
                ));
            } else if let Some(period) = step.period(&steps, thresholds) {
                for (at, frames) in run.chunks(period).enumerate() {
                    let steps = metadata_steps(&self.exposures[at * period..][..period])
                        .unwrap_or_else(|| from_median(&steps[at * period..][..period]));
                    let found = bracket(BracketEvidence::Metadata, steps);
                    moments.push(moment(frames, offset + at * period, found));
                }
            } else {
                moments.push(moment(run, offset, burst()));
            }
            return;
        }
        let step = Step::new(thresholds.preview_bracket_step_ev, PREVIEW_TOLERANCE_EV);
        let measured = fits
            .then(|| {
                self.items.clear();
                self.items.extend(run.iter().map(|frame| frame.item));
                self.probe.measure(&self.items)
            })
            .flatten()
            .filter(|measured| measured.len() == run.len() && step.apart(measured));
        let found = match measured {
            Some(measured) => bracket(BracketEvidence::Previews, from_median(&measured)),
            None => burst(),
        };
        moments.push(moment(run, offset, found));
    }
}

/// Whether two consecutive frames of one body are one run.
fn linked(a: &FrameFacts, b: &FrameFacts, thresholds: &Thresholds) -> bool {
    let (Some(a_ms), Some(b_ms)) = (a.instant_ms, b.instant_ms) else {
        return false;
    };
    let gap = a_ms.abs_diff(b_ms);
    let whole_seconds = a_ms.rem_euclid(1000) == 0 && b_ms.rem_euclid(1000) == 0;
    let within = |limit: u64| {
        if whole_seconds {
            gap <= limit
        } else {
            gap < limit
        }
    };
    within(thresholds.run_gap_ms)
        || (within(thresholds.matched_run_gap_ms) && a.exposure.settings_match(&b.exposure))
}

/// A bracket's step rule: sorted values at least `apart` from one another, and values within
/// `same` of one another counted as one exposure when a pattern repeats.
struct Step {
    apart: f32,
    same: f32,
}

impl Step {
    fn new(step: f32, tolerance: f32) -> Self {
        let same = tolerance.min(step / 2.0);
        Self {
            apart: step - same,
            same,
        }
    }

    /// Whether `values` are all finite and, sorted, each at least the step from the next.
    fn apart(&self, values: &[f32]) -> bool {
        if values.iter().any(|value| !value.is_finite()) {
            return false;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f32::total_cmp);
        sorted
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= self.apart)
    }

    /// The shortest period in the bracket frame range, shorter than the run and dividing it, with
    /// which `values` repeat and every period of which is a bracket.
    fn period(&self, values: &[f32], thresholds: &Thresholds) -> Option<usize> {
        let count = values.len();
        (thresholds.bracket_min_frames as usize..=thresholds.bracket_max_frames as usize)
            .filter(|period| *period < count && count.is_multiple_of(*period))
            .find(|&period| {
                values
                    .iter()
                    .zip(&values[period..])
                    .all(|(a, b)| (a - b).abs() <= self.same)
                    && values.chunks(period).all(|chunk| self.apart(chunk))
            })
    }
}

/// Measured values relative to their median-ranked one.
fn from_median(values: &[f32]) -> Vec<f32> {
    let reference = values[middle_ranked(values)];
    values.iter().map(|value| value - reference).collect()
}

/// What a moment is: its kind, its evidence and its steps.
struct Found {
    kind: MomentKind,
    evidence: Option<BracketEvidence>,
    steps_ev: Vec<f32>,
}

fn bracket(evidence: BracketEvidence, steps_ev: Vec<f32>) -> Found {
    Found {
        kind: MomentKind::Bracket,
        evidence: Some(evidence),
        steps_ev,
    }
}

fn burst() -> Found {
    Found {
        kind: MomentKind::Burst,
        evidence: None,
        steps_ev: Vec::new(),
    }
}

fn moment(frames: &[FrameFacts], offset: usize, found: Found) -> Moment {
    let instant = |frame: &FrameFacts| frame.instant_ms.unwrap_or_default();
    Moment {
        kind: found.kind,
        evidence: found.evidence,
        steps_ev: found.steps_ev,
        span_ms: instant(&frames[0]).abs_diff(instant(&frames[frames.len() - 1])),
        start: offset as u32,
        len: frames.len() as u32,
    }
}
