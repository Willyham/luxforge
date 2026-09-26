//! SplitMix64: a tiny, dependency-free and fully reproducible generator, so every fixed-seed sample
//! and figure the studies and the core's tests draw is the same on any machine and any Rust
//! version. It chooses inputs; it is not part of any reference's arithmetic.

/// The generator's whole state is the tuple field, so `SplitMix64(seed)` starts a sequence.
#[derive(Clone, Debug)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `[lo, hi)` from the top 53 bits.
    pub fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + u * (hi - lo)
    }

    /// A value in `0..bound` by remainder.
    pub fn next_u32(&mut self, bound: u32) -> u32 {
        (self.next_u64() % u64::from(bound)) as u32
    }

    /// A value in `0..bound` by remainder.
    pub fn next_usize(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    pub fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// Two uniforms summed and centred: a bounded, reproducible stand-in for sensor noise, with no
    /// distribution claim beyond "symmetric about zero".
    pub fn next_noise(&mut self) -> f64 {
        self.next_range(-1.0, 1.0) + self.next_range(-1.0, 1.0)
    }
}
