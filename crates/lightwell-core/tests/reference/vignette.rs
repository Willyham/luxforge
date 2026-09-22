//! Independent f64 reference for the frozen post-crop vignette (TASK-004).
//!
//! This module shares no code with production, matching the convention set by
//! `tests/reference/tone.rs`: it exists so a later production implementation of
//! the `lightwell.vignette` module has an oracle it cannot influence. The
//! frozen equations are written out in full in `docs/design/vignette-study.md`;
//! this file is their literal transcription and the two must be read together.
//! Every constant and formula below is named to match that document.
//!
//! The one exception to "shares no code" is deliberate and narrow: the
//! positive-amount branch works in the same analytically-continued sRGB
//! working domain the Tone study already froze (see `docs/design/basic-tone.md`
//! "Working tone domain"), so this file reuses `encode_srgb_extended` and
//! `decode_srgb_extended` from `tests/reference/tone.rs` rather than
//! duplicating the sRGB OETF a third time in this test tree; it does not reuse
//! any of Tone's curve stages, which are unrelated to the vignette.
//!
//! Domain: `mask` and `apply` take and return plain `f64`/`[f64; 3]` with no
//! gamut clamp beyond what the frozen equations themselves guarantee (the
//! positive branch is proved never to exceed encoded `1`; the negative branch
//! never produces a value farther from zero than the input). Non-finite
//! *inputs* are out of scope for this reference, exactly as `tone.rs` states:
//! production refuses them before this stage is reached, matching the host's
//! "non-finite after any unit is an explicit render/sample error" rule.

use super::tone::{decode_srgb_extended, encode_srgb_extended};

/// The frozen vignette parameters, one field per `lightwell.vignette` slider,
/// each in the agreed UI range. `NEUTRAL` is the all-default payload, which is
/// not the identity for `mask` (the mask still varies by position under the
/// default midpoint/feather) but *is* the identity for `apply` (`amount = 0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VignetteParams {
    /// -100..100, default 0. Negative darkens toward black; positive lightens
    /// toward encoded white. See `apply`.
    pub amount: f64,
    /// 0..100, default 50. Fraction of the shape radius where the falloff
    /// begins. See `mask`.
    pub midpoint: f64,
    /// -100..100, default 0. Morphs the shape from a rounded rectangle
    /// (-100) through an ellipse (0) to a circle (100). See `shape_radius`.
    pub roundness: f64,
    /// 0..100, default 50. Width of the falloff transition. See `mask`.
    pub feather: f64,
}

impl VignetteParams {
    pub const NEUTRAL: Self = Self {
        amount: 0.0,
        midpoint: 50.0,
        roundness: 0.0,
        feather: 50.0,
    };

    fn is_amount_neutral(&self) -> bool {
        self.amount == 0.0
    }
}

/// Normalised pixel-centre coordinates over the output stage: `u, v` are in
/// `(-1, 1)`, with the pixel grid's corners approaching but never reaching
/// `(+-1, +-1)` for any finite `width`/`height` (see "Corner radius, in closed
/// form" in the study note for exactly how close). `width` and `height` are
/// the output stage's pixel dimensions; `x, y` are zero-based pixel indices in
/// `0..width` / `0..height`.
fn pixel_uv(x: u32, y: u32, width: u32, height: u32) -> (f64, f64) {
    let half_w = f64::from(width) / 2.0;
    let half_h = f64::from(height) / 2.0;
    let u = (f64::from(x) + 0.5 - half_w) / half_w;
    let v = (f64::from(y) + 0.5 - half_h) / half_h;
    (u, v)
}

/// The shape radius `r(u, v)`, `0` at the centre, `1` at the continuous
/// corners `(+-1, +-1)` (see `pixel_uv`) for every roundness. `s =
/// roundness / 100` in `[-1, 1]`.
///
/// `s >= 0` (ellipse toward circle): `r^2 = a*u^2 + b*v^2`, `a = (1 - s)/2 +
/// s*(width/2)^2/hd2`, `b = (1 - s)/2 + s*(height/2)^2/hd2`, `hd2` the
/// squared half-diagonal `(width/2)^2 + (height/2)^2`. `a + b == 1` always (by
/// construction: the `(1-s)/2` terms sum to `1-s`, and the `s*(...)/hd2` terms
/// sum to `s*(hd2)/hd2 = s`), so the continuous corners stay at `r = 1` for
/// every `s`, which is what makes `s = 0` (`a = b = 0.5`, the ellipse `r^2 =
/// (u^2+v^2)/2`) and `s = 1` (`a = (width/2)^2/hd2`, `b = (height/2)^2/hd2`,
/// the true circle `r` = distance-to-centre / half-diagonal) meet at the same
/// corner value as every `s` in between.
///
/// `s < 0` (ellipse toward rounded rectangle): `r = ((|u|^p + |v|^p) / 2)^(1/p)`
/// with `p = 2 + 6*(-s)`, so `p` runs from `2` (ellipse, matching the `s = 0`
/// case above exactly) to `8` (rounded rectangle) as `s` runs from `0` to
/// `-1`. This form is homogeneous of degree 1 (`r(t*u, t*v) = t*r(u,v)` for
/// `t >= 0`), so it too reaches exactly `1` at the continuous corners for
/// every `p`. `p = 8` is a rounded rectangle, not a true rectangle: see the
/// study note's "Limitations".
fn shape_radius(u: f64, v: f64, width: u32, height: u32, roundness: f64) -> f64 {
    let s = roundness / 100.0;
    if s >= 0.0 {
        let half_w = f64::from(width) / 2.0;
        let half_h = f64::from(height) / 2.0;
        let hd2 = half_w * half_w + half_h * half_h;
        let a = (1.0 - s) / 2.0 + s * half_w * half_w / hd2;
        let b = (1.0 - s) / 2.0 + s * half_h * half_h / hd2;
        (a * u * u + b * v * v).sqrt()
    } else {
        let p = 2.0 + 6.0 * (-s);
        ((u.abs().powf(p) + v.abs().powf(p)) / 2.0).powf(1.0 / p)
    }
}

/// `3t^2 - 2t^3` on `t` already clamped to `[0, 1]` by the caller; `0` at
/// `t = 0`, `1` at `t = 1`, with zero derivative at both ends (the standard
/// smoothstep).
fn smoothstep(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// The falloff: `0` at `r0 = midpoint*(1 - feather)`, `1` at `r1 = midpoint +
/// (1 - midpoint)*feather` (both in the `[0, 1]` fraction-of-`m`/`f` domain,
/// `m = midpoint/100`, `f = feather/100`), `smoothstep` in between. When `r1 ==
/// r0` (in particular whenever `feather == 0`, and also at the degenerate
/// endpoints `midpoint` = 0 with `feather` = 0 or `midpoint` = 100 with
/// `feather` = 0) the transition has zero width, so the mask is a hard step:
/// `0` for `r <= r0`, `1` for `r > r0`.
fn falloff(r: f64, midpoint: f64, feather: f64) -> f64 {
    let m = midpoint / 100.0;
    let f = feather / 100.0;
    let r0 = m * (1.0 - f);
    let r1 = m + (1.0 - m) * f;
    if r1 == r0 {
        if r <= r0 { 0.0 } else { 1.0 }
    } else {
        let t = ((r - r0) / (r1 - r0)).clamp(0.0, 1.0);
        smoothstep(t)
    }
}

/// The vignette mask `m(x, y)` in `[0, 1]`: `0` inside the midpoint radius,
/// rising to `1` over the feather transition, following the shape
/// `shape_radius` describes. Depends only on `(x, y, width, height, params)`,
/// not on any other pixel or on `amount` -- a crop that recentres the stage
/// (smaller `width`/`height`, same relative position) recomputes an
/// independent mask, which is exactly the "vignette recentres after crop"
/// property the host design relies on.
///
/// Pixel-centre convention: `x` and `y` are zero-based pixel indices in
/// `0..width` / `0..height`; this function does not validate that range (an
/// out-of-range index still produces a mathematically well-defined `u, v`
/// outside `(-1, 1)`, which every formula above accepts, though the host never
/// calls it that way).
pub fn mask(x: u32, y: u32, width: u32, height: u32, params: &VignetteParams) -> f64 {
    let (u, v) = pixel_uv(x, y, width, height);
    let r = shape_radius(u, v, width, height, params.roundness);
    falloff(r, params.midpoint, params.feather)
}

/// The amount equation, given a precomputed `mask` value (`apply` does not
/// recompute position; `vignette_pixel` below is the convenience that does
/// both). `amount` is the raw `-100..100` slider value.
///
/// `amount == 0`: exact identity, for every `mask` (including `mask` values
/// outside `[0, 1]`, though the frozen `mask` never produces one).
///
/// `amount < 0` (darken): `rgb' = rgb * (1 - |a|*mask)` in linear light,
/// `a = amount/100`. At `a = -1` (amount = -100) and `mask = 1`, the gain is
/// exactly `0`: the corner goes to black. This branch never clamps and can
/// produce a negative output for an input that was already negative (an
/// out-of-gamut value preserved from an earlier stage), matching the
/// project's "preserve out-of-range values between colour operations" rule.
///
/// `amount > 0` (lighten): per channel, in the Tone study's analytically
/// continued sRGB encoded domain: `E = encode_ext(c)`; if `E < 1` then
/// `E' = E + a*mask*(1 - E)`, else `E' = E` (values already at or above
/// encoded white are unchanged); `c' = decode_ext(E')`. Because `a` and
/// `mask` are each in `[0, 1]` here, `a*mask <= 1`, so whenever `E < 1`:
/// `E' = E + a*mask*(1-E) <= E + (1-E) = 1`, with equality only when
/// `a*mask == 1` exactly (amount = +100 and mask = 1). So `E'` never exceeds
/// `1` for any input, positive or negative `E` included, proving the
/// "positive amount never exceeds encoded white" property directly from the
/// formula rather than only by sampling.
pub fn apply(rgb_linear: [f64; 3], mask: f64, amount: f64) -> [f64; 3] {
    if amount == 0.0 {
        return rgb_linear;
    }
    let a = amount / 100.0;
    if a < 0.0 {
        let gain = 1.0 - a.abs() * mask;
        [
            rgb_linear[0] * gain,
            rgb_linear[1] * gain,
            rgb_linear[2] * gain,
        ]
    } else {
        rgb_linear.map(|c| {
            let encoded = encode_srgb_extended(c);
            let lifted = if encoded < 1.0 {
                encoded + a * mask * (1.0 - encoded)
            } else {
                encoded
            };
            decode_srgb_extended(lifted)
        })
    }
}

/// Convenience combining `mask` and `apply` for one output pixel: the shape
/// the host's `PointwiseColor` row evaluation actually calls per pixel.
pub fn vignette_pixel(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgb_linear: [f64; 3],
    params: &VignetteParams,
) -> [f64; 3] {
    if params.is_amount_neutral() {
        return rgb_linear;
    }
    let m = mask(x, y, width, height, params);
    apply(rgb_linear, m, params.amount)
}

/// The exact radius at the discrete corner pixel `(0, 0)` (equivalently, by
/// the mirror/flip symmetry `shape_radius` has in `|u|`/`|v|`, every one of
/// the four corner pixels), in closed form. The corner pixel's coordinate
/// magnitude is `|u_corner| = (width - 1) / width`, `|v_corner| = (height - 1)
/// / height` (substitute `x = 0` into `pixel_uv`: `(0.5 - width/2) / (width/2)
/// = 1/width - 1`, whose absolute value is `(width-1)/width`), which
/// approaches but never reaches `1` for any finite `width`/`height` -- exactly
/// the "corners approach (+-1, +-1)" the design doc states. This is *not* a
/// property `mask`/`shape_radius` need at runtime; it exists so the study
/// note and this file's tests can state exactly when the discrete corner
/// pixel's mask reaches `1`, rather than only sampling it. See "Corner
/// saturation condition" in the study note.
pub fn corner_radius(width: u32, height: u32, roundness: f64) -> f64 {
    let (u, v) = pixel_uv(0, 0, width, height);
    shape_radius(u, v, width, height, roundness)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn amount_zero_is_identity_regardless_of_mask() {
        let rgb = [0.5, -0.2, 1.4];
        for m in [0.0, 0.25, 0.5, 1.0, -1.0, 2.0] {
            assert_eq!(apply(rgb, m, 0.0), rgb);
        }
    }

    #[test]
    fn falloff_hard_step_when_feather_is_zero() {
        // midpoint = 50, feather = 0: r0 = r1 = 0.5.
        assert_eq!(falloff(0.4999, 50.0, 0.0), 0.0);
        assert_eq!(falloff(0.5, 50.0, 0.0), 0.0);
        assert_eq!(falloff(0.5001, 50.0, 0.0), 1.0);
    }

    #[test]
    fn shape_radius_a_plus_b_is_always_one_for_every_roundness_in_range() {
        for roundness in [-100.0, -50.0, 0.0, 33.0, 100.0] {
            if roundness < 0.0 {
                continue;
            }
            let s = roundness / 100.0;
            let (w, h) = (37u32, 51u32);
            let half_w = f64::from(w) / 2.0;
            let half_h = f64::from(h) / 2.0;
            let hd2 = half_w * half_w + half_h * half_h;
            let a = (1.0 - s) / 2.0 + s * half_w * half_w / hd2;
            let b = (1.0 - s) / 2.0 + s * half_h * half_h / hd2;
            assert!(
                (a + b - 1.0).abs() < 1e-12,
                "roundness={roundness}: a+b={}",
                a + b
            );
        }
    }

    #[test]
    fn corner_radius_approaches_but_never_reaches_one() {
        for (w, h) in [(24u32, 16u32), (16, 24), (20, 20), (4, 4), (4000, 3000)] {
            for roundness in [-100.0, 0.0, 100.0] {
                let r = corner_radius(w, h, roundness);
                assert!(
                    r < 1.0,
                    "{w}x{h} roundness={roundness}: corner_radius={r} must be < 1"
                );
                // A loose sanity floor only, not a frozen property: even the
                // smallest size tested here (4x4, rounded-rectangle p=8)
                // still reads 0.75, comfortably above this floor.
                assert!(
                    r > 0.5,
                    "{w}x{h} roundness={roundness}: corner_radius={r} unexpectedly small"
                );
            }
        }
    }
}
