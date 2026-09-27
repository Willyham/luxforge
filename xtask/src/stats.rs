//! The one report shape and one process sampler for every timing tool: [`row`] writes a figure as
//! `{"metric","unit","distribution"}` over the one nearest-rank [`Distribution`] (which lives in
//! `luxforge-testbase`, so the crates' own timing tests compute with it too), [`rows`] and
//! [`distribution`] read a report's rows back, `usage(root, pid)` samples CPU time and RSS, and
//! [`idle_window`] is the one settle-then-idle window (a one second settle, then a thirty second
//! window).
use crate::*;
pub use luxforge_testbase::Distribution;
use std::time::{Duration, Instant};

/// A distribution in the one JSON shape: `{"count","p50","p95","min","max","samples"}`.
fn distribution_value(distribution: &Distribution) -> Value {
    json!({
        "count": distribution.count,
        "p50": distribution.p50,
        "p95": distribution.p95,
        "min": distribution.min,
        "max": distribution.max,
        "samples": distribution.samples,
    })
}

/// One row of the one report shape every timing tool writes into its report's `rows` array:
/// `{"metric","unit","distribution"}`, the distribution `null` when there are no samples, so a
/// metric the run never reached is still listed and never reported as a figure.
pub fn row(metric: &str, unit: &str, samples: impl IntoIterator<Item = f64>) -> Value {
    json!({
        "metric": metric,
        "unit": unit,
        "distribution": Distribution::of(samples).as_ref().map(distribution_value),
    })
}

/// A single observation — a one-shot core step, an idle window's CPU, frames per second over a
/// whole run, a GPU counter — as a one-sample row, or a row with no distribution when there was
/// nothing to observe.
pub fn scalar(metric: &str, unit: &str, value: Option<f64>) -> Value {
    row(metric, unit, value)
}

/// The rows of a report written in the one shape; none for a report that holds none.
pub fn rows(report: &Value) -> &[Value] {
    report["rows"].as_array().map_or(&[], Vec::as_slice)
}

/// The distribution one metric of a report was recorded with, or `None` when the report has no
/// row for it or the row has no samples.
pub fn distribution<'a>(report: &'a Value, metric: &str) -> Option<&'a Value> {
    rows(report)
        .iter()
        .find(|row| row["metric"] == metric)
        .map(|row| &row["distribution"])
        .filter(|distribution| distribution.is_object())
}

/// The one `ps` sample every timing tool takes: CPU seconds and resident set size in MiB.
pub fn usage(root: &Path, pid: u32) -> Result<(f64, f64)> {
    let raw = output(root, "ps", &["-o", "time=,rss=", "-p", &pid.to_string()])?;
    let fields: Vec<_> = raw.split_whitespace().collect();
    ensure(fields.len() == 2, "Missing ps measurements")?;
    let (min, sec) = fields[0].split_once(':').ok_or("Unexpected ps CPU time")?;
    Ok((
        min.parse::<f64>()? * 60.0 + sec.parse::<f64>()?,
        fields[1].parse::<f64>()? / 1024.0,
    ))
}

/// A running child, watched for its own resource usage. Every timing tool's RSS-sampling loop reads
/// through here instead of calling `ps` directly; each loop still owns its own elapsed-time clock,
/// deadline and poll rate, since a gesture script samples every 50 ms and an idle window every
/// 500 ms by design, and those differences are kept rather than forced into one shared rate.
pub struct Watch<'a> {
    root: &'a Path,
    pid: u32,
}

impl<'a> Watch<'a> {
    pub fn new(root: &'a Path, pid: u32) -> Self {
        Self { root, pid }
    }
    pub fn usage(&self) -> Result<(f64, f64)> {
        usage(self.root, self.pid)
    }
}

/// The settle-then-idle window every idle measurement takes: one second of settling after
/// readiness, then a 30 second window sampling usage every 500 ms for the peak RSS, with the CPU
/// time delta across the window reported as a percentage of one core. `measure` and
/// `editor-latency --idle` both call this.
pub struct IdleWindow {
    pub duration_s: f64,
    pub cpu_percent_one_core: f64,
    pub rss_mib_start: f64,
    pub rss_mib_end: f64,
    pub rss_mib_peak: f64,
}

impl IdleWindow {
    /// The window's figures as rows of the one shape, each one observation, named `idle.<figure>`.
    pub fn rows(&self) -> Vec<Value> {
        [
            (
                "cpu_percent_one_core",
                "% of one core",
                self.cpu_percent_one_core,
            ),
            ("rss_mib_peak", "MiB", self.rss_mib_peak),
            ("rss_mib_start", "MiB", self.rss_mib_start),
            ("rss_mib_end", "MiB", self.rss_mib_end),
            ("duration_s", "s", self.duration_s),
        ]
        .into_iter()
        .map(|(figure, unit, value)| scalar(&format!("idle.{figure}"), unit, Some(value)))
        .collect()
    }
}

/// One second of settling on `pid`, then thirty seconds sampling its usage every 500 ms.
pub fn idle_window(root: &Path, pid: u32) -> Result<IdleWindow> {
    std::thread::sleep(Duration::from_secs(1));
    let watch = Watch::new(root, pid);
    let before = watch.usage()?;
    let start = Instant::now();
    let mut peak = before.1;
    while start.elapsed() < Duration::from_secs(30) {
        peak = peak.max(watch.usage()?.1);
        std::thread::sleep(Duration::from_millis(500));
    }
    let after = watch.usage()?;
    let seconds = start.elapsed().as_secs_f64();
    Ok(IdleWindow {
        duration_s: seconds,
        cpu_percent_one_core: (after.0 - before.0) / seconds * 100.0,
        rss_mib_start: before.1,
        rss_mib_end: after.1,
        rss_mib_peak: peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one row shape, over the golden n = 5 vector of `luxforge_testbase::Distribution`: p50
    /// is the 3rd smallest, p95 the 5th, and every sample is kept, sorted.
    #[test]
    fn a_row_holds_the_nearest_rank_distribution() {
        let value = row("input_to_presented_frame", "ms", [5.0, 3.0, 1.0, 4.0, 2.0]);
        assert_eq!(
            value,
            json!({
                "metric": "input_to_presented_frame",
                "unit": "ms",
                "distribution": {
                    "count": 5, "p50": 3.0, "p95": 5.0, "min": 1.0, "max": 5.0,
                    "samples": [1.0, 2.0, 3.0, 4.0, 5.0],
                },
            })
        );
    }

    /// An even n reads nearest-rank, not an interpolated or upper median: the golden n = 20
    /// vector's p50 is the 10th smallest and its p95 the 19th.
    #[test]
    fn a_row_of_twenty_samples_reads_the_golden_ranks() {
        let value = row("frame_gap_ms", "ms", (1..=20).map(f64::from));
        assert_eq!(value["distribution"]["p50"], 10.0);
        assert_eq!(value["distribution"]["p95"], 19.0);
        assert_eq!(value["distribution"]["count"], 20);
    }

    #[test]
    fn no_samples_is_a_row_without_a_distribution() {
        assert_eq!(
            row("gpu_upload", "ms", Vec::new()),
            json!({"metric":"gpu_upload","unit":"ms","distribution":null})
        );
        assert_eq!(
            scalar("presented_fps", "fps", None)["distribution"],
            Value::Null
        );
    }

    #[test]
    fn a_scalar_is_a_one_sample_row() {
        let value = scalar("input_to_first_region_adoption_ms", "ms", Some(42.5));
        assert_eq!(value["distribution"]["count"], 1);
        assert_eq!(value["distribution"]["p50"], 42.5);
        assert_eq!(value["distribution"]["p95"], 42.5);
    }

    #[test]
    fn rows_are_read_back_by_metric() {
        let report = json!({"rows":[row("a", "ms", [1.0, 2.0]), row("b", "ms", Vec::new())]});
        assert_eq!(rows(&report).len(), 2);
        assert_eq!(distribution(&report, "a").unwrap()["p95"], 2.0);
        assert!(distribution(&report, "b").is_none());
        assert!(distribution(&report, "c").is_none());
        assert!(rows(&json!({})).is_empty());
    }

    #[test]
    fn an_idle_window_is_one_row_per_figure() {
        let window = IdleWindow {
            duration_s: 30.2,
            cpu_percent_one_core: 0.8,
            rss_mib_start: 200.0,
            rss_mib_end: 201.0,
            rss_mib_peak: 205.0,
        };
        let report = json!({"rows": window.rows()});
        assert_eq!(rows(&report).len(), 5);
        assert_eq!(rows(&report)[0]["unit"], "% of one core");
        assert_eq!(
            distribution(&report, "idle.cpu_percent_one_core").unwrap()["p50"],
            0.8
        );
    }
}
