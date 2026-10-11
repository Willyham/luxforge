//! The fill's display-referred domain and its inverse. A model takes and returns display-referred
//! values, `0..=1`; the repair stage holds linear light, which may pass 1 in a RAW's highlights.
//! The fill encodes linear light `v` to the code `255 · OETF(clamp(v / k, 0, 1))`, the sRGB
//! transfer function over the context's own range `k`, and decodes a code `e` back to
//! `k · EOTF(clamp(e / 255, 0, 1))`. `k` is 1 for a context within white, and otherwise the
//! brightest channel's 99.9th percentile over the context the model sees, so a bright sky is
//! encoded rather than clipped while one hot pixel does not darken everything else. The fill's
//! later stages (grain, seam and detail) work on these codes, in the units their thresholds were
//! measured in. Every candidate records the encoding (`encoding`, `encoding_scale`).
use crate::colour::srgb;

/// The encoding's name in provenance.
pub(crate) const ENCODING: &str = "srgb";
/// The largest context range `k` the encoding takes: six stops past white.
pub(crate) const MAX_SCALE: f64 = 64.0;
/// Histogram bins per stop of the range estimate.
const BINS_PER_STOP: usize = 32;
/// Share of the context's pixels whose brightest channel may lie past `k`.
const PAST_SHARE: f64 = 0.001;

/// The encoding of one fill: its context range `k`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Encoding {
    pub(crate) scale: f64,
}

impl Encoding {
    /// The encoding for a context whose pixels' brightest channels are `maxima`: `k` is the
    /// smallest edge `2^(b/32)` of a fixed log histogram over `1..=MAX_SCALE` past which at most
    /// 0.1% of them lie, 1 when that many already lie within white, and `MAX_SCALE` at most.
    /// `O(pixels)` and 200 counters.
    pub(crate) fn measure(maxima: impl Iterator<Item = f32>) -> Self {
        let stops = MAX_SCALE.log2() as usize;
        let mut bins = vec![0_u64; stops * BINS_PER_STOP];
        let (mut total, mut over) = (0_u64, 0_u64);
        for value in maxima {
            total += 1;
            let value = f64::from(value);
            if value.is_nan() || value <= 1.0 {
                continue;
            }
            if value > MAX_SCALE {
                over += 1;
                continue;
            }
            let bin = (value.log2() * BINS_PER_STOP as f64).ceil() as usize;
            let last = bins.len();
            bins[bin.clamp(1, last) - 1] += 1;
        }
        let allowed = (PAST_SHARE * total as f64).floor() as u64;
        // `past[b]`: values above edge `b`, which is `2^(b / BINS_PER_STOP)`.
        let mut past = over;
        let mut edge = bins.len();
        while edge > 0 && past + bins[edge - 1] <= allowed {
            past += bins[edge - 1];
            edge -= 1;
        }
        let scale = if past > allowed {
            MAX_SCALE
        } else {
            2_f64.powf(edge as f64 / BINS_PER_STOP as f64)
        };
        Self { scale }
    }

    /// Linear light to a code, `0..=255`.
    pub(crate) fn code(self, linear: f32) -> f32 {
        (255.0 * srgb::encode((f64::from(linear) / self.scale).clamp(0.0, 1.0))) as f32
    }

    /// A code back to linear light: the inverse of [`Self::code`] within `0..=k`.
    pub(crate) fn linear(self, code: f32) -> f32 {
        (self.scale * srgb::decode((f64::from(code) / 255.0).clamp(0.0, 1.0))) as f32
    }
}
