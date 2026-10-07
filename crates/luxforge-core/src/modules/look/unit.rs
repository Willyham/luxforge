//! The RAW look's one fused pointwise unit (`docs/design/raw-looks.md`, "The look").
//!
//! One unit runs the look's four steps on each pixel, in the frozen reference's order
//! (`crates/luxforge-reference/src/look.rs`, `look_pixel`), because Amount blends the look's output
//! with the unit's own input, which a later unit no longer has:
//!
//! 1. **Tone**: encoded Rec. 709 luminance through the Tone curve's interpolant
//!    ([`Interpolant`] under [`Tails::Look`]), reconstructed over the curve's black level.
//! 2. **Chroma**: Oklab `a` and `b` scaled by the gain, at constant `L`; skipped at a gain of 1.
//! 3. **Path to white**: a colour whose largest channel `m` passes the knee is mixed toward the
//!    grey of its own luminance until its largest channel is the knee's soft shoulder.
//! 4. **Amount**: `in + a (white - in)`; skipped at `a = 1`. At `a = 0` the unit is the identity,
//!    which only the GPU shape compiles (`CompileStage::gpu_shape`).
//!
//! The knots, inverse widths and coefficients are cast to `f32` once, exactly as the Tone curve's
//! unit casts its own; the per-pixel path is `f32` throughout and ignores the row coordinates. The
//! sRGB transfer, the luminance weights, the reconstruction and the Oklab conversion are the shared
//! ones in [`crate::colour`]; `unit.wgsl` restates them for the GPU in the same order.
use super::super::curve::{Interpolant, Tails, saturating_f32};
use crate::{
    colour::{
        luma,
        oklab::{Oklab, from_oklab, to_oklab},
        srgb::{self, decode_f32, encode_f32},
    },
    modules::PointwiseColor,
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};
use std::sync::Arc;

/// The look unit's GPU program (`unit.wgsl`): the knot count, the curve's ends, its black level,
/// its first slope, the chroma gain, the knee and the amount as words, and the interpolant's
/// knots, inverse widths and coefficients as a storage block laid out as the Tone curve's.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_look_look",
    source: include_str!("unit.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 8,
    enabled: true,
};

/// A resolved look the unit is built from: the checked knots, chroma gain, knee and amount
/// (`0..=200`), all `f64` as stored.
#[derive(Clone, Copy, Debug)]
pub(super) struct LookParameters<'a> {
    pub(super) knots: &'a [[f64; 2]],
    pub(super) chroma: f64,
    pub(super) knee: f64,
    pub(super) amount: f64,
}

/// The look's one pointwise unit.
#[derive(Debug)]
pub(super) struct Look {
    /// The stored values in `f64`, which [`PointwiseColor::describe`] writes exactly.
    knots: Vec<[f64; 2]>,
    chroma64: f64,
    knee64: f64,
    amount64: f64,
    /// `x_i` per knot.
    x32: Vec<f32>,
    /// `1 / h_i` per segment, computed in `f64` and saturating at `f32::MAX`.
    inv_h: Vec<f32>,
    /// `[c0, c1, c2, c3]` per segment.
    coefficients: Vec<[f32; 4]>,
    /// `y_0`.
    first: f32,
    /// `y_{n-1}`.
    last: f32,
    /// `L_floor = decode(y_0)`.
    floor: f32,
    /// `d_0`, the slope the curve continues below `0` with.
    slope: f32,
    /// The Oklab chroma gain.
    chroma: f32,
    /// The path to white's knee.
    knee: f32,
    /// `a = amount / 100`, computed in `f64`.
    amount: f32,
}

impl Look {
    pub(super) fn new(look: LookParameters<'_>) -> Self {
        let interpolant = Interpolant::with_tails(look.knots, Tails::Look);
        let segments = interpolant.len() - 1;
        Self {
            knots: look.knots.to_vec(),
            chroma64: look.chroma,
            knee64: look.knee,
            amount64: look.amount,
            x32: interpolant.xs().iter().map(|&x| x as f32).collect(),
            inv_h: interpolant
                .widths()
                .iter()
                .map(|&h| saturating_f32(1.0 / h))
                .collect(),
            coefficients: (0..segments)
                .map(|i| interpolant.coefficients(i).map(|c| c as f32))
                .collect(),
            first: interpolant.first() as f32,
            last: interpolant.last() as f32,
            floor: srgb::decode(interpolant.first()) as f32,
            slope: interpolant.slopes()[0] as f32,
            chroma: look.chroma as f32,
            knee: look.knee as f32,
            amount: (look.amount / 100.0) as f32,
        }
    }

    /// `T(x)` in `f32` under the look's tails: the first segment continued below `0`, the last
    /// value held at and above the last knot, and otherwise the later segment whose left knot is at
    /// or below `x`, with `t` clamped to `[0, 1]` so a collapsed segment keeps `T` within its knot
    /// values.
    #[inline]
    fn curve(&self, x: f32) -> f32 {
        let n = self.x32.len();
        if x < 0.0 {
            return self.first + self.slope * x;
        }
        if x >= self.x32[n - 1] {
            return self.last;
        }
        let i = (self.x32.partition_point(|&k| k <= x).saturating_sub(1)).min(n - 2);
        let t = ((x - self.x32[i]) * self.inv_h[i]).clamp(0.0, 1.0);
        let [c0, c1, c2, c3] = self.coefficients[i];
        c0 + t * (c1 + t * (c2 + t * c3))
    }

    /// Step 1: encoded luminance through the curve, reconstructed over its black level.
    #[inline]
    pub(super) fn tone(&self, rgb: [f32; 3]) -> [f32; 3] {
        let l_in = luma::rec709(rgb);
        let l_out = decode_f32(self.curve(encode_f32(l_in)));
        luma::reconstruct_over_floor(rgb, l_in, l_out, self.floor)
    }

    /// Step 2: Oklab `a` and `b` scaled by the gain at constant `L`; nothing at a gain of 1.
    #[inline]
    pub(super) fn chroma(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.chroma == 1.0 {
            return rgb;
        }
        let lab = to_oklab(rgb);
        from_oklab(Oklab {
            l: lab.l,
            a: lab.a * self.chroma,
            b: lab.b * self.chroma,
        })
    }

    /// Step 3: a colour whose largest channel `m` passes the knee `k` mixed toward the grey of its
    /// own luminance `Y`, `Y + t (rgb - Y)`, until its largest channel is
    /// `k + (1 - k)(1 - e^{-(m - k)/(1 - k)})`; a colour at or below the knee, or one whose largest
    /// channel is not above its luminance, is unchanged.
    #[inline]
    pub(super) fn white(&self, rgb: [f32; 3]) -> [f32; 3] {
        let m = rgb[0].max(rgb[1]).max(rgb[2]);
        if m <= self.knee {
            return rgb;
        }
        let y = luma::rec709(rgb);
        if m <= y {
            return rgb;
        }
        let room = 1.0 - self.knee;
        let target = self.knee + room * (1.0 - (-(m - self.knee) / room).exp());
        let t = ((target - y) / (m - y)).max(0.0);
        [
            y + t * (rgb[0] - y),
            y + t * (rgb[1] - y),
            y + t * (rgb[2] - y),
        ]
    }

    /// Step 4: the look's output blended with the unit's input, `in + a (white - in)`.
    #[inline]
    pub(super) fn blend(&self, input: [f32; 3], white: [f32; 3]) -> [f32; 3] {
        if self.amount == 1.0 {
            return white;
        }
        let a = self.amount;
        [
            input[0] + a * (white[0] - input[0]),
            input[1] + a * (white[1] - input[1]),
            input[2] + a * (white[2] - input[2]),
        ]
    }

    /// The whole look on one pixel.
    #[inline]
    pub(super) fn pixel(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.amount == 0.0 {
            return rgb;
        }
        let white = self.white(self.chroma(self.tone(rgb)));
        self.blend(rgb, white)
    }
}

impl PointwiseColor for Look {
    /// The four steps on every pixel; nothing is clamped, and the row coordinates are ignored.
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            *pixel = self.pixel(*pixel);
        }
    }

    fn is_finite(&self) -> bool {
        self.x32
            .iter()
            .chain(&self.inv_h)
            .chain(self.coefficients.iter().flatten())
            .chain([
                &self.first,
                &self.last,
                &self.floor,
                &self.slope,
                &self.chroma,
                &self.knee,
                &self.amount,
            ])
            .all(|value| value.is_finite())
    }

    /// The effect and its `f64` knots, chroma gain, knee and amount in the shortest round-trip
    /// form: every cached value is a pure function of them.
    fn describe(&self) -> String {
        let knots: Vec<String> = self
            .knots
            .iter()
            .map(|[x, y]| format!("[{x}, {y}]"))
            .collect();
        format!(
            "{}(tone [{}], chroma {}, knee {}, amount {})",
            super::LOOK_EFFECT,
            knots.join(", "),
            self.chroma64,
            self.knee64,
            self.amount64
        )
    }

    /// The knot count, `y_0`, `y_{n-1}`, the black level, `d_0`, the chroma gain, the knee and
    /// `a` as words, and the knots, inverse widths and per-segment coefficients as the block, laid
    /// out as the Tone curve's: the `f32` values `apply_row` reads, all pure functions of what the
    /// description writes. Built here, `O(knots)`, so a compile that never plans for the GPU
    /// builds nothing.
    fn gpu(&self) -> Option<GpuDescription> {
        let block: Vec<u32> = self
            .x32
            .iter()
            .chain(&self.inv_h)
            .chain(self.coefficients.iter().flatten())
            .map(|value| value.to_bits())
            .collect();
        Some(
            GpuDescription::new(
                &PROGRAM,
                vec![
                    self.x32.len() as u32,
                    self.first.to_bits(),
                    self.last.to_bits(),
                    self.floor.to_bits(),
                    self.slope.to_bits(),
                    self.chroma.to_bits(),
                    self.knee.to_bits(),
                    self.amount.to_bits(),
                ],
            )
            .with_block(Arc::from(block)),
        )
    }
}

#[cfg(test)]
#[path = "unit_tests.rs"]
mod tests;
