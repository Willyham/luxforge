//! Frozen f64 reference for the Saturation and Vibrance units.
//!
//! This mirrors the equations and constants frozen in
//! `docs/design/basic-colour.md`. It is intentionally independent of any
//! production implementation: it exists to give a later implementation an
//! oracle, and the two must never share a bug by sharing code. Read the
//! design document alongside this file; every constant and formula here has
//! its justification there, not here.

use super::{code_to_linear, linear_to_code};

/// Linear sRGB (D65) to LMS, Björn Ottosson's published Oklab matrix.
const M1: [[f64; 3]; 3] = [
    [0.4122214708, 0.5363325363, 0.0514459929],
    [0.2119034982, 0.6806995451, 0.1073969566],
    [0.0883024619, 0.2817188376, 0.6299787005],
];

/// LMS' (post signed-cube-root) to Oklab `(L, a, b)`.
const M2: [[f64; 3]; 3] = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];

/// Oklab `(L, a, b)` to LMS' (pre cube). Independently published and
/// rounded, not an exact algebraic inverse of `M2`; `matrices_round_trip`
/// below measures the residual this introduces.
const M2_INV: [[f64; 3]; 3] = [
    [1.0, 0.3963377774, 0.2158037573],
    [1.0, -0.1055613458, -0.0638541728],
    [1.0, -0.0894841775, -1.2914855480],
];

/// LMS to linear sRGB. Independently published and rounded, not an exact
/// algebraic inverse of `M1`.
const M1_INV: [[f64; 3]; 3] = [
    [4.0767416621, -3.3077115913, 0.2309699292],
    [-1.2684380046, 2.6097574011, -0.3413193965],
    [-0.0041960863, -0.7034186147, 1.7076147010],
];

/// Reference Oklab chroma of the most saturated point on the sRGB gamut
/// surface (`(255, 0, 255)`, the sRGB magenta primary), measured by
/// scanning the gamut surface at 8-bit resolution (~0.3225). This only
/// normalizes the vibrance chroma weight below; it is not a gamut test.
const CHROMA_REFERENCE: f64 = 0.32;

/// Normalized-chroma smoothstep edges for the vibrance chroma weight `w_c`:
/// full gain at and below `CHROMA_LOW`, zero gain at and above
/// `CHROMA_HIGH`, smooth in between.
const CHROMA_LOW: f64 = 0.10;
const CHROMA_HIGH: f64 = 0.70;

/// Skin-like hue band centre and half-width, in Oklab hue degrees
/// (`atan2(b, a)`). Chosen to cover the three documented skin-like patches
/// (measured hues 52.9°, 58.7°, 74.1°) with margin while leaving the sRGB
/// red (29.2°) and yellow (109.8°) primaries close to unweighted.
const SKIN_HUE_CENTER_DEG: f64 = 55.0;
const SKIN_HUE_HALF_WIDTH_DEG: f64 = 35.0;

/// Maximum fraction of vibrance gain removed at the skin hue band centre.
const SKIN_PROTECTION: f64 = 0.6;

/// Below this chroma, `atan2(b, a)` is not numerically meaningful: `a` and
/// `b` are themselves near the floating-point noise floor of `to_oklab`
/// (measured up to ~4e-8 for the 256 grey codes), because `M1`'s three rows
/// do not sum to bit-identical values, so a nominally achromatic input can
/// carry a tiny, essentially arbitrary hue. `CHROMA_EPSILON` is several
/// orders of magnitude above that noise floor and several orders below the
/// smallest chroma of a perceptibly coloured patch (the pastels in
/// `fixtures/basic/colour-cases.json` are all above 0.01), so it never
/// affects a real colour.
const CHROMA_EPSILON: f64 = 1e-4;

/// An Oklab colour: perceptual lightness `l` and the two opponent chroma
/// axes `a` (green-red) and `b` (blue-yellow).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

fn matvec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The signed cube root `sign(x) * |x|^(1/3)`, defined and finite for every
/// finite `x`, including negative ones. `f64::powf(1.0 / 3.0)` is **not**
/// safe here: it returns NaN for a negative base. Out-of-range linear input
/// from an earlier Basic unit (for example a value above 1.0 or below 0.0
/// preserved between units, per the integration contract) can still land a
/// negative LMS component after `M1`, so every Oklab conversion in this
/// module goes through `signed_cbrt`, never `powf`.
fn signed_cbrt(x: f64) -> f64 {
    x.signum() * x.abs().cbrt()
}

/// Linear sRGB (D65) to Oklab. Finite for every finite input, including
/// components outside `[0, 1]` and negative components.
pub fn to_oklab(rgb: [f64; 3]) -> Oklab {
    let lms = matvec(&M1, rgb);
    let lms_root = [
        signed_cbrt(lms[0]),
        signed_cbrt(lms[1]),
        signed_cbrt(lms[2]),
    ];
    let lab = matvec(&M2, lms_root);
    Oklab {
        l: lab[0],
        a: lab[1],
        b: lab[2],
    }
}

/// Oklab to linear sRGB. Finite for every finite input.
pub fn from_oklab(lab: Oklab) -> [f64; 3] {
    let lms_root = matvec(&M2_INV, [lab.l, lab.a, lab.b]);
    let lms = [
        lms_root[0] * lms_root[0] * lms_root[0],
        lms_root[1] * lms_root[1] * lms_root[1],
        lms_root[2] * lms_root[2] * lms_root[2],
    ];
    matvec(&M1_INV, lms)
}

/// Oklab chroma `sqrt(a^2 + b^2)`.
pub fn chroma(lab: Oklab) -> f64 {
    lab.a.hypot(lab.b)
}

/// Oklab hue angle in degrees, `atan2(b, a)` mapped to `(-180, 180]`.
/// `atan2(0, 0)` is defined as `0` (an achromatic colour has no hue; the
/// value is never read by the weight functions below because chroma is 0
/// there too).
pub fn hue_degrees(lab: Oklab) -> f64 {
    lab.b.atan2(lab.a).to_degrees()
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Normalize a hue delta (degrees) to `(-180, 180]`.
fn normalize_hue_delta(mut delta: f64) -> f64 {
    delta %= 360.0;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta <= -180.0 {
        delta += 360.0;
    }
    delta
}

/// `w_c`: full vibrance gain near grey, smoothly falling to zero as chroma
/// approaches `CHROMA_REFERENCE`.
fn chroma_weight(chroma: f64) -> f64 {
    let normalized = (chroma / CHROMA_REFERENCE).max(0.0);
    1.0 - smoothstep(CHROMA_LOW, CHROMA_HIGH, normalized)
}

/// The skin-like hue band response: a raised cosine that is `1` at the band
/// centre and `0` at and beyond the half-width, `0` outside the band. This
/// is a colour heuristic over Oklab hue, not skin detection: it weights
/// every pixel whose hue falls in the band the same way regardless of what
/// the pixel depicts, and it is not a promise about every skin tone.
fn skin_hue_response(hue_deg: f64) -> f64 {
    let delta = normalize_hue_delta(hue_deg - SKIN_HUE_CENTER_DEG);
    if delta.abs() >= SKIN_HUE_HALF_WIDTH_DEG {
        0.0
    } else {
        (delta / SKIN_HUE_HALF_WIDTH_DEG * std::f64::consts::FRAC_PI_2)
            .cos()
            .max(0.0)
    }
}

/// `w_h`: `1` outside the skin-like hue band, falling to `1 - SKIN_PROTECTION`
/// at the band centre.
fn hue_weight(hue_deg: f64) -> f64 {
    1.0 - SKIN_PROTECTION * skin_hue_response(hue_deg)
}

/// The combined vibrance weight `w(C, h) = w_c(C) * w_h(h)`, always in
/// `[0, 1 - SKIN_PROTECTION * 0]` i.e. `[0, 1]`, so the vibrance gain factor
/// below always lands in `[0, 2]` for `v` in `[-100, 100]`, exactly as
/// saturation's gain factor does.
pub fn vibrance_weight(chroma: f64, hue_deg: f64) -> f64 {
    chroma_weight(chroma) * hue_weight(hue_deg)
}

/// Saturation: `chroma_out = chroma_in * (1 + s / 100)`, realized by scaling
/// the Oklab `a` and `b` components by the same factor (which scales
/// `sqrt(a^2 + b^2)` by exactly that factor and preserves `atan2(b, a)` for
/// any nonnegative factor). `s = -100` gives the factor `0.0` exactly, so
/// `a = b = 0.0` exactly: grey with `L` unchanged. `s = 100` doubles chroma.
pub fn apply_saturation(rgb: [f64; 3], s: f64) -> [f64; 3] {
    let lab = to_oklab(rgb);
    let k = 1.0 + s / 100.0;
    from_oklab(Oklab {
        l: lab.l,
        a: lab.a * k,
        b: lab.b * k,
    })
}

/// Vibrance: `chroma_out = chroma_in * (1 + (v / 100) * w(C, h))`, realized
/// the same way as saturation (scaling `a` and `b` by the gain factor).
/// Negative `v` reduces chroma with the same weight `w`. A colour with
/// `chroma == 0` has `w` multiplying a coefficient that is already `0`, so
/// it is returned unchanged regardless of `v`; `L` is never touched. Below
/// `CHROMA_EPSILON` the hue weight is skipped (see its doc comment) and the
/// full chroma weight `1.0` is used instead, so `v = -100` still lands at
/// exactly `a = b = 0` for every near-achromatic input, the same as
/// `apply_saturation(rgb, -100.0)` does.
pub fn apply_vibrance(rgb: [f64; 3], v: f64) -> [f64; 3] {
    let lab = to_oklab(rgb);
    let c = chroma(lab);
    let weight = if c < CHROMA_EPSILON {
        1.0
    } else {
        vibrance_weight(c, hue_degrees(lab))
    };
    let k = 1.0 + (v / 100.0) * weight;
    from_oklab(Oklab {
        l: lab.l,
        a: lab.a * k,
        b: lab.b * k,
    })
}

/// The frozen internal order: vibrance runs before saturation, as the last
/// two units of the Basic layer (`docs/design/basic-and-histogram.md`,
/// "Internal order").
pub fn apply_basic_colour(rgb: [f64; 3], vibrance: f64, saturation: f64) -> [f64; 3] {
    apply_saturation(apply_vibrance(rgb, vibrance), saturation)
}

/// sRGB8 input through the frozen unit order to a quantized sRGB8 output,
/// the same round trip a production caller and `fixtures/basic/colour-cases.json`
/// use.
pub fn apply_basic_colour_srgb8(rgb: [u8; 3], vibrance: f64, saturation: f64) -> [u8; 3] {
    let linear = [
        code_to_linear(rgb[0]),
        code_to_linear(rgb[1]),
        code_to_linear(rgb[2]),
    ];
    let out = apply_basic_colour(linear, vibrance, saturation);
    [
        linear_to_code(out[0]),
        linear_to_code(out[1]),
        linear_to_code(out[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_linear(code: u8) -> [f64; 3] {
        let l = code_to_linear(code);
        [l, l, l]
    }

    /// Matrices round trip: `to_oklab` and `from_oklab` are independently
    /// rounded, not exact algebraic inverses. Measure the residual across a
    /// sweep that includes out-of-gamut values, and hold it to the ~1e-7
    /// order documented in `basic-colour.md`.
    #[test]
    fn matrices_round_trip_within_documented_residual() {
        let mut max_residual: f64 = 0.0;
        let steps = 25;
        let sample = |i: i32| -0.2 + 1.7 * f64::from(i) / f64::from(steps - 1);
        for xi in 0..steps {
            for yi in 0..steps {
                for zi in 0..steps {
                    let rgb = [sample(xi), sample(yi), sample(zi)];
                    let back = from_oklab(to_oklab(rgb));
                    for c in 0..3 {
                        max_residual = max_residual.max((rgb[c] - back[c]).abs());
                    }
                }
            }
        }
        assert!(
            max_residual < 2e-6,
            "round-trip residual {max_residual} exceeds the documented ~1e-7 order"
        );
        assert!(
            max_residual > 0.0,
            "a zero residual would mean the matrices are bit-exact inverses, which basic-colour.md does not claim"
        );
    }

    /// Neutral identity: vibrance 0 and saturation 0 reproduce every one of
    /// the 256 grey codes bit exact through the reference quantizer.
    #[test]
    fn neutral_identity_is_bit_exact_for_all_greys() {
        for code in 0u8..=255 {
            let out = apply_basic_colour_srgb8([code, code, code], 0.0, 0.0);
            assert_eq!(
                out,
                [code, code, code],
                "grey {code} drifted under the identity transform"
            );
        }
    }

    /// Saturation -100 zeroes `a` and `b` exactly (not just approximately):
    /// the gain factor is exactly `0.0` in f64, so `a * 0.0` and `b * 0.0`
    /// are exactly `0.0` for any finite `a`, `b`.
    #[test]
    fn saturation_negative_100_zeroes_chroma_exactly() {
        let samples: [[u8; 3]; 5] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [224, 172, 140],
            [10, 200, 30],
        ];
        for [r, g, b] in samples {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            let k = 1.0 + (-100.0_f64) / 100.0;
            assert_eq!(k, 0.0);
            assert_eq!(lab.a * k, 0.0);
            assert_eq!(lab.b * k, 0.0);
        }
    }

    /// The same property end to end, in linear RGB, over a 6-bit colour
    /// cube (64 levels per channel): every channel of the saturation -100
    /// output agrees within 1e-9, because `M1_INV`'s rows and `M2_INV`'s
    /// first column each sum to exactly 1.0 in f64, so an achromatic Oklab
    /// input reconstructs identical R, G and B.
    #[test]
    fn saturation_negative_100_is_grey_on_a_6_bit_cube() {
        for ri in 0..64u32 {
            for gi in 0..64u32 {
                for bi in 0..64u32 {
                    let r = (ri * 255 / 63) as u8;
                    let g = (gi * 255 / 63) as u8;
                    let b = (bi * 255 / 63) as u8;
                    let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
                    let out = apply_saturation(rgb, -100.0);
                    let spread = (out[0] - out[1])
                        .abs()
                        .max((out[1] - out[2]).abs())
                        .max((out[0] - out[2]).abs());
                    assert!(
                        spread < 1e-9,
                        "saturation -100 left a colour cast for ({r},{g},{b}): {out:?}, spread {spread}"
                    );
                }
            }
        }
    }

    /// Monotone: chroma is nondecreasing in `s` across `[-100, 100]` for a
    /// mixed set of colours.
    #[test]
    fn saturation_chroma_is_monotone_in_s() {
        let colours: [[u8; 3]; 6] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [224, 172, 140],
            [141, 85, 36],
            [200, 50, 200],
        ];
        for [r, g, b] in colours {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            let mut previous = -1.0_f64;
            for s in -100..=100 {
                let k = 1.0 + f64::from(s) / 100.0;
                let c = chroma(Oklab {
                    l: lab.l,
                    a: lab.a * k,
                    b: lab.b * k,
                });
                assert!(
                    c + 1e-12 >= previous,
                    "chroma decreased at s={s} for ({r},{g},{b}): {c} < {previous}"
                );
                previous = c;
            }
        }
    }

    /// Greys stay grey for every `s` and `v`. In exact arithmetic
    /// achromatic input has `a = b = 0`, unaffected by any chroma gain; in
    /// f64 `M1`'s three rows do not sum to bit-identical values, so a grey
    /// input's `a`, `b` are a tiny (measured up to ~4e-8), essentially
    /// arbitrary-hue noise rather than exact zero. `apply_basic_colour`
    /// composes two independent Oklab round trips (vibrance, then
    /// saturation), each of which can amplify that noise through the cube
    /// root/cube pair; the bound below (1e-5) is set from the measured
    /// worst case over the full grey ramp and the full `v`/`s` range
    /// (~2e-6, at white with `v = s = 100`) with a 5x margin, matching the
    /// production-vs-reference tolerance `basic-colour.md` freezes for the
    /// same reason. It is far below one 8-bit output code across the
    /// range that matters (the linear step near white is >1e-3).
    #[test]
    fn greys_stay_grey_for_every_s_and_v() {
        for code in 0u8..=255 {
            let rgb = grey_linear(code);
            for v in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                for s in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    let spread = (out[0] - out[1])
                        .abs()
                        .max((out[1] - out[2]).abs())
                        .max((out[0] - out[2]).abs());
                    assert!(
                        spread < 1e-5,
                        "grey {code} drifted at v={v} s={s}: {out:?}, spread {spread}"
                    );
                }
            }
        }
    }

    /// Vibrance affects a low-chroma colour more than a high-chroma one at
    /// the same `v`. The frozen pair (both at hue 200°, outside the skin
    /// band): `C = 0.02` (near neutral) versus `C = 0.20` (moderately
    /// saturated). See "Vibrance weight examples" in basic-colour.md.
    #[test]
    fn vibrance_affects_low_chroma_more_than_high_chroma() {
        let low = vibrance_weight(0.02, 200.0);
        let high = vibrance_weight(0.20, 200.0);
        assert!(
            low > high,
            "low-chroma weight {low} should exceed high-chroma weight {high}"
        );
        let ratio = low / high;
        assert!(
            (ratio - 23.2727).abs() < 0.01,
            "ratio drifted from the frozen ~23.27x: {ratio}"
        );
    }

    /// The skin-like hue band reduces gain at the band centre relative to
    /// the opposite hue, at equal chroma. Because `w_c` cancels (same
    /// chroma on both sides) and the opposite hue falls outside the band
    /// (`skin_hue_response == 0`), the ratio is exactly `1 - SKIN_PROTECTION`.
    #[test]
    fn skin_band_reduces_gain_relative_to_opposite_hue() {
        let centre = vibrance_weight(0.08, SKIN_HUE_CENTER_DEG);
        let opposite = vibrance_weight(0.08, SKIN_HUE_CENTER_DEG + 180.0);
        let ratio = centre / opposite;
        assert!(
            (ratio - (1.0 - SKIN_PROTECTION)).abs() < 1e-9,
            "expected the band centre to keep exactly 1 - SKIN_PROTECTION = {} of the opposite-hue gain, got {ratio}",
            1.0 - SKIN_PROTECTION
        );
    }

    /// Hue is preserved by saturation for in-gamut results, measured on the
    /// Oklab value the scaling step itself produces (before the round trip
    /// back through the independently rounded matrices, whose own ~1e-7
    /// residual would otherwise dominate a 1e-9 hue tolerance and mask what
    /// the scaling formula guarantees).
    #[test]
    fn saturation_preserves_hue_for_in_gamut_results() {
        let colours: [[u8; 3]; 4] = [[255, 0, 0], [10, 200, 30], [224, 172, 140], [80, 80, 200]];
        for [r, g, b] in colours {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            let h0 = hue_degrees(lab);
            for s in [-90.0, -50.0, 25.0, 50.0, 100.0] {
                let k = 1.0 + s / 100.0;
                let scaled = Oklab {
                    l: lab.l,
                    a: lab.a * k,
                    b: lab.b * k,
                };
                let delta = normalize_hue_delta(hue_degrees(scaled) - h0);
                assert!(
                    delta.abs() < 1e-9,
                    "hue drifted for ({r},{g},{b}) at s={s}: {delta}"
                );
            }
        }
    }

    /// Combined extremes stay finite.
    #[test]
    fn combined_extremes_stay_finite() {
        let colours: [[u8; 3]; 4] = [[255, 0, 0], [224, 172, 140], [0, 0, 0], [255, 255, 255]];
        for [r, g, b] in colours {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            for v in [-100.0, 100.0] {
                for s in [-100.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    assert!(
                        out.iter().all(|c| c.is_finite()),
                        "non-finite output for ({r},{g},{b}) v={v} s={s}: {out:?}"
                    );
                }
            }
        }
    }

    /// Out-of-gamut linear inputs (above 1.0, below 0.0) stay finite
    /// through both units, including through the signed cube root.
    #[test]
    fn out_of_gamut_linear_inputs_stay_finite() {
        let inputs: [[f64; 3]; 5] = [
            [1.5, 0.5, 0.2],
            [-0.1, 0.3, 0.8],
            [1.5, -0.1, 0.7],
            [-0.1, -0.1, -0.1],
            [1.5, 1.5, 1.5],
        ];
        for rgb in inputs {
            for v in [-100.0, 0.0, 100.0] {
                for s in [-100.0, 0.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    assert!(
                        out.iter().all(|c| c.is_finite()),
                        "non-finite output for {rgb:?} v={v} s={s}: {out:?}"
                    );
                }
            }
        }
    }
}
