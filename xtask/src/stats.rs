//! One distribution type and one process sampler for every timing tool: the nearest-rank
//! percentile and its JSON shape, `usage(root, pid)` for CPU time and RSS, and the one
//! settle-then-idle window (a one second settle, then a thirty second window).
use crate::*;
use std::time::{Duration, Instant};

/// One p50/p95 distribution over millisecond samples, with one nearest-rank definition, serialized
/// the same way by every timing tool: `{"count":N,"p50":X,"p95":Y,"min":A,"max":B,"samples":[...]}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Distribution {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
    pub samples: Vec<f64>,
}

impl Distribution {
    /// Nearest-rank percentile: `sorted[ceil(percent * n / 100) - 1]`. The reported figure is
    /// always one of the samples, so a tail is never interpolated away. `sorted` must already be
    /// sorted ascending; `None` for an empty slice.
    pub fn percentile(sorted: &[f64], percent: usize) -> Option<f64> {
        if sorted.is_empty() {
            return None;
        }
        let rank = (percent * sorted.len()).div_ceil(100).max(1);
        sorted.get(rank - 1).copied()
    }

    /// Sort `samples` and compute count/p50/p95/min/max. `None` for an empty distribution, so a
    /// tool cannot silently report a distribution with no observations.
    pub fn of(mut samples: Vec<f64>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        let p50 = Self::percentile(&samples, 50)?;
        let p95 = Self::percentile(&samples, 95)?;
        Some(Self {
            count: samples.len(),
            p50,
            p95,
            min: samples[0],
            max: *samples.last().expect("checked non-empty above"),
            samples,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "count": self.count,
            "p50": self.p50,
            "p95": self.p95,
            "min": self.min,
            "max": self.max,
            "samples": self.samples,
        })
    }
}

/// The one JSON shape every timing tool now writes for a distribution, or `Value::Null` for an
/// empty one. The convenience every tool's own report-building calls instead of shaping the object
/// itself.
pub fn distribution_json(samples: Vec<f64>) -> Value {
    Distribution::of(samples)
        .map(|d| d.to_json())
        .unwrap_or(Value::Null)
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
/// `editor-latency --idle` used to write this window themselves; both now call this.
pub struct IdleWindow {
    pub duration_s: f64,
    pub cpu_percent_one_core: f64,
    pub rss_mib_start: f64,
    pub rss_mib_end: f64,
    pub rss_mib_peak: f64,
}

impl IdleWindow {
    pub fn to_json(&self) -> Value {
        json!({
            "duration_s": self.duration_s,
            "cpu_percent_one_core": self.cpu_percent_one_core,
            "rss_mib_start": self.rss_mib_start,
            "rss_mib_end": self.rss_mib_end,
            "rss_mib_peak": self.rss_mib_peak,
        })
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

    /// n = 1: the single sample answers every percentile.
    #[test]
    fn golden_percentile_n1() {
        let d = Distribution::of(vec![7.0]).unwrap();
        assert_eq!(d.count, 1);
        assert_eq!((d.p50, d.p95, d.min, d.max), (7.0, 7.0, 7.0, 7.0));
    }

    /// n = 5: p50 rank = ceil(2.5) = 3rd smallest; p95 rank = ceil(4.75) = 5th smallest (the max).
    #[test]
    fn golden_percentile_n5() {
        let d = Distribution::of(vec![5.0, 3.0, 1.0, 4.0, 2.0]).unwrap();
        assert_eq!(d.count, 5);
        assert_eq!(d.p50, 3.0);
        assert_eq!(d.p95, 5.0);
        assert_eq!(d.min, 1.0);
        assert_eq!(d.max, 5.0);
    }

    /// n = 20: p50 rank = ceil(10.0) = 10th smallest; p95 rank = ceil(19.0) = 19th smallest, one
    /// short of the max. This is the vector on which the old `diagnostics::measure` p95
    /// (`sorted[n*95/100]`, no ceiling) silently read the 20th element instead: `20*95/100` divides
    /// exactly, so its unceilinged index landed on the max rather than the 19th smallest.
    #[test]
    fn golden_percentile_n20() {
        let samples: Vec<f64> = (1..=20).map(f64::from).collect();
        let d = Distribution::of(samples).unwrap();
        assert_eq!(d.count, 20);
        assert_eq!(d.p50, 10.0);
        assert_eq!(d.p95, 19.0);
        assert_eq!(d.max, 20.0);
    }

    /// n = 30, the sample count `docs/specs/performance.md` names for a p50/p95 claim: p50 rank =
    /// ceil(15.0) = 15th smallest; p95 rank = ceil(28.5) = 29th smallest.
    #[test]
    fn golden_percentile_n30() {
        let samples: Vec<f64> = (1..=30).map(f64::from).collect();
        let d = Distribution::of(samples).unwrap();
        assert_eq!(d.count, 30);
        assert_eq!(d.p50, 15.0);
        assert_eq!(d.p95, 29.0);
    }

    /// An even n: nearest-rank never interpolates, so p50 is the third of six sorted samples (rank
    /// ceil(3.0) = 3), not the average of the third and fourth that an interpolated median (the old
    /// `diagnostics::measure` statistic) would have reported for this vector (35.0).
    #[test]
    fn golden_percentile_even_n() {
        let d = Distribution::of(vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0]).unwrap();
        assert_eq!(d.count, 6);
        assert_eq!(d.p50, 30.0, "nearest-rank, not the interpolated 35.0");
        assert_eq!(d.p95, 60.0);
    }

    #[test]
    fn empty_distribution_is_none() {
        assert!(Distribution::of(Vec::new()).is_none());
        assert_eq!(distribution_json(Vec::new()), Value::Null);
    }

    #[test]
    fn distribution_json_matches_the_struct() {
        let value = distribution_json(vec![1.0, 2.0, 3.0]);
        assert_eq!(value["count"], 3);
        assert_eq!(value["p50"], 2.0);
        assert_eq!(value["p95"], 3.0);
        assert_eq!(value["min"], 1.0);
        assert_eq!(value["max"], 3.0);
        assert_eq!(value["samples"], json!([1.0, 2.0, 3.0]));
    }
}
