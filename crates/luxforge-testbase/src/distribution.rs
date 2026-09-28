//! The one distribution every timing figure in the workspace is computed with: `xtask`'s timing
//! tools and the crates' own ignored timing tests alike.

/// A distribution of samples with one nearest-rank percentile definition.
///
/// The samples are retained, sorted ascending, so a tail is never dropped silently and any other
/// percentile a test prints (a p99, say) is read from the same definition through
/// [`Distribution::percentile`]. The unit is the caller's: `xtask`'s reports are in milliseconds,
/// and a crate's timing test may count nanoseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Distribution {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
    /// Every sample, sorted ascending.
    pub samples: Vec<f64>,
}

/// Nearest-rank percentile: `sorted[ceil(percent * n / 100) - 1]`. The figure is always one of
/// the samples, so a tail is never interpolated away. `sorted` is non-empty and ascending.
fn nearest_rank(sorted: &[f64], percent: usize) -> f64 {
    let rank = (percent * sorted.len())
        .div_ceil(100)
        .clamp(1, sorted.len());
    sorted[rank - 1]
}

impl Distribution {
    /// Sort `samples` and compute count, p50, p95, min and max. `None` for no samples, so nothing
    /// can report a distribution with no observations.
    pub fn of(samples: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut samples: Vec<f64> = samples.into_iter().collect();
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        Some(Self {
            count: samples.len(),
            p50: nearest_rank(&samples, 50),
            p95: nearest_rank(&samples, 95),
            min: samples[0],
            max: samples[samples.len() - 1],
            samples,
        })
    }

    /// The nearest-rank `percent`th percentile of the samples, `percent` in `1..=100`.
    pub fn percentile(&self, percent: usize) -> f64 {
        nearest_rank(&self.samples, percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// n = 1: the single sample answers every percentile.
    #[test]
    fn golden_percentile_n1() {
        let d = Distribution::of([7.0]).unwrap();
        assert_eq!(d.count, 1);
        assert_eq!((d.p50, d.p95, d.min, d.max), (7.0, 7.0, 7.0, 7.0));
        assert_eq!(d.percentile(1), 7.0);
        assert_eq!(d.percentile(100), 7.0);
    }

    /// n = 5: p50 rank = ceil(2.5) = 3rd smallest; p95 rank = ceil(4.75) = 5th smallest (the max).
    #[test]
    fn golden_percentile_n5() {
        let d = Distribution::of([5.0, 3.0, 1.0, 4.0, 2.0]).unwrap();
        assert_eq!(d.count, 5);
        assert_eq!(d.p50, 3.0);
        assert_eq!(d.p95, 5.0);
        assert_eq!(d.min, 1.0);
        assert_eq!(d.max, 5.0);
        assert_eq!(d.samples, [1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    /// n = 20: p50 rank = ceil(10.0) = 10th smallest; p95 rank = ceil(19.0) = 19th smallest, one
    /// short of the max. An unceilinged `sorted[n*95/100]` index reads the 20th here, because
    /// `20*95/100` divides exactly; the `(n-1)*q` index some crates' timing tests used reads the
    /// 19th for p95 but the 10th only by rounding down, and the upper median `sorted[n/2]` reads
    /// the 11th.
    #[test]
    fn golden_percentile_n20() {
        let d = Distribution::of((1..=20).map(f64::from)).unwrap();
        assert_eq!(d.count, 20);
        assert_eq!(d.p50, 10.0);
        assert_eq!(d.p95, 19.0);
        assert_eq!(d.max, 20.0);
    }

    /// n = 30, the sample count `docs/specs/performance.md` names for a p50/p95 claim: p50 rank =
    /// ceil(15.0) = 15th smallest; p95 rank = ceil(28.5) = 29th smallest.
    #[test]
    fn golden_percentile_n30() {
        let d = Distribution::of((1..=30).map(f64::from)).unwrap();
        assert_eq!(d.count, 30);
        assert_eq!(d.p50, 15.0);
        assert_eq!(d.p95, 29.0);
    }

    /// An even n: nearest-rank never interpolates, so p50 is the third of six sorted samples (rank
    /// ceil(3.0) = 3), not the interpolated 35.0 nor the upper median 40.0 (`sorted[n/2]`).
    #[test]
    fn golden_percentile_even_n() {
        let d = Distribution::of([10.0, 20.0, 30.0, 40.0, 50.0, 60.0]).unwrap();
        assert_eq!(d.count, 6);
        assert_eq!(d.p50, 30.0, "nearest-rank, not the interpolated 35.0");
        assert_eq!(d.p95, 60.0);
    }

    /// Any other percentile is read by the same definition: p99 of 1..=1000 is the 990th smallest,
    /// and of 1..=10 it is the max.
    #[test]
    fn golden_other_percentiles() {
        let d = Distribution::of((1..=1000).map(f64::from)).unwrap();
        assert_eq!(d.percentile(99), 990.0);
        assert_eq!(d.percentile(50), d.p50);
        let d = Distribution::of((1..=10).map(f64::from)).unwrap();
        assert_eq!(d.percentile(99), 10.0);
    }

    #[test]
    fn empty_distribution_is_none() {
        assert!(Distribution::of(Vec::new()).is_none());
    }
}
