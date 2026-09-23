//! The Performance section's model: what the state panel's last block says about this editor's own
//! memory, CPU and GPU use and its long-running work, derived from the `resources.read` and
//! `activity.list` answers the section's sampler collected (`docs/design/performance-panel.md`).
//!
//! Everything here is a pure function of those answers. The counters are cumulative, so a rate
//! comes from two consecutive samples; values are formatted in Activity Monitor's units; each series
//! is normalised against its own scale rule; and the job rows follow the section's display rules.
//! Time enters only through the answers themselves — the monotonic clock each read was taken at and
//! the elapsed and ended times each board snapshot carries — so nothing here reads a clock and
//! every rule can be tested with made-up samples.
use crate::state::Inputs;
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};

/// The sparklines' window: one sample a second for a minute.
pub(crate) const WINDOW: usize = 60;

/// How many raw samples the history keeps: one more than the window, because a rate needs a pair
/// of samples and the CPU and GPU lines should fill the window as the memory line does.
pub(crate) const MAX_SAMPLES: usize = WINDOW + 1;

/// Work shorter than this is never shown, running or finished: at one sample a second it would be
/// a row that flickers in and out. The API still reports every entry; this is a view rule.
pub(crate) const LONG_JOB_MS: u64 = 500;

/// How long a finished long job stays in the section once nothing long is running.
pub(crate) const RECENT_JOB_MS: u64 = 10_000;

/// At most this many running jobs get a row; the rest are counted in a caption under them.
pub(crate) const MAX_JOB_ROWS: usize = 4;

/// What a value reads while there is nothing truthful to show: an unavailable counter, or a rate
/// before its second sample.
pub(crate) const DASH: &str = "\u{2013}";

const MIB: u128 = 1 << 20;
const GIB: u128 = 1 << 30;

/// The memory scale never goes below this, so a small, flat footprint draws a low line rather than
/// one that fills the sparkline's height with noise.
const MEMORY_SCALE_FLOOR: f64 = (64 * MIB) as f64;

/// The memory scale's headroom over the window's largest sample, so the peak never touches the top.
const MEMORY_HEADROOM: f64 = 1.1;

/// GPU time is drawn against one GPU's worth of time.
const GPU_SCALE: f64 = 100.0;

/// One `resources.read` answer, as much of it as the section reads. The core's report types are
/// serialise-only, so the answer is read here the way any API client would read it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct ResourceSample {
    /// Only meaningful as the difference between two samples.
    pub(crate) monotonic_ns: u64,
    pub(crate) cpu: CpuCounters,
    pub(crate) memory: MemoryCounters,
    pub(crate) gpu: GpuCounters,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct CpuCounters {
    pub(crate) time_ns: Option<u64>,
    pub(crate) logical_cpus: u32,
    /// The platform's reason for each counter it cannot give, by key.
    #[serde(default)]
    pub(crate) unavailable: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct MemoryCounters {
    /// Which measure `bytes` is: `footprint`, `resident` or `private`.
    pub(crate) kind: String,
    pub(crate) bytes: Option<u64>,
    pub(crate) peak_bytes: Option<u64>,
    pub(crate) resident_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) unavailable: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct GpuCounters {
    pub(crate) time_ns: Option<u64>,
    pub(crate) allocated_bytes: Option<u64>,
    /// Whether GPU allocations are part of `memory.bytes`.
    pub(crate) unified_memory: Option<bool>,
    #[serde(default)]
    pub(crate) unavailable: BTreeMap<String, String>,
}

/// One `activity.list` answer, as much of it as the section reads: the rows need labels, details,
/// phases, progress and times, never an entry's kind, identity or job.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct ActivityList {
    /// Oldest first.
    pub(crate) active: Vec<ActiveJob>,
    /// Newest first.
    pub(crate) recent: Vec<RecentJob>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct ActiveJob {
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
    pub(crate) phase: Option<String>,
    pub(crate) progress: Option<JobProgress>,
    /// Milliseconds since the work began, at the moment of the snapshot.
    pub(crate) elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct RecentJob {
    pub(crate) label: String,
    /// `completed`, `cancelled` or `failed`.
    pub(crate) outcome: String,
    pub(crate) duration_ms: u64,
    /// Milliseconds since it ended, at the moment of the snapshot.
    pub(crate) ended_ms_ago: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct JobProgress {
    pub(crate) done: u64,
    pub(crate) total: u64,
}

/// The sampler's window of raw samples and the board it read last. It is bounded to
/// [`MAX_SAMPLES`] small samples, and its version moves with every change, so the section is
/// re-derived — and its sparklines re-tessellated — once per sample rather than once per message.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PerformanceHistory {
    samples: VecDeque<ResourceSample>,
    activity: Option<ActivityList>,
    version: u64,
}

impl PerformanceHistory {
    /// Forget every sample: the section starts a fresh window whenever it starts sampling, because
    /// a line drawn across a gap in the samples would misplace every point before it.
    pub(crate) fn clear(&mut self) {
        self.samples.clear();
        self.activity = None;
        self.version = self.version.wrapping_add(1);
    }

    /// Record one read of the counters and the board, dropping the oldest sample past the bound.
    pub(crate) fn push(&mut self, sample: ResourceSample, activity: ActivityList) {
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.activity = Some(activity);
        self.version = self.version.wrapping_add(1);
    }

    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }

    pub(crate) fn samples(&self) -> &VecDeque<ResourceSample> {
        &self.samples
    }

    pub(crate) fn activity(&self) -> Option<&ActivityList> {
        self.activity.as_ref()
    }

    /// Changes whenever the samples do, and never otherwise.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }
}

/// One metric row as the section shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricRow {
    pub(crate) label: &'static str,
    /// The figure (`1.42`, `38`), or [`DASH`].
    pub(crate) value: String,
    /// `MB`, `GB` or `%`; empty beside a dash.
    pub(crate) unit: &'static str,
    /// The counter exists on this platform. A rate that is only waiting for its second sample is
    /// available; one the platform cannot give is not.
    pub(crate) available: bool,
    /// Oldest first, at most [`WINDOW`] values, each in `0.0..=1.0` against the row's own scale.
    pub(crate) series: Vec<f32>,
    pub(crate) tooltip: String,
}

/// One job row as the section shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobRow {
    pub(crate) label: String,
    /// Elapsed while running, duration once finished; empty on the quiet row.
    pub(crate) trailing: String,
    pub(crate) detail: Option<String>,
    /// Only for work that reports a truthful total.
    pub(crate) progress: Option<f32>,
    pub(crate) running: bool,
}

/// The job rows, the `+N more` caption under them and the heading's caption.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Jobs {
    pub(crate) rows: Vec<JobRow>,
    pub(crate) more: Option<String>,
    pub(crate) caption: Option<String>,
}

/// The Performance section as plain data. Collapsed, it is the heading alone: no rows, no caption,
/// because nothing is being sampled and anything shown would be stale.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PerformanceModel {
    pub(crate) expanded: bool,
    /// `1 job` or `N jobs` while long work runs.
    pub(crate) caption: Option<String>,
    /// Memory, CPU and GPU, in that order, while expanded.
    pub(crate) metrics: Vec<MetricRow>,
    pub(crate) jobs: Vec<JobRow>,
    /// The one job line shown has no detail line, so the section keeps that line's height empty
    /// under it. A job starting, finishing or ending in silence then changes nothing below the
    /// heading but the rows' text, and the section's height changes only when two long jobs
    /// overlap, which is the design's rule; without it, the pinned heading would jump by a line
    /// every time the quiet row gave way to a job with a file name or a phase.
    pub(crate) reserve_detail: bool,
    /// `+N more` when more long jobs run than get a row.
    pub(crate) more: Option<String>,
    /// The history's version: the sparklines' cache identity, which moves once per sample.
    pub(crate) version: u64,
}

impl PerformanceModel {
    /// Re-derive the section when its inputs changed: the expanded flag or a sample. Every other
    /// message — most of them, a drag sends dozens a second — leaves the model and its sparklines'
    /// version exactly as they were.
    pub(crate) fn refresh(&mut self, inputs: &Inputs<'_>) {
        let expanded = inputs.performance_expanded;
        let history = inputs.performance;
        if self.expanded == expanded && self.version == history.version() {
            return;
        }
        *self = derive(expanded, history);
    }
}

/// The section for this history, expanded or collapsed.
pub(crate) fn derive(expanded: bool, history: &PerformanceHistory) -> PerformanceModel {
    if !expanded {
        return PerformanceModel {
            version: history.version(),
            ..PerformanceModel::default()
        };
    }
    let samples: Vec<&ResourceSample> = history.samples().iter().collect();
    let jobs = jobs(history.activity());
    PerformanceModel {
        expanded,
        caption: jobs.caption,
        metrics: vec![memory_row(&samples), cpu_row(&samples), gpu_row(&samples)],
        reserve_detail: matches!(jobs.rows.as_slice(), [only] if only.detail.is_none()),
        jobs: jobs.rows,
        more: jobs.more,
        version: history.version(),
    }
}

/// Percent of one unit of a cumulative time counter over each pair of consecutive samples:
/// `100 × Δcounter / Δmonotonic_ns`, as Activity Monitor counts CPU (14 cores can reach 1400%).
///
/// A pair whose clock did not advance says nothing about a rate and is skipped, as is a pair in
/// which either sample lacks the counter. A counter that went backwards — which a cumulative counter
/// should never do — reads as no work rather than negative work.
pub(crate) fn rates(
    samples: &[&ResourceSample],
    counter: impl Fn(&ResourceSample) -> Option<u64>,
) -> Vec<f64> {
    samples
        .windows(2)
        .filter_map(|pair| {
            let (earlier, later) = (pair[0], pair[1]);
            let elapsed = later
                .monotonic_ns
                .checked_sub(earlier.monotonic_ns)
                .filter(|elapsed| *elapsed > 0)?;
            let spent = counter(later)?.saturating_sub(counter(earlier)?);
            Some(100.0 * spent as f64 / elapsed as f64)
        })
        .collect()
}

/// A byte count in binary units with Activity Monitor's labels, as the figure and its unit: whole
/// megabytes below one gibibyte (`812 MB`), two decimals below ten (`1.42 GB`) and one from there
/// (`12.4 GB`). The unit is chosen from the rounded figure, so a count just under a boundary reads
/// `1.00 GB` rather than `1024 MB`. Integer arithmetic rounds half up, so the same count always
/// reads the same way.
pub(crate) fn format_bytes(bytes: u64) -> (String, &'static str) {
    let bytes = u128::from(bytes);
    let megabytes = (bytes + MIB / 2) / MIB;
    if megabytes < 1024 {
        return (megabytes.to_string(), "MB");
    }
    let hundredths = (bytes * 100 + GIB / 2) / GIB;
    if hundredths < 1000 {
        return (
            format!("{}.{:02}", hundredths / 100, hundredths % 100),
            "GB",
        );
    }
    let tenths = (bytes * 10 + GIB / 2) / GIB;
    (format!("{}.{}", tenths / 10, tenths % 10), "GB")
}

/// A byte count as one phrase, for a tooltip: `980 MB`.
fn bytes_phrase(bytes: u64) -> String {
    let (value, unit) = format_bytes(bytes);
    format!("{value} {unit}")
}

/// A percentage without its sign: one decimal below ten (`3.2`), whole numbers from there (`38`,
/// `420`), chosen from the rounded figure so 9.96 reads `10`, not `10.0`. A negative rate reads
/// zero and a non-finite one a dash.
pub(crate) fn format_percent(percent: f64) -> String {
    if !percent.is_finite() {
        return DASH.to_owned();
    }
    let percent = percent.max(0.0);
    let tenths = (percent * 10.0).round() as u64;
    if tenths < 100 {
        format!("{}.{}", tenths / 10, tenths % 10)
    } else {
        format!("{}", percent.round() as u64)
    }
}

/// How long something ran: tenths of a second below ten seconds (`0.8 s`), whole seconds below a
/// minute (`12 s`), then minutes and seconds (`1 min 4 s`). Rounded half up at each step, and the
/// step is chosen from the rounded figure, so 9.96 s reads `10 s`.
pub(crate) fn format_elapsed(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    if tenths < 100 {
        return format!("{}.{} s", tenths / 10, tenths % 10);
    }
    let seconds = (ms + 500) / 1000;
    if seconds < 60 {
        return format!("{seconds} s");
    }
    format!("{} min {} s", seconds / 60, seconds % 60)
}

/// The memory scale: 110% of the window's largest sample, and never below 64 MiB.
pub(crate) fn memory_scale(window: &[u64]) -> f64 {
    let peak = window.iter().copied().max().unwrap_or(0);
    (peak as f64 * MEMORY_HEADROOM).max(MEMORY_SCALE_FLOOR)
}

/// The CPU scale: one core, or the window's peak when that is higher, so a burst across several
/// cores reaches the top without clipping and idle stays low against one core.
pub(crate) fn cpu_scale(rates: &[f64]) -> f64 {
    rates.iter().copied().fold(100.0, f64::max)
}

/// Each value divided by the scale, clamped into `0.0..=1.0`. The scale rules never return less
/// than a positive floor, so there is no division by zero to guard.
pub(crate) fn normalised(values: &[f64], scale: f64) -> Vec<f32> {
    values
        .iter()
        .map(|value| (value / scale).clamp(0.0, 1.0) as f32)
        .collect()
}

/// The newest [`WINDOW`] values: what the sparkline shows, and so what its scale is taken over.
fn newest<T: Copy>(values: &[T]) -> &[T] {
    &values[values.len().saturating_sub(WINDOW)..]
}

/// A row with no sample yet: the moment between expanding and the first read's answer.
fn waiting(label: &'static str) -> MetricRow {
    MetricRow {
        label,
        value: DASH.to_owned(),
        unit: "",
        available: true,
        series: Vec::new(),
        tooltip: String::new(),
    }
}

/// A counter the platform cannot give: a dash, the baseline alone, and the platform's reason.
fn unavailable(label: &'static str, reason: Option<&String>) -> MetricRow {
    MetricRow {
        label,
        value: DASH.to_owned(),
        unit: "",
        available: false,
        series: Vec::new(),
        tooltip: reason
            .cloned()
            .unwrap_or_else(|| format!("{label} is not reported on this platform")),
    }
}

/// What a memory kind measures, in words: Activity Monitor's own column on macOS.
pub(crate) fn memory_kind_words(kind: &str) -> String {
    match kind {
        "footprint" => "Memory footprint, as Activity Monitor's Memory column".to_owned(),
        "resident" => "Resident memory".to_owned(),
        "private" => "Private bytes".to_owned(),
        other => format!("Memory ({other})"),
    }
}

fn memory_row(samples: &[&ResourceSample]) -> MetricRow {
    const LABEL: &str = "Memory";
    let Some(latest) = samples.last() else {
        return waiting(LABEL);
    };
    let memory = &latest.memory;
    let Some(bytes) = memory.bytes else {
        return unavailable(LABEL, memory.unavailable.get("bytes"));
    };
    let window: Vec<u64> = samples
        .iter()
        .filter_map(|sample| sample.memory.bytes)
        .collect();
    let window = newest(&window);
    let scale = memory_scale(window);
    let values: Vec<f64> = window.iter().map(|bytes| *bytes as f64).collect();
    let mut tooltip = vec![memory_kind_words(&memory.kind)];
    if let Some(peak) = memory.peak_bytes {
        tooltip.push(format!("peak {} since launch", bytes_phrase(peak)));
    }
    if let Some(resident) = memory.resident_bytes {
        tooltip.push(format!("resident {}", bytes_phrase(resident)));
    }
    if latest.gpu.unified_memory == Some(true)
        && let Some(allocated) = latest.gpu.allocated_bytes
    {
        tooltip.push(format!(
            "includes {} of GPU allocations",
            bytes_phrase(allocated)
        ));
    }
    let (value, unit) = format_bytes(bytes);
    MetricRow {
        label: LABEL,
        value,
        unit,
        available: true,
        series: normalised(&values, scale),
        tooltip: tooltip.join(" \u{b7} "),
    }
}

/// How long the rates shown span, for a tooltip: `the last minute` once the window is full, and
/// the span of the samples behind them until then.
fn window_phrase(samples: &[&ResourceSample], rates: usize) -> String {
    if rates >= WINDOW {
        return "the last minute".to_owned();
    }
    let span = match (samples.first(), samples.last()) {
        (Some(first), Some(last)) => last.monotonic_ns.saturating_sub(first.monotonic_ns),
        _ => 0,
    };
    format!("the last {}", format_elapsed(span / 1_000_000))
}

/// The samples the newest [`WINDOW`] rates were computed from: one more than the rates.
fn rate_samples<'a, 'b>(samples: &'a [&'b ResourceSample]) -> &'a [&'b ResourceSample] {
    &samples[samples.len().saturating_sub(MAX_SAMPLES)..]
}

fn cpu_row(samples: &[&ResourceSample]) -> MetricRow {
    const LABEL: &str = "CPU";
    let Some(latest) = samples.last() else {
        return waiting(LABEL);
    };
    if latest.cpu.time_ns.is_none() {
        return unavailable(LABEL, latest.cpu.unavailable.get("time_ns"));
    }
    let samples = rate_samples(samples);
    let rates = rates(samples, |sample| sample.cpu.time_ns);
    let rates = newest(&rates);
    let mut tooltip = vec!["Percent of one core".to_owned()];
    match latest.cpu.logical_cpus {
        0 => {}
        1 => tooltip.push("1 core: 100%".to_owned()),
        cores => tooltip.push(format!("{cores} cores: {}%", u64::from(cores) * 100)),
    }
    let Some(now) = rates.last() else {
        return MetricRow {
            tooltip: tooltip.join(" \u{b7} "),
            ..waiting(LABEL)
        };
    };
    let peak = rates.iter().copied().fold(0.0, f64::max);
    tooltip.push(format!(
        "peak {}% in {}",
        format_percent(peak),
        window_phrase(samples, rates.len())
    ));
    MetricRow {
        label: LABEL,
        value: format_percent(*now),
        unit: "%",
        available: true,
        series: normalised(rates, cpu_scale(rates)),
        tooltip: tooltip.join(" \u{b7} "),
    }
}

fn gpu_row(samples: &[&ResourceSample]) -> MetricRow {
    const LABEL: &str = "GPU";
    let Some(latest) = samples.last() else {
        return waiting(LABEL);
    };
    let gpu = &latest.gpu;
    if gpu.time_ns.is_none() {
        return unavailable(LABEL, gpu.unavailable.get("time_ns"));
    }
    let samples = rate_samples(samples);
    let rates = rates(samples, |sample| sample.gpu.time_ns);
    let rates = newest(&rates);
    let mut tooltip = vec!["Percent of GPU time".to_owned()];
    if let Some(peak) = rates.iter().copied().reduce(f64::max) {
        tooltip.push(format!(
            "peak {}% in {}",
            format_percent(peak),
            window_phrase(samples, rates.len())
        ));
    }
    match (gpu.allocated_bytes, gpu.unavailable.get("allocated_bytes")) {
        (Some(allocated), _) => tooltip.push(format!("{} allocated", bytes_phrase(allocated))),
        (None, Some(reason)) => tooltip.push(format!("allocations: {reason}")),
        (None, None) => {}
    }
    let tooltip = tooltip.join(" \u{b7} ");
    let Some(now) = rates.last() else {
        return MetricRow {
            tooltip,
            ..waiting(LABEL)
        };
    };
    MetricRow {
        label: LABEL,
        value: format_percent(*now),
        unit: "%",
        available: true,
        series: normalised(rates, GPU_SCALE),
        tooltip,
    }
}

/// What the jobs part of the section shows for one board snapshot.
///
/// Every active entry that has run for at least [`LONG_JOB_MS`], in the board's order (oldest
/// first), at most [`MAX_JOB_ROWS`] and then a `+N more` caption, with the heading counting all of
/// them. When none has, the newest recent entry that ran at least that long and ended within
/// [`RECENT_JOB_MS`], dimmed, with its duration and how it ended. Otherwise one dimmed `No
/// background work` row, so there is always one job line and the section's height changes only
/// when two long jobs overlap. Before the first snapshot there is nothing running to show either.
pub(crate) fn jobs(activity: Option<&ActivityList>) -> Jobs {
    let quiet = || Jobs {
        rows: vec![JobRow {
            label: "No background work".to_owned(),
            ..JobRow::default()
        }],
        more: None,
        caption: None,
    };
    let Some(activity) = activity else {
        return quiet();
    };
    let long: Vec<&ActiveJob> = activity
        .active
        .iter()
        .filter(|job| job.elapsed_ms >= LONG_JOB_MS)
        .collect();
    if !long.is_empty() {
        let hidden = long.len().saturating_sub(MAX_JOB_ROWS);
        return Jobs {
            rows: long
                .iter()
                .take(MAX_JOB_ROWS)
                .map(|job| running(job))
                .collect(),
            more: (hidden > 0).then(|| format!("+{hidden} more")),
            caption: Some(match long.len() {
                1 => "1 job".to_owned(),
                count => format!("{count} jobs"),
            }),
        };
    }
    activity
        .recent
        .iter()
        .find(|job| job.duration_ms >= LONG_JOB_MS && job.ended_ms_ago <= RECENT_JOB_MS)
        .map(|job| Jobs {
            rows: vec![finished(job)],
            more: None,
            caption: None,
        })
        .unwrap_or_else(quiet)
}

/// A running job: its elapsed time, and its detail and phase joined on the line under it.
fn running(job: &ActiveJob) -> JobRow {
    let parts: Vec<String> = job
        .detail
        .iter()
        .cloned()
        .chain(job.phase.iter().map(|phase| format!("{phase} phase")))
        .collect();
    JobRow {
        label: job.label.clone(),
        trailing: format_elapsed(job.elapsed_ms),
        detail: (!parts.is_empty()).then(|| parts.join(" \u{b7} ")),
        progress: job
            .progress
            .filter(|progress| progress.total > 0)
            .map(|progress| (progress.done as f64 / progress.total as f64) as f32),
        running: true,
    }
}

/// A finished job: its duration, and how and when it ended. The time is whole seconds and at least
/// one, because "0 s ago" reads as a mistake.
fn finished(job: &RecentJob) -> JobRow {
    let how = match job.outcome.as_str() {
        "cancelled" => "Cancelled",
        "failed" => "Failed",
        _ => "Finished",
    };
    let ago = job.ended_ms_ago.saturating_add(500) / 1000;
    JobRow {
        label: job.label.clone(),
        trailing: format_elapsed(job.duration_ms),
        detail: Some(format!("{how} {} s ago", ago.max(1))),
        progress: None,
        running: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MS: u64 = 1_000_000;

    /// A sample at `at_ms` on the monotonic clock with the given counters.
    fn sample(at_ms: u64, cpu_ms: u64, gpu_ms: u64, bytes: u64) -> ResourceSample {
        ResourceSample {
            monotonic_ns: at_ms * MS,
            cpu: CpuCounters {
                time_ns: Some(cpu_ms * MS),
                logical_cpus: 14,
                unavailable: BTreeMap::new(),
            },
            memory: MemoryCounters {
                kind: "footprint".into(),
                bytes: Some(bytes),
                peak_bytes: Some(2_011_000_000),
                resident_bytes: Some(980 * (1 << 20)),
                unavailable: BTreeMap::new(),
            },
            gpu: GpuCounters {
                time_ns: Some(gpu_ms * MS),
                allocated_bytes: Some(312 * (1 << 20)),
                unified_memory: Some(true),
                unavailable: BTreeMap::new(),
            },
        }
    }

    fn history(samples: impl IntoIterator<Item = ResourceSample>) -> PerformanceHistory {
        let mut history = PerformanceHistory::default();
        for sample in samples {
            history.push(sample, ActivityList::default());
        }
        history
    }

    fn close(found: &[f32], expected: &[f32]) -> bool {
        found.len() == expected.len()
            && found
                .iter()
                .zip(expected)
                .all(|(found, expected)| (found - expected).abs() < 1e-6)
    }

    fn row<'a>(model: &'a PerformanceModel, label: &str) -> &'a MetricRow {
        model
            .metrics
            .iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("no {label} row"))
    }

    #[test]
    fn bytes_read_in_activity_monitors_units_at_every_boundary() {
        const MI: u64 = 1 << 20;
        const GI: u64 = 1 << 30;
        let text = |bytes| {
            let (value, unit) = format_bytes(bytes);
            format!("{value} {unit}")
        };
        assert_eq!(text(0), "0 MB");
        assert_eq!(text(812 * MI), "812 MB");
        assert_eq!(
            text(812 * MI + MI / 2),
            "813 MB",
            "half a megabyte rounds up"
        );
        assert_eq!(text(1023 * MI), "1023 MB");
        assert_eq!(
            text(GI - 1),
            "1.00 GB",
            "just under a gibibyte rounds to 1024 MB, so it reads in gigabytes"
        );
        assert_eq!(text(GI), "1.00 GB");
        assert_eq!(text(GI * 142 / 100), "1.42 GB");
        assert_eq!(
            text(10 * GI - GI / 400),
            "10.0 GB",
            "9.9975 GiB rounds to 10.00"
        );
        assert_eq!(text(10 * GI - GI / 100), "9.99 GB");
        assert_eq!(text(10 * GI), "10.0 GB");
        assert_eq!(text(GI * 124 / 10), "12.4 GB");
        assert_eq!(format_bytes(812 * MI), ("812".to_owned(), "MB"));
        assert_eq!(format_bytes(u64::MAX).1, "GB", "no overflow at the top");
    }

    #[test]
    fn percentages_take_one_decimal_below_ten_and_whole_numbers_above() {
        assert_eq!(format_percent(0.0), "0.0");
        assert_eq!(format_percent(3.24), "3.2");
        assert_eq!(format_percent(9.94), "9.9");
        assert_eq!(format_percent(9.96), "10", "rounds to ten, so whole");
        assert_eq!(format_percent(10.0), "10");
        assert_eq!(format_percent(38.4), "38");
        assert_eq!(format_percent(419.6), "420");
        assert_eq!(format_percent(-2.0), "0.0");
        assert_eq!(format_percent(f64::NAN), DASH);
        assert_eq!(format_percent(f64::INFINITY), DASH);
    }

    #[test]
    fn elapsed_times_read_in_tenths_seconds_then_minutes() {
        assert_eq!(format_elapsed(0), "0.0 s");
        assert_eq!(format_elapsed(760), "0.8 s");
        assert_eq!(format_elapsed(1_204), "1.2 s");
        assert_eq!(format_elapsed(9_949), "9.9 s");
        assert_eq!(
            format_elapsed(9_950),
            "10 s",
            "rounds to ten seconds, so whole"
        );
        assert_eq!(format_elapsed(12_400), "12 s");
        assert_eq!(format_elapsed(59_499), "59 s");
        assert_eq!(format_elapsed(59_500), "1 min 0 s");
        assert_eq!(format_elapsed(64_000), "1 min 4 s");
        assert_eq!(format_elapsed(3_725_000), "62 min 5 s");
    }

    #[test]
    fn the_scales_follow_the_design_rules() {
        const MI: u64 = 1 << 20;
        assert_eq!(memory_scale(&[]), (64 * MI) as f64);
        assert_eq!(
            memory_scale(&[10 * MI, 20 * MI]),
            (64 * MI) as f64,
            "never below 64 MiB"
        );
        assert_eq!(
            memory_scale(&[500 * MI, 1000 * MI]),
            1000.0 * MI as f64 * 1.1
        );
        assert_eq!(cpu_scale(&[]), 100.0);
        assert_eq!(cpu_scale(&[3.0, 40.0]), 100.0, "at least one core");
        assert_eq!(cpu_scale(&[3.0, 410.0, 40.0]), 410.0, "the window's peak");
        assert_eq!(
            normalised(&[-5.0, 0.0, 50.0, 100.0, 150.0], 100.0),
            vec![0.0, 0.0, 0.5, 1.0, 1.0]
        );
    }

    /// A rate is the counter's change over the clock's, in percent; a pair whose clock did not
    /// advance is skipped, and a counter that went backwards reads as no work.
    #[test]
    fn rates_come_from_consecutive_pairs_and_skip_or_clamp_the_degenerate_ones() {
        let samples = [
            sample(0, 0, 0, 0),
            sample(1_000, 380, 120, 0),
            sample(2_000, 4_580, 120, 0),
            sample(2_000, 9_000, 500, 0), // no time passed: skipped
            sample(2_500, 8_000, 510, 0), // the counter went backwards: zero
            sample(3_000, 8_016, 520, 0),
        ];
        let refs: Vec<&ResourceSample> = samples.iter().collect();
        let cpu = rates(&refs, |sample| sample.cpu.time_ns);
        assert_eq!(cpu.len(), samples.len() - 2, "one pair skipped");
        assert!((cpu[0] - 38.0).abs() < 1e-9);
        assert!((cpu[1] - 420.0).abs() < 1e-9, "four cores' worth");
        assert_eq!(cpu[2], 0.0, "a backwards counter is no work");
        assert!((cpu[3] - 3.2).abs() < 1e-9);
        let gpu = rates(&refs, |sample| sample.gpu.time_ns);
        assert!((gpu[0] - 12.0).abs() < 1e-9);
        assert_eq!(gpu[1], 0.0);
        // A clock that went backwards is no pair at all.
        let backwards = [sample(2_000, 0, 0, 0), sample(1_000, 100, 0, 0)];
        let refs: Vec<&ResourceSample> = backwards.iter().collect();
        assert!(rates(&refs, |sample| sample.cpu.time_ns).is_empty());
        // A pair missing the counter in either sample is skipped.
        let mut missing = sample(1_000, 0, 0, 0);
        missing.cpu.time_ns = None;
        let pair = [sample(0, 0, 0, 0), missing];
        let refs: Vec<&ResourceSample> = pair.iter().collect();
        assert!(rates(&refs, |sample| sample.cpu.time_ns).is_empty());
    }

    /// The first sample shows memory and a dash for both rates; the second gives each rate one
    /// point; the history keeps one sample more than the window, so every line fills it.
    #[test]
    fn the_rates_need_two_samples_and_every_line_fills_the_window() {
        let one = derive(true, &history([sample(0, 0, 0, 812 << 20)]));
        let memory = row(&one, "Memory");
        assert_eq!((memory.value.as_str(), memory.unit), ("812", "MB"));
        assert_eq!(memory.series.len(), 1);
        for label in ["CPU", "GPU"] {
            let rate = row(&one, label);
            assert_eq!((rate.value.as_str(), rate.unit), (DASH, ""), "{label}");
            assert!(rate.series.is_empty() && rate.available, "{label}");
        }
        assert!(row(&one, "CPU").tooltip.contains("14 cores: 1400%"));
        assert!(!row(&one, "CPU").tooltip.contains("peak"));

        let two = derive(
            true,
            &history([
                sample(0, 0, 0, 812 << 20),
                sample(1_000, 32, 120, 812 << 20),
            ]),
        );
        assert_eq!(row(&two, "CPU").value, "3.2");
        assert_eq!(row(&two, "CPU").unit, "%");
        assert_eq!(row(&two, "CPU").series.len(), 1);
        assert_eq!(row(&two, "GPU").value, "12");
        assert!(close(&row(&two, "GPU").series, &[0.12]));

        let full = history((0..100).map(|index| sample(index * 1_000, index * 10, index, 1 << 30)));
        assert_eq!(full.len(), MAX_SAMPLES, "bounded");
        let model = derive(true, &full);
        for label in ["Memory", "CPU", "GPU"] {
            assert_eq!(row(&model, label).series.len(), WINDOW, "{label}");
        }
        assert!(row(&model, "CPU").tooltip.ends_with("in the last minute"));
    }

    #[test]
    fn each_row_is_normalised_against_its_own_scale() {
        const MI: u64 = 1 << 20;
        let model = derive(
            true,
            &history([
                sample(0, 0, 0, 500 * MI),
                sample(1_000, 500, 50, 1000 * MI),
                sample(2_000, 4_600, 60, 800 * MI),
            ]),
        );
        let memory = row(&model, "Memory");
        assert!((memory.series[1] - 1.0 / 1.1).abs() < 1e-6, "{memory:?}");
        assert!((memory.series[2] - 0.8 / 1.1).abs() < 1e-6);
        let cpu = row(&model, "CPU");
        assert!(
            close(&cpu.series, &[50.0 / 410.0, 1.0]),
            "against the 410% peak: {cpu:?}"
        );
        assert_eq!(cpu.value, "410");
        assert!(
            cpu.tooltip.contains("peak 410% in the last 2.0 s"),
            "{cpu:?}"
        );
        let gpu = row(&model, "GPU");
        assert!(close(&gpu.series, &[0.05, 0.01]), "against 100%: {gpu:?}");
    }

    #[test]
    fn tooltips_state_what_each_figure_measures() {
        let model = derive(
            true,
            &history([sample(0, 0, 0, 1 << 30), sample(1_000, 10, 5, 1 << 30)]),
        );
        assert_eq!(
            row(&model, "Memory").tooltip,
            "Memory footprint, as Activity Monitor's Memory column \u{b7} peak 1.87 GB since launch \
             \u{b7} resident 980 MB \u{b7} includes 312 MB of GPU allocations"
        );
        assert_eq!(
            row(&model, "CPU").tooltip,
            "Percent of one core \u{b7} 14 cores: 1400% \u{b7} peak 1.0% in the last 1.0 s"
        );
        assert_eq!(
            row(&model, "GPU").tooltip,
            "Percent of GPU time \u{b7} peak 0.5% in the last 1.0 s \u{b7} 312 MB allocated"
        );
        // Resident memory on another platform, with no unified GPU allocations to mention.
        let mut linux = sample(0, 0, 0, 1 << 30);
        linux.memory.kind = "resident".into();
        linux.memory.peak_bytes = None;
        linux.gpu.unified_memory = None;
        let model = derive(true, &history([linux]));
        assert_eq!(
            row(&model, "Memory").tooltip,
            "Resident memory \u{b7} resident 980 MB"
        );
        assert_eq!(memory_kind_words("private"), "Private bytes");
    }

    /// A counter the platform cannot give reads a dash with no unit, draws the baseline alone and
    /// says why, while the rows beside it are unaffected.
    #[test]
    fn an_unavailable_counter_reads_a_dash_and_names_its_reason() {
        let linux = |at: u64, cpu: u64| {
            let mut sample = sample(at, cpu, 0, 700 << 20);
            sample.gpu = GpuCounters {
                time_ns: None,
                allocated_bytes: None,
                unified_memory: None,
                unavailable: BTreeMap::from([
                    (
                        "time_ns".to_owned(),
                        "GPU time is not reported on Linux yet".to_owned(),
                    ),
                    (
                        "allocated_bytes".to_owned(),
                        "GPU allocations are not reported on Linux yet".to_owned(),
                    ),
                ]),
            };
            sample
        };
        let model = derive(true, &history([linux(0, 0), linux(1_000, 50)]));
        let gpu = row(&model, "GPU");
        assert_eq!(
            gpu,
            &MetricRow {
                label: "GPU",
                value: DASH.to_owned(),
                unit: "",
                available: false,
                series: Vec::new(),
                tooltip: "GPU time is not reported on Linux yet".to_owned(),
            }
        );
        assert_eq!(row(&model, "CPU").value, "5.0");
        let mut no_memory = sample(0, 0, 0, 0);
        no_memory.memory.bytes = None;
        no_memory
            .memory
            .unavailable
            .insert("bytes".into(), "no footprint here".into());
        let model = derive(true, &history([no_memory]));
        let memory = row(&model, "Memory");
        assert!(!memory.available && memory.series.is_empty());
        assert_eq!((memory.value.as_str(), memory.unit), (DASH, ""));
        assert_eq!(memory.tooltip, "no footprint here");
    }

    /// The core's own JSON reads into these types, unavailable keys included, and an optional key
    /// the section does not read is ignored rather than refused.
    #[test]
    fn the_api_answers_parse_as_documented() {
        let resources: ResourceSample = serde_json::from_value(json!({
            "monotonic_ns": 81_234_567_000_u64,
            "cpu": {"time_ns": 5_231_200_000_u64, "logical_cpus": 14},
            "memory": {"kind": "resident", "bytes": 980, "resident_bytes": 980,
                "unavailable": {"peak_bytes": "no peak here"}},
            "gpu": {"unavailable": {"time_ns": "GPU time is not reported on Linux yet"}},
            "budgets": {"spatial": {"target_bytes": 1, "in_use_bytes": 0, "peak_bytes": 0}}
        }))
        .unwrap();
        assert_eq!(resources.cpu.time_ns, Some(5_231_200_000));
        assert_eq!(resources.memory.peak_bytes, None);
        assert_eq!(resources.memory.unavailable["peak_bytes"], "no peak here");
        assert_eq!(resources.gpu.time_ns, None);
        let activity: ActivityList = serde_json::from_value(json!({
            "sequence": 812,
            "active": [{"id": 41, "kind": "source.develop", "label": "Developing RAW",
                "detail": "DSC_0412.NEF", "asset_id": "a", "job_id": "source-job-7",
                "elapsed_ms": 1204}],
            "recent": [{"id": 40, "kind": "preview.render", "label": "Rendering preview",
                "phase": "exact", "outcome": "completed", "duration_ms": 1610,
                "ended_ms_ago": 4020}],
            "untracked": 0
        }))
        .unwrap();
        assert_eq!(activity.active[0].detail.as_deref(), Some("DSC_0412.NEF"));
        assert_eq!(activity.recent[0].duration_ms, 1610);
        assert_eq!(activity.recent[0].outcome, "completed");
    }

    fn active(label: &str, elapsed_ms: u64) -> ActiveJob {
        ActiveJob {
            label: label.into(),
            elapsed_ms,
            ..ActiveJob::default()
        }
    }

    fn recent(label: &str, outcome: &str, duration_ms: u64, ended_ms_ago: u64) -> RecentJob {
        RecentJob {
            label: label.into(),
            outcome: outcome.into(),
            duration_ms,
            ended_ms_ago,
        }
    }

    /// Long active work gets a row each, oldest first, with its detail and phase joined under it;
    /// short work gets none, and the heading counts the long ones.
    #[test]
    fn long_running_work_is_listed_oldest_first_with_its_detail_and_phase() {
        let mut develop = active("Developing RAW", 1_204);
        develop.detail = Some("DSC_0412.NEF".into());
        let mut preview = active("Rendering preview", 700);
        preview.phase = Some("exact".into());
        let mut both = active("Preparing original", 500);
        both.detail = Some("a.jpg".into());
        both.phase = Some("decode".into());
        let short = active("Measuring histogram", 499);
        let jobs = jobs(Some(&ActivityList {
            active: vec![develop, preview, both, short],
            recent: vec![recent("Developing RAW", "completed", 1_600, 100)],
        }));
        assert_eq!(jobs.caption.as_deref(), Some("3 jobs"));
        assert_eq!(jobs.more, None);
        assert_eq!(
            jobs.rows,
            vec![
                JobRow {
                    label: "Developing RAW".into(),
                    trailing: "1.2 s".into(),
                    detail: Some("DSC_0412.NEF".into()),
                    progress: None,
                    running: true,
                },
                JobRow {
                    label: "Rendering preview".into(),
                    trailing: "0.7 s".into(),
                    detail: Some("exact phase".into()),
                    progress: None,
                    running: true,
                },
                JobRow {
                    label: "Preparing original".into(),
                    trailing: "0.5 s".into(),
                    detail: Some("a.jpg \u{b7} decode phase".into()),
                    progress: None,
                    running: true,
                },
            ],
            "the finished job is not shown while long work runs"
        );
    }

    #[test]
    fn one_long_job_is_one_job_and_its_progress_is_a_fraction() {
        let mut export = active("Exporting", 64_000);
        export.progress = Some(JobProgress { done: 3, total: 8 });
        let mut unknown = active("Exporting", 400);
        unknown.progress = Some(JobProgress { done: 3, total: 0 });
        let listed = jobs(Some(&ActivityList {
            active: vec![export, unknown],
            ..ActivityList::default()
        }));
        assert_eq!(listed.caption.as_deref(), Some("1 job"));
        assert_eq!(listed.rows.len(), 1);
        assert_eq!(listed.rows[0].trailing, "1 min 4 s");
        assert_eq!(listed.rows[0].progress, Some(0.375));
        assert_eq!(listed.rows[0].detail, None, "neither a detail nor a phase");
        let zero_total = running(&ActiveJob {
            progress: Some(JobProgress { done: 3, total: 0 }),
            ..active("x", 900)
        });
        assert_eq!(zero_total.progress, None, "no truthful total, no bar");
    }

    #[test]
    fn more_than_four_long_jobs_end_in_a_caption() {
        let listed = jobs(Some(&ActivityList {
            active: (1..=6)
                .map(|index| active("Rendering preview", 2_000 - index * 100))
                .collect(),
            ..ActivityList::default()
        }));
        assert_eq!(listed.rows.len(), MAX_JOB_ROWS);
        assert_eq!(
            listed.rows[0].trailing, "1.9 s",
            "oldest first, as the board lists them"
        );
        assert_eq!(listed.more.as_deref(), Some("+2 more"));
        assert_eq!(listed.caption.as_deref(), Some("6 jobs"));
    }

    /// With nothing long running, the newest recent long job that ended within ten seconds is shown
    /// dimmed, with its duration and how it ended.
    #[test]
    fn a_recent_long_job_is_shown_finished_when_nothing_long_runs() {
        let listed = jobs(Some(&ActivityList {
            active: vec![active("Rendering preview", 120)],
            recent: vec![
                recent("Rendering preview", "completed", 300, 200), // too short
                recent("Developing RAW", "completed", 1_610, 4_020),
                recent("Preparing original", "completed", 2_000, 6_000), // older
            ],
        }));
        assert_eq!(listed.caption, None, "no long work is running");
        assert_eq!(
            listed.rows,
            vec![JobRow {
                label: "Developing RAW".into(),
                trailing: "1.6 s".into(),
                detail: Some("Finished 4 s ago".into()),
                progress: None,
                running: false,
            }]
        );
        for (outcome, word) in [
            ("cancelled", "Cancelled"),
            ("failed", "Failed"),
            ("completed", "Finished"),
        ] {
            let row = finished(&recent("Rendering preview", outcome, 900, 10_000));
            assert_eq!(
                row.detail.as_deref(),
                Some(format!("{word} 10 s ago").as_str())
            );
        }
        assert_eq!(
            finished(&recent("x", "completed", 900, 120))
                .detail
                .as_deref(),
            Some("Finished 1 s ago"),
            "never 0 s ago"
        );
    }

    #[test]
    fn otherwise_the_section_says_there_is_no_background_work() {
        let quiet = JobRow {
            label: "No background work".into(),
            ..JobRow::default()
        };
        for activity in [
            None,
            Some(ActivityList::default()),
            Some(ActivityList {
                active: vec![active("Rendering preview", 499)],
                recent: vec![
                    recent("Developing RAW", "completed", 1_600, 10_001), // too long ago
                    recent("Rendering preview", "completed", 499, 10),    // too short
                ],
            }),
        ] {
            let listed = jobs(activity.as_ref());
            assert_eq!(listed.rows, vec![quiet.clone()], "{activity:?}");
            assert_eq!((listed.more, listed.caption), (None, None));
        }
        assert!(!quiet.running && quiet.trailing.is_empty() && quiet.detail.is_none());
    }

    /// One job line always keeps room for a detail line under it, so the quiet row, a finished job
    /// and a running one with or without a detail are all the same height.
    #[test]
    fn one_job_line_keeps_room_for_its_detail() {
        let with = |active: Vec<ActiveJob>| {
            let mut history = PerformanceHistory::default();
            history.push(
                sample(0, 0, 0, 1 << 30),
                ActivityList {
                    active,
                    ..ActivityList::default()
                },
            );
            derive(true, &history)
        };
        assert!(with(Vec::new()).reserve_detail, "the quiet row");
        assert!(with(vec![active("Measuring histogram", 900)]).reserve_detail);
        let mut develop = active("Developing RAW", 900);
        develop.detail = Some("DSC_0412.NEF".into());
        assert!(
            !with(vec![develop.clone()]).reserve_detail,
            "its own detail fills it"
        );
        assert!(
            !with(vec![active("Measuring histogram", 900), develop]).reserve_detail,
            "two long jobs set the height themselves"
        );
        assert!(!derive(false, &PerformanceHistory::default()).reserve_detail);
    }

    /// Collapsed, the section is its heading alone, whatever the history holds; expanded, it has
    /// three rows and at least one job line.
    #[test]
    fn collapsed_is_the_heading_alone() {
        let mut history = history([sample(0, 0, 0, 1 << 30)]);
        history.push(
            sample(1_000, 900, 0, 1 << 30),
            ActivityList {
                active: vec![active("Developing RAW", 900)],
                ..ActivityList::default()
            },
        );
        let collapsed = derive(false, &history);
        assert!(!collapsed.expanded);
        assert!(collapsed.metrics.is_empty() && collapsed.jobs.is_empty());
        assert_eq!(collapsed.caption, None, "a stale count would lie");
        let expanded = derive(true, &history);
        assert_eq!(expanded.metrics.len(), 3);
        assert_eq!(expanded.caption.as_deref(), Some("1 job"));
        assert_eq!(expanded.version, history.version());
    }

    /// The history's version moves with every push and clear and with nothing else, and it is
    /// bounded to one sample more than the window.
    #[test]
    fn the_history_is_bounded_and_versioned_by_its_contents() {
        let mut history = PerformanceHistory::default();
        let start = history.version();
        for index in 0..(MAX_SAMPLES as u64 + 5) {
            history.push(sample(index, 0, 0, 0), ActivityList::default());
        }
        assert_eq!(history.len(), MAX_SAMPLES);
        assert_eq!(history.samples().front().unwrap().monotonic_ns, 5 * MS);
        assert_eq!(history.version(), start + MAX_SAMPLES as u64 + 5);
        let before = history.version();
        history.clear();
        assert_eq!(history.len(), 0);
        assert!(history.activity().is_none());
        assert_ne!(history.version(), before);
    }
}
