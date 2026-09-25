//! Texture: the medium-frequency luminance band, isolated as the difference of two self-guided
//! edge-preserving smoothers of the encoded luminance, scaled by the compressive gain and
//! reapplied.
//!
//! ```text
//! E     = encode(luminance(rgb))
//! F     = guided_self(E, r_fine,   EPS_TEXTURE)
//! C     = guided_self(E, r_coarse, EPS_TEXTURE)
//! band  = F - C
//! delta = soft_clip(gain_texture(amount) * band, E, LIMIT_TEXTURE)
//! out   = reconstruct(rgb, L, decode(E + delta))
//! ```
//!
//! Frozen in `docs/design/presence-study.md` and transcribed in `f64` at
//! `crates/lightwell-reference/src/presence.rs`. Structure finer than `r_fine` survives both
//! smoothers and cancels in the difference; structure coarser than `r_coarse` is reproduced by both
//! and cancels too; a strong edge drives both filters' `a` toward 1, so both reproduce it and the
//! band collapses there, which is what keeps the overshoot bounded.

use super::filters::{
    self, GUIDED_SELF_PLANES, Geometry, Plane, PlaneMut, Rect, Scratch, guided_self,
};
use crate::{
    Error,
    colour::{luma, srgb},
    modules::{Global, Parallelism, Planes, PlanesMut, SpatialUnit, Stage},
};

/// Texture: the fine and coarse guided-filter radii in pixels at the reference long side.
const R_FINE_6000: f64 = 1.0;
const R_COARSE_6000: f64 = 4.0;
/// The guided filter's regularization in squared encoded units: `a = 0.5` at a window standard
/// deviation of `sqrt(EPS_TEXTURE) = 0.05` encoded, about 13 of 255 codes.
const EPS_TEXTURE: f32 = 2.5e-3;
/// The soft-clip limit on the encoded excursion, in encoded units.
const LIMIT_TEXTURE: f32 = 0.10;
/// The band is amplified to `1 + GAIN_POS` times its own amplitude at `+100` and removed at `-100`.
const GAIN_POS: f64 = 3.0;
const GAIN_NEG: f64 = 1.0;

/// Texture's fine radius in full-resolution pixels.
pub(super) fn fine_radius(long_side: u32) -> i64 {
    filters::scaled_radius(R_FINE_6000, long_side)
}

/// Texture's coarse radius in full-resolution pixels. The band-nondegeneracy rule keeps it at least
/// one pixel above the fine radius: without it both radii round to 1 below a long side of about
/// 750 px, the two smoothers become identical and the band is identically zero, which would make
/// Texture a silent no-op that is neither the identity nor an effect.
pub(super) fn coarse_radius(long_side: u32) -> i64 {
    filters::scaled_radius(R_COARSE_6000, long_side).max(fine_radius(long_side) + 1)
}

/// Texture's halo: two box passes of the coarse guided filter, at full resolution.
pub(super) fn halo(long_side: u32) -> i64 {
    2 * coarse_radius(long_side)
}

/// Texture's amount-to-gain mapping: piecewise linear, continuous and strictly increasing through 0.
fn gain(amount: f64) -> f32 {
    (if amount >= 0.0 {
        amount / 100.0 * GAIN_POS
    } else {
        amount / 100.0 * GAIN_NEG
    }) as f32
}

/// The Texture unit of one compiled Presence operation.
///
/// `long_side` is the long side of the stage the operation was compiled against, which is the stage
/// it runs at: the host compiles a layer against its input stage and evaluates it at that same
/// stage. Holding it means the radii, the declared halo and the rectangles `apply` reads are all
/// derived from one number, so a unit can never disagree with the halo the host reserved for it.
#[derive(Debug)]
pub(super) struct Texture {
    amount: f64,
    long_side: u32,
    gain: f32,
    r_fine: i64,
    r_coarse: i64,
}

impl Texture {
    /// The unit for a non-zero amount at this stage long side. The module never constructs one at
    /// amount 0: an amount-0 unit is the exact identity, so omitting it changes nothing and saves
    /// its halo and its work.
    pub(super) fn new(amount: f64, long_side: u32) -> Self {
        Self {
            amount,
            long_side,
            gain: gain(amount),
            r_fine: fine_radius(long_side),
            r_coarse: coarse_radius(long_side),
        }
    }
}

impl SpatialUnit for Texture {
    /// The halo is the stage's, fixed when the unit was compiled; the host evaluates the operation
    /// at that same stage.
    fn halo(&self, _: Stage) -> u32 {
        halo(self.long_side) as u32
    }

    /// The encoded plane, the two smoothed planes and the guided filter's own planes, all over the
    /// input region: every rectangle this unit builds lies inside the region the host hands it.
    fn scratch_bytes(&self, region: Stage) -> u64 {
        let values = filters::region_values(region);
        filters::scratch_bytes(values.saturating_mul(3 + GUIDED_SELF_PLANES as u64))
    }

    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        let stage = input.stage();
        let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
        let out = Rect::of(output.region());
        if out.is_empty() {
            return Ok(());
        }
        let geometry = Geometry::new(frame.x1, frame.y1, out);
        let encoded_rect = out.expand(2 * self.r_coarse).clip(frame);

        let mut scratch = Scratch::new(scratch);
        let encoded_buffer = scratch.take(encoded_rect.pixels())?;
        let fine_buffer = scratch.take(out.pixels())?;
        let coarse_buffer = scratch.take(out.pixels())?;

        let mut encoded = PlaneMut::over(encoded_buffer, geometry, encoded_rect)?;
        encoded.for_rows(parallelism, |y, row| {
            for x in encoded_rect.x0..encoded_rect.x1 {
                row[(x - encoded_rect.x0) as usize] =
                    filters::encoded_luminance(input.sample(x, y));
            }
        });
        let encoded: Plane<'_> = encoded.as_plane();

        let mut fine = PlaneMut::over(fine_buffer, geometry, out)?;
        guided_self(
            &encoded,
            self.r_fine,
            EPS_TEXTURE,
            &mut fine,
            &mut scratch,
            parallelism,
        )?;
        let mut coarse = PlaneMut::over(coarse_buffer, geometry, out)?;
        guided_self(
            &encoded,
            self.r_coarse,
            EPS_TEXTURE,
            &mut coarse,
            &mut scratch,
            parallelism,
        )?;

        let (fine, coarse) = (fine.as_plane(), coarse.as_plane());
        output.for_rows(parallelism, |y, red, green, blue| {
            let y = i64::from(y);
            for x in out.x0..out.x1 {
                let rgb = input.sample(x, y);
                let e = encoded.get(x, y);
                let band = fine.get(x, y) - coarse.get(x, y);
                let delta = filters::soft_clip(self.gain * band, e, LIMIT_TEXTURE);
                let value = if delta == 0.0 {
                    // An exact pass-through, with no encode/decode round trip to round it.
                    rgb
                } else {
                    luma::reconstruct(rgb, luma::rec709(rgb), srgb::decode_f32(e + delta))
                };
                let column = (x - out.x0) as usize;
                [red[column], green[column], blue[column]] = value;
            }
        });
        Ok(())
    }

    fn is_finite(&self) -> bool {
        self.amount.is_finite() && self.gain.is_finite()
    }

    fn describe(&self) -> String {
        format!(
            "presence texture(amount={:+}, long side {})",
            self.amount, self.long_side
        )
    }
}
