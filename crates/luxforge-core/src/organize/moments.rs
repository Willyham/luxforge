//! Moments (P5): the bursts, brackets and panoramas of one body's frames of one day, in view order.
//!
//! - **Panoramas from folders.** Consecutive frames of one folder whose name, or whose parent's
//!   name, has `pano` in it (any case), each at most [`FOLDER_PANORAMA_GAP_MS`] from the next, are
//!   a panorama: drones keep each panorama's frames in a folder of its own (DJI's
//!   `DCIM/PANORAMA/100_0001`, `PANO0001`), and a person's own `Panoramas` folder says the same.
//!   Two frames are enough. The folder decides before anything else, so the auto-exposed frames of
//!   a drone's sweep are neither a burst nor a bracket.
//! - **Panoramas from previews.** Elsewhere, a chain of consecutive frames each [`pan_linked`] to
//!   the next (at most [`PANORAMA_GAP_MS`] apart and at least [`PANORAMA_MIN_GAP_MS`], one focal
//!   length, exposures at most [`PANORAMA_MAX_EV`] apart) asks the [`MomentProbe`] for its pans,
//!   and [`PANORAMA_MIN_FRAMES`] or more frames whose every link is a measured pan, each carrying
//!   on the sweep of the last ([`Pan::continued_by`]), are a panorama. A panorama found decides
//!   its frames: the runs below are found in the frames between panoramas.
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
//! - **Brackets from previews.** When the metadata cannot say, the [`MomentProbe`] measures the
//!   run (in the frame range) from its decoded previews; distinct measured steps at least
//!   `preview_bracket_step_ev` apart, less [`PREVIEW_TOLERANCE_EV`], make a bracket.
//! - Any other run of two or more frames is a **burst**; a frame alone is a single, with no moment.
//!
//! A bracket's steps are relative to its metered frame: the frame with no bias from metadata
//! ([`metadata_steps`]), the median-ranked frame from previews.
use super::{metadata_steps, middle_ranked};
use crate::catalog_types::{
    Exposure, FolderIndex, FrameFacts, FrameTables, Moment, MomentEvidence, MomentKind,
    MomentProbe, Pan, Thresholds, ViewItem,
};
use std::{collections::HashMap, ops::Range};

/// How far below the metadata step a nominal exposure step may fall and still count. Cameras record
/// nominal values, rounded from the true third-stop series: 1/200 → 1/250 s records 0.32 EV,
/// 1/50 → 1/60 s and f/3.2 → f/3.5 0.26 EV, and 1/13 → 1/15 s, the tightest, 0.21 EV, for a true
/// ⅓ EV each (1/12.7 → 1/16 s). 0.13 EV admits all of them against the default ⅓ EV step (at least
/// 0.20 EV), while a sixth of a stop (0.17 EV) still does not count. Never more than half the step.
pub(super) const NOMINAL_TOLERANCE_EV: f32 = 0.13;
/// How far below the preview step a measured step may fall and still count: a preview's brightness
/// is measured through the camera's tone curve, so the default ⅔ EV means "about ⅔": at least
/// 0.42 EV measured. The preview lane's calibration (`previews/bracket.rs`, brackets within ±2 EV
/// of six synthetic scenes): without a tone curve, as the generated folders render, every link
/// measures 0.97 to 1.01 times its step; through three camera-like curves 0.56 to 1.50 times, a
/// shoulder compressing the brighter links and a contrasty toe expanding the darker ones, so a ⅔ EV
/// link measured 0.45 to 0.95 EV. ⅙ EV below (0.5) kept 25 of those 36 ⅔ EV brackets, ¼ all 36, and
/// still none of the 48 ⅓ EV ones (links of 0.24 to 0.45 EV). Never more than half the step. To be
/// confirmed on a labelled corpus of real brackets.
pub(super) const PREVIEW_TOLERANCE_EV: f32 = 0.25;

/// Frames of a panorama folder further apart than this are two panoramas (1 minute): a drone's
/// sphere of 26 frames takes about a minute, a frame every 2 or 3 seconds, and a person's
/// `Panoramas` folder holds one sweep after another.
pub(super) const FOLDER_PANORAMA_GAP_MS: u64 = 60_000;
/// Frames further apart than this are no panorama's neighbours from previews (10 s): a sweep
/// taken by hand turns, frames and shoots in a second or three a frame.
pub(super) const PANORAMA_GAP_MS: u64 = 10_000;
/// …and frames closer than this neither (250 ms): a burst at 4 frames a second or more following a
/// moving subject pans its background too, but is a burst. Clocks of whole seconds cannot tell, and
/// do not refuse.
pub(super) const PANORAMA_MIN_GAP_MS: u64 = 250;
/// Exposures further apart than this are a bracket's, not a panorama's (3 EV, as the panorama
/// merge refuses).
pub(super) const PANORAMA_MAX_EV: f32 = 3.0;
/// A panorama from previews has at least this many frames: two frames that overlap are as often a
/// subject framed twice.
pub(super) const PANORAMA_MIN_FRAMES: usize = 3;

/// Finds moments, reusing its buffers from one run to the next.
pub(super) struct Finder<'a> {
    thresholds: &'a Thresholds,
    tables: &'a FrameTables,
    probe: &'a dyn MomentProbe,
    exposures: Vec<Exposure>,
    items: Vec<ViewItem>,
    /// Whether each folder asked about so far is a panorama folder.
    panorama_folders: HashMap<FolderIndex, bool>,
}

impl<'a> Finder<'a> {
    pub(super) fn new(
        thresholds: &'a Thresholds,
        tables: &'a FrameTables,
        probe: &'a dyn MomentProbe,
    ) -> Self {
        Self {
            thresholds,
            tables,
            probe,
            exposures: Vec::new(),
            items: Vec::new(),
            panorama_folders: HashMap::new(),
        }
    }

    /// Appends to `moments` the bursts, brackets and panoramas of `frames`, one body's frames of
    /// one day in view order, which start at `offset` in the view: its panoramas from folders,
    /// then from previews in the frames between them, then the runs between every panorama.
    pub(super) fn find(&mut self, frames: &[FrameFacts], offset: usize, moments: &mut Vec<Moment>) {
        let mut panoramas = Vec::new();
        let mut from = 0;
        for (range, evidence) in self.folder_panoramas(frames) {
            self.preview_panoramas(frames, from..range.start, &mut panoramas);
            from = range.end;
            panoramas.push((range, evidence));
        }
        self.preview_panoramas(frames, from..frames.len(), &mut panoramas);
        let mut from = 0;
        for (range, evidence) in panoramas {
            self.runs(&frames[from..range.start], offset + from, moments);
            moments.push(moment(
                &frames[range.clone()],
                offset + range.start,
                panorama(evidence),
            ));
            from = range.end;
        }
        self.runs(&frames[from..], offset + from, moments);
    }

    /// The panoramas of `frames` their folders show, in order.
    fn folder_panoramas(&mut self, frames: &[FrameFacts]) -> Vec<(Range<usize>, MomentEvidence)> {
        let mut found = Vec::new();
        let mut start = 0;
        for at in 1..=frames.len() {
            let joined = at < frames.len() && {
                let (a, b) = (&frames[at - 1], &frames[at]);
                a.folder == b.folder
                    && !a.merged
                    && !b.merged
                    && gap_within(a, b, FOLDER_PANORAMA_GAP_MS)
                    && self.panorama_folder(a.folder)
            };
            if !joined {
                if at - start >= 2 {
                    found.push((start..at, MomentEvidence::Folder));
                }
                start = at;
            }
        }
        found
    }

    /// Whether `folder`, or the folder holding it, is named for panoramas.
    fn panorama_folder(&mut self, folder: FolderIndex) -> bool {
        let tables = self.tables;
        *self.panorama_folders.entry(folder).or_insert_with(|| {
            let path = tables.folder_path(folder);
            [
                path.file_name(),
                path.parent().and_then(|parent| parent.file_name()),
            ]
            .into_iter()
            .flatten()
            .any(|name| names_panoramas(&name.to_string_lossy()))
        })
    }

    /// Appends to `found` the panoramas the previews show among `frames[within]`, in order.
    fn preview_panoramas(
        &mut self,
        frames: &[FrameFacts],
        within: Range<usize>,
        found: &mut Vec<(Range<usize>, MomentEvidence)>,
    ) {
        let mut start = within.start;
        for at in within.start + 1..=within.end {
            if at == within.end || !pan_linked(&frames[at - 1], &frames[at]) {
                if at - start >= PANORAMA_MIN_FRAMES {
                    self.sweeps(&frames[start..at], start, found);
                }
                start = at;
            }
        }
    }

    /// Appends to `found` the sweeps of a chain of linked frames, which starts at `offset`: each
    /// longest stretch of [`PANORAMA_MIN_FRAMES`] or more whose every link the probe measures as a
    /// pan carrying on the last.
    fn sweeps(
        &mut self,
        chain: &[FrameFacts],
        offset: usize,
        found: &mut Vec<(Range<usize>, MomentEvidence)>,
    ) {
        self.items.clear();
        self.items.extend(chain.iter().map(|frame| frame.item));
        let pans = self.probe.pans(&self.items);
        if pans.len() + 1 != chain.len() {
            return;
        }
        let mut start = 0;
        let mut last: Option<Pan> = None;
        for at in 1..=chain.len() {
            // The link from frame `at - 1` to frame `at`, when there is one.
            let carried = (at < chain.len())
                .then(|| pans[at - 1])
                .flatten()
                .filter(|pan| last.is_none_or(|last| last.continued_by(pan)));
            match carried {
                Some(pan) => last = Some(pan),
                None => {
                    let swept = at - start >= PANORAMA_MIN_FRAMES;
                    if swept {
                        found.push((offset + start..offset + at, MomentEvidence::Previews));
                    }
                    // A pan that turns back starts the next stretch at its first frame, unless
                    // that frame ended a sweep.
                    let turned = (at < chain.len() && !swept).then(|| pans[at - 1]).flatten();
                    start = if turned.is_some() { at - 1 } else { at };
                    last = turned;
                }
            }
        }
    }

    /// Appends to `moments` the bursts and brackets of `frames`, which start at `offset`.
    fn runs(&mut self, frames: &[FrameFacts], offset: usize, moments: &mut Vec<Moment>) {
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
                    bracket(MomentEvidence::Metadata, steps),
                ));
            } else if let Some(period) = step.period(&steps, thresholds) {
                for (at, frames) in run.chunks(period).enumerate() {
                    let steps = metadata_steps(&self.exposures[at * period..][..period])
                        .unwrap_or_else(|| from_median(&steps[at * period..][..period]));
                    let found = bracket(MomentEvidence::Metadata, steps);
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
            Some(measured) => bracket(MomentEvidence::Previews, from_median(&measured)),
            None => burst(),
        };
        moments.push(moment(run, offset, found));
    }
}

/// Whether a folder's name says it holds panoramas: `pano` anywhere in it, in any case
/// (`PANORAMA`, `PANO0001`, `Panoramas`).
fn names_panoramas(name: &str) -> bool {
    name.as_bytes()
        .windows(4)
        .any(|window| window.eq_ignore_ascii_case(b"pano"))
}

/// Two dated frames' gap in milliseconds, and whether both clocks record whole seconds only.
fn gap(a: &FrameFacts, b: &FrameFacts) -> Option<(u64, bool)> {
    let (a_ms, b_ms) = (a.instant_ms?, b.instant_ms?);
    let whole_seconds = a_ms.rem_euclid(1000) == 0 && b_ms.rem_euclid(1000) == 0;
    Some((a_ms.abs_diff(b_ms), whole_seconds))
}

/// Whether two dated frames are less than `limit` apart, or up to and including it on clocks of
/// whole seconds.
fn gap_within(a: &FrameFacts, b: &FrameFacts, limit: u64) -> bool {
    gap(a, b).is_some_and(|(gap, whole_seconds)| {
        if whole_seconds {
            gap <= limit
        } else {
            gap < limit
        }
    })
}

/// Whether two consecutive frames of one body are one run.
fn linked(a: &FrameFacts, b: &FrameFacts, thresholds: &Thresholds) -> bool {
    if a.merged || b.merged {
        return false;
    }
    gap_within(a, b, thresholds.run_gap_ms)
        || (gap_within(a, b, thresholds.matched_run_gap_ms)
            && a.exposure.settings_match(&b.exposure))
}

/// Whether two consecutive frames of one body may be neighbours in a panorama from previews:
/// dated, at most [`PANORAMA_GAP_MS`] apart and at least [`PANORAMA_MIN_GAP_MS`] (unless the
/// clocks record whole seconds), at one focal length when both record it, and exposed at most
/// [`PANORAMA_MAX_EV`] apart when both record their settings.
fn pan_linked(a: &FrameFacts, b: &FrameFacts) -> bool {
    if a.merged || b.merged || !gap_within(a, b, PANORAMA_GAP_MS) {
        return false;
    }
    let burst_fast =
        gap(a, b).is_some_and(|(gap, whole_seconds)| !whole_seconds && gap < PANORAMA_MIN_GAP_MS);
    let one_focal = match (a.exposure.focal_mm, b.exposure.focal_mm) {
        (Some(a), Some(b)) => (a - b).abs() <= 0.05 * a.max(b),
        _ => true,
    };
    let exposed_alike = match (a.exposure.ev(), b.exposure.ev()) {
        (Some(a), Some(b)) => (a - b).abs() <= PANORAMA_MAX_EV,
        _ => true,
    };
    !burst_fast && one_focal && exposed_alike
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
    evidence: Option<MomentEvidence>,
    steps_ev: Vec<f32>,
}

fn bracket(evidence: MomentEvidence, steps_ev: Vec<f32>) -> Found {
    Found {
        kind: MomentKind::Bracket,
        evidence: Some(evidence),
        steps_ev,
    }
}

fn panorama(evidence: MomentEvidence) -> Found {
    Found {
        kind: MomentKind::Panorama,
        evidence: Some(evidence),
        steps_ev: Vec::new(),
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
        picked: 0,
    }
}
