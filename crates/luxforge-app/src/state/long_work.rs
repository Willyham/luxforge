//! Long-running work as the desktop shows it ([catalog design](../../../../docs/design/catalog.md#long-running-work),
//! P18): the status bar's busiest job with a small bar and the number of jobs, which opens the
//! Performance section; the in-view progress sheet of a Select view with nothing to show yet; the
//! estimate and Cancel of each Performance row that shows catalog work; and the sentence a finished
//! job leaves in the status bar. Like every view model it names no framework type and reads no
//! clock: time enters only through the board's own `elapsed_ms`, so every rule is testable with
//! made-up snapshots.
//!
//! **What counts.** Long-running work is the catalog's jobs — indexing, reading previews, developing
//! picks, checking and finding originals, batch preset and export — the activity board kinds the
//! core names in `catalog_types::jobs` ([`followed`]). The desktop's own renders and preparations
//! have their own indicators and stay the Performance section's plain rows. A job is shown once it
//! has run [`LONG_JOB_MS`], as the section shows work, so the churn of a scroll's single-file
//! previews never flickers in the status bar.
//!
//! **Honest numbers.** A count is the work's own words (`312 of 612 headers read`, `48,210 of about
//! 200,000 files` while a walk is still discovering its extent); a bar is filled only with the
//! fraction the work reports; an estimate is shown only once the rate is steady ([`Rates`]) and is
//! withdrawn when the rate collapses or the work stalls, and otherwise the row says nothing about
//! time, while the count reads `working` when the work can state none. Nothing here invents a
//! fraction, so a stuck job is never drawn nearly done.
//!
//! **The busiest job** is the running one that began first: the one busy longest, which does not
//! change while it runs, so the status bar never jumps between jobs as their estimates move. The job
//! count beside it counts every shown job, it included.
use super::Inputs;
use crate::state::performance::LONG_JOB_MS;
use crate::state::select::{Shown, shown_path, thousands};
use luxforge_core::{
    ActivitySnapshot,
    activity::{ActiveActivity, ActivityEntry, Outcome, RecentActivity},
    catalog_types::jobs::{INDEX_REFRESH, catalog_job},
};
use serde_json::Value;
use std::{collections::VecDeque, path::Path};

/// An estimate needs at least this many progress updates in its window…
pub(crate) const STEADY_UPDATES: usize = 4;
/// …spanning at least this long…
pub(crate) const STEADY_SPAN_MS: u64 = 2_000;
/// …and the rate over its newest [`STEADY_UPDATES`] updates within this share of the rate over the
/// whole window: 25%, so a rate that is still settling shows no estimate.
pub(crate) const STEADY_TOLERANCE: f64 = 0.25;
/// Once shown, an estimate stays while the newest updates' rate is within this share of the
/// window's, so ordinary jitter does not make it flicker; past it the rate has collapsed (or
/// surged) and the estimate is withdrawn until it is steady again.
pub(crate) const COLLAPSE_TOLERANCE: f64 = 0.5;
/// The rate is measured over the updates of the last this many milliseconds, so it follows the
/// work as it is now rather than as it began.
pub(crate) const RATE_WINDOW_MS: u64 = 10_000;
/// The most updates kept for one job: the window's, bounded however fast the work reports.
pub(crate) const MAX_RATE_SAMPLES: usize = 32;
/// A job has stalled — its estimate withdrawn — when nothing has moved for this many times its usual
/// time between updates…
pub(crate) const STALL_FACTOR: f64 = 3.0;
/// …and at least this long.
pub(crate) const STALL_MIN_MS: u64 = 2_000;
/// A finished job leaves a sentence in the status bar when it ran at least this long: every job over
/// a second, as the design counts long-running work.
pub(crate) const FINISHED_SENTENCE_MS: u64 = 1_000;
/// A place longer than this is named by its last component in a label (`…/images`).
pub(crate) const MAX_PLACE_CHARS: usize = 28;

/// What the progress sheet says about the first look at a folder or card.
pub(crate) const SHEET_NOTE: &str = "The first look at a card or folder reads every file's \
    header. Frames appear as soon as it is done; previews follow.";

/// Whether a board entry is long-running catalog work: a kind a catalog job publishes, with the job
/// `job.read` answers and `job.cancel` stops.
pub(crate) fn followed(entry: &ActivityEntry) -> bool {
    entry.job_id.is_some() && catalog_job(&entry.kind).is_some()
}

/// What long-running work has read and chosen, which the model is derived from.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LongWorkState {
    /// The board as the desktop last read it: the one snapshot the status bar, the progress sheet
    /// and the Performance section's job rows are all derived from, so they never disagree.
    pub(crate) board: Option<ActivitySnapshot>,
    /// Moves with every read of the board, and never otherwise: what the Performance section is
    /// rebuilt on beside its own samples.
    pub(crate) version: u64,
    /// Each running job's recent progress, for its estimate.
    pub(crate) rates: Rates,
    /// The Select view waiting on a job before it can show anything.
    pub(crate) waiting: Option<Waiting>,
    /// The job whose sheet was sent to the background: it keeps running, in the section and the
    /// status bar, without the sheet.
    pub(crate) background: Option<String>,
}

impl LongWorkState {
    /// Take in one read of the board.
    pub(crate) fn observe(&mut self, snapshot: ActivitySnapshot) {
        self.rates.observe(&snapshot);
        self.board = Some(snapshot);
        self.version = self.version.wrapping_add(1);
    }

    /// The followed jobs running now, oldest first, as the board lists them.
    pub(crate) fn running(&self) -> impl Iterator<Item = &ActiveActivity> {
        self.board
            .iter()
            .flat_map(|board| board.active.iter())
            .filter(|job| followed(&job.entry))
    }

    /// The running followed job `job_id`, if the board lists it.
    pub(crate) fn job(&self, job_id: &str) -> Option<&ActiveActivity> {
        self.running()
            .find(|job| job.entry.job_id.as_deref() == Some(job_id))
    }
}

/// A Select view that has nothing to show until a job ends: the first look at a folder or card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub(crate) job_id: String,
    /// What is read, as the sheet's title names it: a folder's name, or a card's label.
    pub(crate) name: String,
    pub(crate) card: bool,
}

/// What long-running work shows now.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LongWorkModel {
    /// The busiest job, for the status bar; none while nothing runs.
    pub(crate) busiest: Option<BusiestJob>,
    /// The in-view progress sheet: only for a Select view with nothing to show yet, such as the
    /// first look at a card or folder before its headers are read.
    pub(crate) sheet: Option<ProgressSheet>,
}

/// The status bar's job: what the busiest job is doing, how far it has got when its total is known,
/// and how many jobs run.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BusiestJob {
    pub(crate) job_id: String,
    pub(crate) label: String,
    /// `0.0..=1.0`, only for work that knows its total; none draws no fill and says "working".
    pub(crate) fraction: Option<f32>,
    pub(crate) jobs: u32,
}

/// The in-view progress sheet: the job it follows, its title and note, how far it has got and
/// the estimate once one is truthful.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ProgressSheet {
    pub(crate) job_id: String,
    /// A card is read (the drive glyph) rather than a folder.
    pub(crate) card: bool,
    pub(crate) title: String,
    pub(crate) note: String,
    /// "312 of 612 headers read", or none while the work states no count.
    pub(crate) count: Option<String>,
    pub(crate) fraction: Option<f32>,
    /// "about 4 s left", once the rate is steady.
    pub(crate) estimate: Option<String>,
}

/// What a Performance row adds for catalog work: the job its Cancel stops, the work's own count and
/// the estimate once the rate is steady.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WorkInfo {
    pub(crate) job_id: String,
    pub(crate) count: Option<String>,
    /// `about 1 min 40 s`.
    pub(crate) estimate: Option<String>,
}

/// Long-running work's model, derived from the inputs after every message.
pub(crate) fn derive(inputs: &Inputs<'_>) -> LongWorkModel {
    model(
        inputs.long_work,
        inputs.select.shown == Shown::Select,
        inputs.select.home.as_deref(),
    )
}

/// The model for this state, with Select shown or not.
pub(crate) fn model(
    state: &LongWorkState,
    select_shown: bool,
    home: Option<&Path>,
) -> LongWorkModel {
    let shown: Vec<&ActiveActivity> = state
        .running()
        .filter(|job| job.elapsed_ms >= LONG_JOB_MS)
        .collect();
    let busiest = shown.first().map(|job| BusiestJob {
        job_id: job.entry.job_id.clone().unwrap_or_default(),
        label: work_label(&job.entry, home),
        fraction: fraction(&job.entry),
        jobs: u32::try_from(shown.len()).unwrap_or(u32::MAX),
    });
    LongWorkModel {
        busiest,
        sheet: select_shown.then(|| sheet(state)).flatten(),
    }
}

/// The sheet over the view waiting on a job: only while that job runs and has run long enough to
/// be worth a sheet, and not once it was sent to the background. It goes away by itself when the
/// job ends, since the view can then draw its first frames.
fn sheet(state: &LongWorkState) -> Option<ProgressSheet> {
    let waiting = state.waiting.as_ref()?;
    if state.background.as_deref() == Some(waiting.job_id.as_str()) {
        return None;
    }
    let job = state
        .job(&waiting.job_id)
        .filter(|job| job.elapsed_ms >= LONG_JOB_MS)?;
    Some(ProgressSheet {
        job_id: waiting.job_id.clone(),
        card: waiting.card,
        title: if waiting.card {
            format!("Reading the {} card", waiting.name)
        } else {
            format!("Reading {}", waiting.name)
        },
        note: SHEET_NOTE.to_owned(),
        count: count(&job.entry),
        fraction: fraction(&job.entry),
        estimate: state
            .rates
            .remaining_ms(job.entry.id)
            .map(|ms| format!("{} left", format_remaining(ms))),
    })
}

/// A Performance row's catalog-work part for a running entry, or none for work that is not the
/// catalog's: that row stays a plain job row.
pub(crate) fn work_info(job: &ActiveActivity, rates: &Rates) -> Option<WorkInfo> {
    if !followed(&job.entry) {
        return None;
    }
    Some(WorkInfo {
        job_id: job.entry.job_id.clone()?,
        count: count(&job.entry),
        estimate: rates.remaining_ms(job.entry.id).map(format_remaining),
    })
}

/// The work's own count, as it words it.
fn count(entry: &ActivityEntry) -> Option<String> {
    entry
        .progress
        .as_ref()
        .and_then(|progress| progress.message.clone())
}

/// The fraction the work reports, only when it knows its total.
fn fraction(entry: &ActivityEntry) -> Option<f32> {
    entry
        .progress
        .as_ref()
        .and_then(|progress| progress.fraction)
        .filter(|fraction| fraction.is_finite())
        .map(|fraction| fraction.clamp(0.0, 1.0) as f32)
}

/// What a job is doing, for people: `Indexing ~/Pictures`, `Indexing the NIKON Z 8 card`, `Reading
/// previews`, `Rendering previews · DSC_0412.NEF`. Indexing acts on its place, which follows the
/// verb; any other detail follows a dot.
pub(crate) fn work_label(entry: &ActivityEntry, home: Option<&Path>) -> String {
    match entry.detail.as_deref() {
        None => entry.label.to_string(),
        Some(detail) if entry.kind == INDEX_REFRESH.activity => {
            format!("{} {}", entry.label, place(detail, home))
        }
        Some(detail) => format!("{} \u{b7} {detail}", entry.label),
    }
}

/// A place a job names: a path under the home folder from `~`, and a long one by its last
/// component; anything else (`the NIKON Z 8 card`, `3 indexed folders`) as it is.
pub(crate) fn place(detail: &str, home: Option<&Path>) -> String {
    let path = Path::new(detail);
    if !path.is_absolute() {
        return detail.to_owned();
    }
    let shown = shown_path(path, home);
    if shown.chars().count() <= MAX_PLACE_CHARS {
        return shown;
    }
    path.file_name()
        .map_or(shown, |name| format!("\u{2026}/{}", name.to_string_lossy()))
}

/// The sentence a finished job leaves in the status bar, from its board entry and, when it has one,
/// its `job.read` record: `Indexed 12,408 files in ~/Pictures`, `Cancelled indexing ~/Pictures`,
/// `Indexing ~/Pictures failed: …`, `Developed picks · 18 of 18`. None for work that ran less than
/// [`FINISHED_SENTENCE_MS`] or is not the catalog's.
pub(crate) fn finished_sentence(
    job: &RecentActivity,
    record: Option<&Value>,
    home: Option<&Path>,
) -> Option<String> {
    let entry = &job.entry;
    if !followed(entry) || job.duration_ms < FINISHED_SENTENCE_MS {
        return None;
    }
    let what = work_label(entry, home);
    Some(match job.outcome {
        Outcome::Cancelled => format!("Cancelled {}", lowercase_first(&what)),
        Outcome::Failed => match record.and_then(|record| record["error"]["message"].as_str()) {
            Some(reason) => format!("{what} failed: {reason}"),
            None => format!("{what} failed"),
        },
        Outcome::Completed => {
            let indexed = record
                .and_then(|record| record["result"]["files"].as_u64())
                .filter(|_| entry.kind == INDEX_REFRESH.activity);
            match (indexed, entry.detail.as_deref()) {
                (Some(files), Some(detail)) => {
                    let files = u32::try_from(files).unwrap_or(u32::MAX);
                    let noun = if files == 1 { "file" } else { "files" };
                    let on = if detail.ends_with(" card") {
                        "on"
                    } else {
                        "in"
                    };
                    format!(
                        "Indexed {} {noun} {on} {}",
                        thousands(files),
                        place(detail, home)
                    )
                }
                _ => {
                    let done = past_tense(entry, home);
                    match count(entry) {
                        Some(count) => format!("{done} \u{b7} {count}"),
                        None => done,
                    }
                }
            }
        }
    })
}

/// What a finished job did: its catalog job's words in the past, with its detail as its label
/// places it.
fn past_tense(entry: &ActivityEntry, home: Option<&Path>) -> String {
    let verb = catalog_job(&entry.kind).map_or(entry.label.as_ref(), |job| match job.activity {
        "index.refresh" => "Indexed",
        "preview.extract" => "Read previews",
        "preview.region" => "Checked focus",
        "preview.photo" => "Rendered previews",
        "pick.develop" => "Developed picks",
        "source.check" => "Checked originals",
        "source.find" => "Searched for originals",
        "source.locate" => "Verified original",
        "batch.apply-preset" => "Applied preset",
        "batch.export" => "Exported",
        _ => job.label,
    });
    match entry.detail.as_deref() {
        None => verb.to_owned(),
        Some(detail) if entry.kind == INDEX_REFRESH.activity => {
            format!("{verb} {}", place(detail, home))
        }
        Some(detail) => format!("{verb} \u{b7} {detail}"),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How long is left, as a person reads an estimate: whole seconds under a minute (`about 4 s`, never
/// `about 0 s`), then minutes and tens of seconds (`about 1 min 40 s`), whole minutes from ten
/// minutes (`about 14 min`) and hours with minutes from an hour (`about 1 h 20 min`).
pub(crate) fn format_remaining(ms: u64) -> String {
    let seconds = ms.div_ceil(1000).max(1);
    if seconds < 60 {
        return format!("about {seconds} s");
    }
    if seconds < 600 {
        let tens = (seconds + 5) / 10 * 10;
        let (minutes, rest) = (tens / 60, tens % 60);
        return if rest == 0 {
            format!("about {minutes} min")
        } else {
            format!("about {minutes} min {rest} s")
        };
    }
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        return format!("about {minutes} min");
    }
    match (minutes / 60, minutes % 60) {
        (hours, 0) => format!("about {hours} h"),
        (hours, rest) => format!("about {hours} h {rest} min"),
    }
}

/// Each running job's recent progress, keyed by its board entry: what an estimate is made from.
/// Bounded by the board's own bound on active entries, each holding at most [`MAX_RATE_SAMPLES`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Rates {
    tracks: Vec<Track>,
}

/// One job's progress updates: `(elapsed_ms, fraction)` at each read that found the fraction moved,
/// the entry's elapsed time at the latest read, and whether its rate is steady.
#[derive(Clone, Debug, Default, PartialEq)]
struct Track {
    id: u64,
    samples: VecDeque<(u64, f64)>,
    now_ms: u64,
    steady: bool,
}

impl Rates {
    /// No job's progress: what a section with nothing read estimates from.
    pub(crate) const EMPTY: Self = Self { tracks: Vec::new() };

    /// Take in one read of the board: every followed running job's fraction at its elapsed time.
    /// Jobs no longer running are forgotten.
    pub(crate) fn observe(&mut self, snapshot: &ActivitySnapshot) {
        let running: Vec<&ActiveActivity> = snapshot
            .active
            .iter()
            .filter(|job| followed(&job.entry))
            .collect();
        self.tracks
            .retain(|track| running.iter().any(|job| job.entry.id == track.id));
        for job in running {
            let index = match self
                .tracks
                .iter()
                .position(|track| track.id == job.entry.id)
            {
                Some(index) => index,
                None => {
                    self.tracks.push(Track {
                        id: job.entry.id,
                        ..Track::default()
                    });
                    self.tracks.len() - 1
                }
            };
            let fraction = job
                .entry
                .progress
                .as_ref()
                .and_then(|progress| progress.fraction)
                .filter(|fraction| fraction.is_finite());
            self.tracks[index].observe(job.elapsed_ms, fraction);
        }
    }

    /// How long job `id` has left, once its rate is steady.
    pub(crate) fn remaining_ms(&self, id: u64) -> Option<u64> {
        let track = self.tracks.iter().find(|track| track.id == id)?;
        track.steady.then(|| track.remaining_ms()).flatten()
    }

    /// Whether job `id`'s rate is steady now.
    #[cfg(test)]
    pub(crate) fn steady(&self, id: u64) -> bool {
        self.tracks
            .iter()
            .any(|track| track.id == id && track.steady)
    }
}

impl Track {
    /// One read: the job's elapsed time and the fraction it reports, if it knows its total.
    fn observe(&mut self, now_ms: u64, fraction: Option<f64>) {
        self.now_ms = now_ms;
        match fraction {
            // No truthful extent: nothing to measure a rate by.
            None => self.samples.clear(),
            Some(fraction) => match self.samples.back() {
                // Progress went back: a new phase measures from its own start.
                Some(&(_, last)) if fraction < last => {
                    self.samples.clear();
                    self.samples.push_back((now_ms, fraction));
                }
                Some(&(_, last)) if fraction == last => {}
                _ => self.samples.push_back((now_ms, fraction)),
            },
        }
        while self.samples.len() > MAX_RATE_SAMPLES
            || self
                .samples
                .front()
                .is_some_and(|&(at, _)| at + RATE_WINDOW_MS < now_ms)
        {
            self.samples.pop_front();
        }
        let tolerance = if self.steady {
            COLLAPSE_TOLERANCE
        } else {
            STEADY_TOLERANCE
        };
        self.steady = self.steady_within(tolerance);
    }

    /// Enough updates over enough time, a rate over the newest updates within `tolerance` of the
    /// window's, no stall, and time left to estimate.
    fn steady_within(&self, tolerance: f64) -> bool {
        let n = self.samples.len();
        if n < STEADY_UPDATES {
            return false;
        }
        let (first_at, first) = self.samples[0];
        let (last_at, last) = self.samples[n - 1];
        let span = last_at.saturating_sub(first_at);
        if span < STEADY_SPAN_MS {
            return false;
        }
        let window = (last - first) / span as f64;
        let (recent_at, recent) = self.samples[n - STEADY_UPDATES];
        let recent_span = last_at.saturating_sub(recent_at);
        if window <= 0.0 || recent_span == 0 {
            return false;
        }
        let newest = (last - recent) / recent_span as f64;
        let usual_gap = span as f64 / (n - 1) as f64;
        let quiet = self.now_ms.saturating_sub(last_at) as f64;
        let stalled = quiet > (STALL_FACTOR * usual_gap).max(STALL_MIN_MS as f64);
        !stalled && (newest / window - 1.0).abs() <= tolerance && self.remaining_ms().is_some()
    }

    /// The time left at the window's rate, counted from the latest read; none when the work is
    /// already overdue by that rate, which is never shown as `about 0 s`.
    fn remaining_ms(&self) -> Option<u64> {
        let (first_at, first) = *self.samples.front()?;
        let (last_at, last) = *self.samples.back()?;
        let span = last_at.checked_sub(first_at).filter(|span| *span > 0)?;
        let rate = (last - first) / span as f64;
        if rate <= 0.0 {
            return None;
        }
        let left = (1.0 - last).max(0.0) / rate - self.now_ms.saturating_sub(last_at) as f64;
        (left >= 1.0).then_some(left as u64)
    }
}

#[cfg(test)]
mod tests;
