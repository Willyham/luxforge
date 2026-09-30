//! The generator's one source of randomness: SplitMix64, seeded from the run's seed and a stream
//! key, so each event draws from its own stream and changing one event's size never reshuffles
//! another's. It is not for anything but test data.

/// A SplitMix64 stream.
pub struct Random(u64);

impl Random {
    /// The stream `key` of the run seeded with `seed`.
    pub fn stream(seed: u64, key: u64) -> Self {
        let mut mixer = Random(seed ^ key.wrapping_mul(0xa076_1d64_78bd_642f));
        Random(mixer.next_u64())
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform value in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform integer in `[low, high]`.
    pub fn between(&mut self, low: i64, high: i64) -> i64 {
        debug_assert!(low <= high);
        let span = (high - low) as u64 + 1;
        low + (self.next_u64() % span) as i64
    }

    /// A uniform value in `[low, high)`.
    pub fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// One of `items`, uniformly.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.between(0, items.len() as i64 - 1) as usize]
    }

    /// An index into `weights`, in proportion to them.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|&w| u64::from(w)).sum();
        let mut target = self.next_u64() % total.max(1);
        for (index, &weight) in weights.iter().enumerate() {
            if target < u64::from(weight) {
                return index;
            }
            target -= u64::from(weight);
        }
        weights.len() - 1
    }
}
