//! The Basic module's colour unit: `ColourAdjust`, fusing Vibrance and Saturation, sharing the
//! Oklab conversion frozen in `docs/design/basic-colour.md` and the independent f64 reference at
//! `crates/lightwell-core/tests/reference/colour.rs`. Both parameters scale Oklab `a`/`b` about
//! the achromatic axis and leave `L` untouched; neither clamps internally, per the design's gamut
//! policy. Vibrance's gain and saturation's gain compose into one factor before either is applied,
//! so one pixel converts to Oklab and back exactly once instead of once per parameter — the two
//! parameters are mathematically independent scalings of the same `(a, b)` pair around the same
//! axis with `L` untouched, so composing their gains before scaling changes no arithmetic on
//! `a`/`b` itself; only the number of round trips through the independently rounded inverse
//! matrices changes (see `colour_adjust_matches_the_sequential_pair_within_the_frozen_tolerance`
//! below for the measured difference this makes). This file never imports the reference: the two
//! are written
//! independently so they never share a bug.
use crate::modules::PointwiseColor;

/// Linear sRGB (D65) to LMS, Björn Ottosson's published Oklab matrix, reproduced with every
/// published digit as `f64` (matching the design and the independent reference byte for byte) and
/// converted once, below, to the `f32` values the hot loop actually multiplies by: coefficients are
/// computed in f64, units run in f32, exactly as the colour processing contract requires.
const M1_F64: [[f64; 3]; 3] = [
    [0.4122214708, 0.5363325363, 0.0514459929],
    [0.2119034982, 0.6806995451, 0.1073969566],
    [0.0883024619, 0.2817188376, 0.6299787005],
];

/// LMS' (post signed-cube-root) to Oklab `(L, a, b)`.
const M2_F64: [[f64; 3]; 3] = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];

/// Oklab `(L, a, b)` to LMS'. Independently published and rounded, not an exact algebraic inverse
/// of `M2_F64`.
const M2_INV_F64: [[f64; 3]; 3] = [
    [1.0, 0.3963377774, 0.2158037573],
    [1.0, -0.1055613458, -0.0638541728],
    [1.0, -0.0894841775, -1.2914855480],
];

/// LMS to linear sRGB. Independently published and rounded, not an exact algebraic inverse of
/// `M1_F64`.
const M1_INV_F64: [[f64; 3]; 3] = [
    [4.0767416621, -3.3077115913, 0.2309699292],
    [-1.2684380046, 2.6097574011, -0.3413193965],
    [-0.0041960863, -0.7034186147, 1.7076147010],
];

const fn as_f32(m: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
    ]
}

const M1: [[f32; 3]; 3] = as_f32(M1_F64);
const M2: [[f32; 3]; 3] = as_f32(M2_F64);
const M2_INV: [[f32; 3]; 3] = as_f32(M2_INV_F64);
const M1_INV: [[f32; 3]; 3] = as_f32(M1_INV_F64);

/// Reference Oklab chroma of the most saturated point on the sRGB gamut surface (`(255, 0, 255)`,
/// sRGB magenta), measured by scanning the gamut surface at 8-bit resolution. Normalizes the
/// vibrance chroma weight below; not a gamut test itself.
const CHROMA_REFERENCE_F64: f64 = 0.32;
/// Normalized-chroma smoothstep edges for the vibrance chroma weight `w_c`: full gain at and below
/// `CHROMA_LOW`, zero gain at and above `CHROMA_HIGH`.
const CHROMA_LOW_F64: f64 = 0.10;
const CHROMA_HIGH_F64: f64 = 0.70;
/// Skin-like hue band centre and half-width, in Oklab hue degrees (`atan2(b, a)`).
const SKIN_HUE_CENTER_DEG_F64: f64 = 55.0;
const SKIN_HUE_HALF_WIDTH_DEG_F64: f64 = 35.0;
/// Maximum fraction of vibrance gain removed at the skin hue band centre.
const SKIN_PROTECTION_F64: f64 = 0.6;
/// Below this chroma, hue is numerical noise rather than a meaningful angle (see
/// `docs/design/basic-colour.md`, "Near-black and achromatic behaviour"); the hue weight is skipped
/// and the full chroma weight used instead, so vibrance −100 still lands at exact grey.
const CHROMA_EPSILON_F64: f64 = 1e-4;

const CHROMA_REFERENCE: f32 = CHROMA_REFERENCE_F64 as f32;
const CHROMA_LOW: f32 = CHROMA_LOW_F64 as f32;
const CHROMA_HIGH: f32 = CHROMA_HIGH_F64 as f32;
const SKIN_HUE_CENTER_DEG: f32 = SKIN_HUE_CENTER_DEG_F64 as f32;
const SKIN_HUE_HALF_WIDTH_DEG: f32 = SKIN_HUE_HALF_WIDTH_DEG_F64 as f32;
const SKIN_PROTECTION: f32 = SKIN_PROTECTION_F64 as f32;
const CHROMA_EPSILON: f32 = CHROMA_EPSILON_F64 as f32;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Oklab {
    pub(crate) l: f32,
    pub(crate) a: f32,
    pub(crate) b: f32,
}

fn matvec(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The signed cube root `sign(x) * |x|^(1/3)`, defined and finite for every finite `x`, including
/// negative ones. `f32::powf(1.0 / 3.0)` is **not** safe here: it returns NaN for a negative base,
/// and out-of-range linear input preserved between units (per the integration contract) can land a
/// negative LMS component after `M1`.
fn signed_cbrt(x: f32) -> f32 {
    x.signum() * x.abs().cbrt()
}

pub(crate) fn to_oklab(rgb: [f32; 3]) -> Oklab {
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

/// Oklab to linear sRGB. An achromatic colour (`a = b = 0`, either sign of zero) reconstructs to
/// `L^3` in all three channels, which is what the exact matrices give it: `M2^-1`'s first column is
/// one and each row of `M1^-1` sums to one. The published rows are rounded, though, so in f32 the
/// general path returns three channels a few ulps apart, and where they straddle an output code
/// threshold a fully desaturated grey would render with one channel a code off.
pub(crate) fn from_oklab(lab: Oklab) -> [f32; 3] {
    if lab.a == 0.0 && lab.b == 0.0 {
        let grey = lab.l * lab.l * lab.l;
        return [grey, grey, grey];
    }
    let lms_root = matvec(&M2_INV, [lab.l, lab.a, lab.b]);
    let lms = [
        lms_root[0] * lms_root[0] * lms_root[0],
        lms_root[1] * lms_root[1] * lms_root[1],
        lms_root[2] * lms_root[2] * lms_root[2],
    ];
    matvec(&M1_INV, lms)
}

pub(crate) fn chroma(lab: Oklab) -> f32 {
    lab.a.hypot(lab.b)
}

/// Oklab hue angle in degrees, `atan2(b, a)` mapped to `(-180, 180]` by `atan2` itself.
pub(crate) fn hue_degrees(lab: Oklab) -> f32 {
    lab.b.atan2(lab.a).to_degrees()
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Normalize a hue delta (degrees) to `(-180, 180]`.
fn normalize_hue_delta(mut delta: f32) -> f32 {
    delta %= 360.0;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta <= -180.0 {
        delta += 360.0;
    }
    delta
}

/// `w_c`: full vibrance gain near grey, smoothly falling to zero as chroma approaches
/// `CHROMA_REFERENCE`.
fn chroma_weight(chroma: f32) -> f32 {
    let normalized = (chroma / CHROMA_REFERENCE).max(0.0);
    1.0 - smoothstep(CHROMA_LOW, CHROMA_HIGH, normalized)
}

/// The skin-like hue band response: a raised cosine that is `1` at the band centre and `0` at and
/// beyond the half-width, `0` outside the band. A colour heuristic over Oklab hue, not skin
/// detection: it weights every pixel whose hue falls in the band the same way regardless of what
/// the pixel depicts, and it is not a promise about every skin tone.
fn skin_hue_response(hue_deg: f32) -> f32 {
    let delta = normalize_hue_delta(hue_deg - SKIN_HUE_CENTER_DEG);
    if delta.abs() >= SKIN_HUE_HALF_WIDTH_DEG {
        0.0
    } else {
        (delta / SKIN_HUE_HALF_WIDTH_DEG * std::f32::consts::FRAC_PI_2)
            .cos()
            .max(0.0)
    }
}

/// `w_h`: `1` outside the skin-like hue band, falling to `1 - SKIN_PROTECTION` at the band centre.
fn hue_weight(hue_deg: f32) -> f32 {
    1.0 - SKIN_PROTECTION * skin_hue_response(hue_deg)
}

/// The combined vibrance weight `w(C, h) = w_c(C) * w_h(h)`, always in `[0, 1]`, so `k` always
/// lands in `[0, 2]` for `v` in `[-100, 100]`, the same bound saturation's factor has.
fn vibrance_weight(chroma: f32, hue_deg: f32) -> f32 {
    chroma_weight(chroma) * hue_weight(hue_deg)
}

/// The fused Vibrance/Saturation unit. Converts one pixel to Oklab once; computes vibrance's
/// chroma- and hue-dependent gain `k_v = 1 + (vibrance / 100) * w(C, h)` from that one conversion,
/// computes saturation's uniform gain `k_s = 1 + saturation / 100` (precomputed once for the whole
/// unit, not per pixel), scales `a`/`b` by the single combined factor `k_v * k_s`, and converts
/// back once.
///
/// This is mathematically identical to running the old separate `Vibrance` then `Saturation` units
/// in sequence: both scale `(a, b)` about the achromatic axis with `L` untouched and vibrance's
/// weight is a pure function of the *input* pixel's chroma and hue either way, so composing the
/// two gains before scaling changes no arithmetic on `a`/`b`. The only difference from the
/// sequential pair is that the sequential path round-trips through the independently rounded
/// inverse matrices (`M2_INV`, `M1_INV`) twice — once after vibrance, again after saturation reads
/// back the vibrance-adjusted pixel — while this fused unit round-trips once; the residual that
/// second conversion would have carried (documented in `docs/design/basic-colour.md` as the
/// composed-unit near-black spread, on the order of `2e-6` in f64) does not accumulate here. See
/// `colour_adjust_matches_the_sequential_pair_within_one_e_minus_6` for the measured maximum
/// difference against the sequential pair.
///
/// `saturation = -100` gives `k_s = 0.0` exactly (computed in f64 and cast, so the exactness
/// survives the cast); combined with `vibrance = 0`, which gives `k_v = 1.0` exactly regardless of
/// the per-pixel weight (the weight is multiplied by a zero gain), the combined factor is `0.0`
/// exactly and `a = b = 0.0` exactly regardless of the input: neutral vibrance with saturation
/// -100 is exact grey, matching the old `Saturation::new(-100.0)`'s exactness.
#[derive(Debug)]
pub(super) struct ColourAdjust {
    /// The stored parameter values, kept for [`PointwiseColor::describe`] and the finiteness
    /// check.
    vibrance: f64,
    saturation: f64,
    /// `vibrance / 100`, computed in f64 and cast once; the per-pixel weight `w(C, h)` cannot be
    /// precomputed because it depends on each pixel's chroma and hue. Exactly `0.0` when vibrance
    /// is neutral, which is used to skip the per-pixel hue computation entirely.
    vibrance_gain: f32,
    /// `1 + saturation / 100`, computed in f64 and cast once: saturation's gain never depends on
    /// the pixel, unlike vibrance's.
    saturation_k: f32,
}

impl ColourAdjust {
    pub(super) fn new(vibrance: f64, saturation: f64) -> Self {
        Self {
            vibrance,
            saturation,
            vibrance_gain: (vibrance / 100.0) as f32,
            saturation_k: (1.0 + saturation / 100.0) as f32,
        }
    }
}

impl PointwiseColor for ColourAdjust {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        // Vibrance's weight needs a chroma (`hypot`) and, past the epsilon, a hue (`atan2` and a
        // `cos`) per pixel. Skip all of it whenever it cannot change the result: when vibrance is
        // neutral, its gain multiplies the weight by exactly zero regardless of the weight's own
        // value, so `k_v` is `1.0` unconditionally and neither chroma nor hue needs computing.
        let vibrance_neutral = self.vibrance_gain == 0.0;
        for pixel in rgb {
            let lab = to_oklab(*pixel);
            let vibrance_k = if vibrance_neutral {
                1.0
            } else {
                let c = chroma(lab);
                // Below `CHROMA_EPSILON`, hue is numerical noise rather than a meaningful angle
                // (see `docs/design/basic-colour.md`, "Near-black and achromatic behaviour"): skip
                // reading it and use the full chroma weight instead.
                let weight = if c < CHROMA_EPSILON {
                    1.0
                } else {
                    vibrance_weight(c, hue_degrees(lab))
                };
                1.0 + self.vibrance_gain * weight
            };
            let k = vibrance_k * self.saturation_k;
            *pixel = from_oklab(Oklab {
                l: lab.l,
                a: lab.a * k,
                b: lab.b * k,
            });
        }
    }

    fn is_finite(&self) -> bool {
        self.vibrance.is_finite()
            && self.saturation.is_finite()
            && self.vibrance_gain.is_finite()
            && self.saturation_k.is_finite()
    }

    /// Both stored values, at the parameter's declared display precision (0 decimals). The host
    /// compares compiled operations by this string, so two units that describe themselves
    /// identically process identically: the per-pixel factor is a pure function of the pixel and
    /// `vibrance_gain`/`saturation_k`, themselves pure functions of `vibrance`/`saturation`.
    fn describe(&self) -> String {
        format!(
            "colour-adjust(vibrance:{:+.0}, saturation:{:+.0})",
            self.vibrance, self.saturation
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::{fs, path::PathBuf};

    /// An achromatic Oklab colour reconstructs to three bit-identical channels, `L^3`, for either
    /// sign of zero, over a million evenly spaced `L` in `[0, 1]`. Without the achromatic branch the
    /// rounded `M1^-1` rows leave the three channels up to about `5e-7` apart, and a few of those
    /// land on different output codes.
    #[test]
    fn an_achromatic_colour_reconstructs_to_three_identical_channels() {
        let samples = 1_000_000;
        for step in 0..=samples {
            let l = step as f32 / samples as f32;
            for (a, b) in [(0.0f32, 0.0f32), (-0.0, 0.0), (0.0, -0.0), (-0.0, -0.0)] {
                let [r, g, bl] = from_oklab(Oklab { l, a, b });
                assert!(
                    r == g && g == bl && r == l * l * l,
                    "L = {l}: {:?}",
                    [r, g, bl]
                );
            }
        }
    }

    /// Saturation `-100` makes every pixel an exact grey: three equal output codes, however
    /// colourful, dark or near-grey the input was.
    #[test]
    fn saturation_minus_100_renders_three_equal_codes_for_every_pixel() {
        let unit = ColourAdjust::new(0.0, -100.0);
        let mut row: Vec<[f32; 3]> = Vec::new();
        for r in (0..=255u16).step_by(5) {
            for g in (0..=255u16).step_by(5) {
                for b in (0..=255u16).step_by(5) {
                    row.push([r as u8, g as u8, b as u8].map(code_to_linear));
                }
            }
        }
        // Near-greys a code or two off the axis, where the conversion's own noise lives.
        for code in 0..=255u8 {
            let near = code.saturating_add(1);
            row.push([code, code, near].map(code_to_linear));
            row.push([near, code, code].map(code_to_linear));
        }
        unit.apply_row(0, 0, &mut row);
        for pixel in &row {
            let codes = crate::render::quantize_pixel(*pixel);
            assert!(
                codes[0] == codes[1] && codes[1] == codes[2],
                "{pixel:?} renders {codes:?}"
            );
        }
    }

    fn code_to_linear(code: u8) -> f32 {
        let encoded = f64::from(code) / 255.0;
        (if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }) as f32
    }

    fn row_of(rgb: [f32; 3]) -> [[f32; 3]; 1] {
        [rgb]
    }

    /// The independent statement of both parameters, applied to one pixel through the production
    /// `PointwiseColor` unit (the fused `ColourAdjust`, replacing the old separate `Vibrance` then
    /// `Saturation` units the frozen fixtures were originally checked against).
    fn apply_basic_colour(rgb: [f32; 3], vibrance: f64, saturation: f64) -> [f32; 3] {
        let mut row = row_of(rgb);
        ColourAdjust::new(vibrance, saturation).apply_row(0, 0, &mut row);
        row[0]
    }

    /// Replicates the pre-fusion two-unit path exactly: convert to Oklab, apply vibrance's
    /// chroma-/hue-dependent gain to `a`/`b`, convert back, convert to Oklab *again* from that
    /// result, apply saturation's uniform gain, convert back. Built from the same private helpers
    /// the production `ColourAdjust` unit uses, so it is not a third independent reference — it is
    /// the two-round-trip composition the fused unit replaces, kept here only to prove the fused
    /// unit agrees with it.
    fn sequential_vibrance_then_saturation(
        rgb: [f32; 3],
        vibrance: f64,
        saturation: f64,
    ) -> [f32; 3] {
        let vibrance_gain = (vibrance / 100.0) as f32;
        let lab = to_oklab(rgb);
        let c = chroma(lab);
        let weight = if c < CHROMA_EPSILON {
            1.0
        } else {
            vibrance_weight(c, hue_degrees(lab))
        };
        let vibrance_k = 1.0 + vibrance_gain * weight;
        let after_vibrance = from_oklab(Oklab {
            l: lab.l,
            a: lab.a * vibrance_k,
            b: lab.b * vibrance_k,
        });

        let saturation_k = (1.0 + saturation / 100.0) as f32;
        let lab2 = to_oklab(after_vibrance);
        from_oklab(Oklab {
            l: lab2.l,
            a: lab2.a * saturation_k,
            b: lab2.b * saturation_k,
        })
    }

    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum Input {
        Srgb8 { rgb: [u8; 3] },
        Linear { rgb: [f64; 3] },
    }

    impl Input {
        fn to_linear_f32(&self) -> [f32; 3] {
            match self {
                Input::Srgb8 { rgb } => [
                    code_to_linear(rgb[0]),
                    code_to_linear(rgb[1]),
                    code_to_linear(rgb[2]),
                ],
                Input::Linear { rgb } => [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32],
            }
        }
    }

    #[derive(Deserialize)]
    struct Case {
        #[allow(dead_code)]
        name: String,
        input: Input,
        vibrance: f64,
        saturation: f64,
        expected_linear: [f64; 3],
    }

    #[derive(Deserialize)]
    struct FixtureFile {
        cases: Vec<Case>,
    }

    fn fixture_path() -> PathBuf {
        // CARGO_MANIFEST_DIR is crates/lightwell-core; fixtures/ is repo-root-level.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("basic")
            .join("colour-cases.json")
    }

    /// Every one of the 144 frozen fixture cases, run through the production `f32` fused unit
    /// (`ColourAdjust`, one Oklab round trip per pixel instead of the two the fixtures were
    /// originally generated against), within the design's frozen tolerance
    /// `1e-5 + 1e-5 * |reference|`.
    ///
    /// Maximum observed error over the 144 cases: **2.3565749481813114e-6** — smaller than the
    /// 3.478e-6 the pre-fusion two-unit path measured against the same fixtures, consistent with
    /// removing one Oklab round trip's rounding rather than adding any — well inside the frozen
    /// tolerance, which is at least 1e-5 for every case and grows with the magnitude of the
    /// reference value.
    #[test]
    fn every_fixture_case_matches_the_frozen_tolerance() {
        let raw = fs::read_to_string(fixture_path()).expect("fixtures/basic/colour-cases.json");
        let file: FixtureFile = serde_json::from_str(&raw).expect("a parsed fixture file");
        assert_eq!(file.cases.len(), 144, "the frozen corpus has 144 cases");

        let mut max_error = 0.0_f64;
        for case in &file.cases {
            let linear = case.input.to_linear_f32();
            let actual = apply_basic_colour(linear, case.vibrance, case.saturation);
            for (channel, (&actual, &expected)) in
                actual.iter().zip(case.expected_linear.iter()).enumerate()
            {
                let error = (f64::from(actual) - expected).abs();
                max_error = max_error.max(error);
                let tolerance = 1e-5 + 1e-5 * expected.abs();
                assert!(
                    error <= tolerance,
                    "{} channel {channel}: production {actual} against reference {expected}, error {error} exceeds tolerance {tolerance}",
                    case.name
                );
            }
        }
        // Recorded so a future change that silently widens the gap is visible in review, not just
        // in a passing assertion against the outer bound.
        assert!(
            max_error < 1e-5,
            "maximum observed error {max_error} regressed past the frozen tolerance's own floor"
        );
    }

    /// Neutral vibrance with saturation −100 zeroes `a` and `b` exactly, the same property the
    /// reference proves for saturation alone, preserved by the fused unit: vibrance's gain is
    /// exactly `0.0` when neutral (so its `k` is exactly `1.0` regardless of the per-pixel weight),
    /// and saturation's `k` is exactly `0.0` at `-100`, so the combined factor is exactly `0.0`.
    /// `a = b = 0.0` exactly, and the design's `M1⁻¹`/`M2⁻¹` exactness property (their rows/column
    /// summing to `1.0` in f64) then reconstructs the rendered pixel *bit-close* equal R, G, B —
    /// the design's own wording, not bit-identical: `M1_INV`/`M2_INV` are independently rounded to
    /// `f32` per element, so the f64 row/column-sum identity does not survive the cast exactly.
    /// Checked here to the same headroom `greys_stay_grey_for_every_v_and_s` uses.
    #[test]
    fn neutral_vibrance_with_saturation_negative_100_is_exact_grey() {
        let unit = ColourAdjust::new(0.0, -100.0);
        assert_eq!(unit.vibrance_gain, 0.0);
        assert_eq!(unit.saturation_k, 0.0);
        for [r, g, b] in [[255u8, 0, 0], [224, 172, 140], [10, 200, 30]] {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            let combined_k = 1.0_f32 * unit.saturation_k;
            assert_eq!(combined_k, 0.0);
            assert_eq!(lab.a * combined_k, 0.0);
            assert_eq!(lab.b * combined_k, 0.0);

            let mut row = row_of(rgb);
            unit.apply_row(0, 0, &mut row);
            let spread = (row[0][0] - row[0][1])
                .abs()
                .max((row[0][1] - row[0][2]).abs())
                .max((row[0][0] - row[0][2]).abs());
            assert!(spread < 1e-4, "not grey for {r},{g},{b}: {row:?}");
        }
    }

    /// The fused `ColourAdjust` unit agrees with running the two steps it replaces (vibrance's own
    /// Oklab round trip, then saturation's) to within the design's frozen tolerance
    /// `1e-5 + 1e-5 * |reference|`, over a dense 33³ linear cube spanning `[-0.2, 1.5]` per channel
    /// (including out-of-range values above white and below black), for a representative spread of
    /// vibrance/saturation pairs including the extremes.
    ///
    /// A flat absolute bound cannot hold over this domain: even at `v = s = 0`, where both paths
    /// apply gain `1.0` throughout, the sequential path still round-trips through the
    /// independently rounded inverse matrices twice against the fused path's one, and
    /// `signed_cbrt`'s derivative diverges as its input approaches zero (documented in
    /// `docs/design/basic-colour.md`, "Near-black and achromatic behaviour" and "Matrix round-trip
    /// residual"), so the two paths' gap widens near-linearly with `|reference|` rather than
    /// staying flat — exactly what the frozen tolerance's relative term is for.
    ///
    /// Maximum observed absolute difference over the full swept domain: **2.4795532e-5** (at
    /// vibrance −100, saturation +100, linear input `[-0.0406, -0.2, 1.3406]`, reference channel
    /// magnitude ≈13.69 — an extreme, deliberately out-of-gamut combination); every difference
    /// measured stays inside the frozen relative+absolute tolerance. Re-measured by
    /// `cargo test --package lightwell-core basic::colour::tests -- --nocapture` if this comment
    /// goes stale.
    #[test]
    fn colour_adjust_matches_the_sequential_pair_within_the_frozen_tolerance() {
        const STEPS: usize = 33;
        const LOW: f32 = -0.2;
        const HIGH: f32 = 1.5;
        let component = |i: usize| LOW + (HIGH - LOW) * i as f32 / (STEPS - 1) as f32;

        let mut max_difference = 0.0_f64;
        for (vibrance, saturation) in [
            (100.0, 100.0),
            (-100.0, -100.0),
            (100.0, -100.0),
            (-100.0, 100.0),
            (50.0, -20.0),
            (-30.0, 60.0),
            (0.0, 0.0),
        ] {
            let unit = ColourAdjust::new(vibrance, saturation);
            for xi in 0..STEPS {
                for yi in 0..STEPS {
                    for zi in 0..STEPS {
                        let rgb = [component(xi), component(yi), component(zi)];
                        let mut fused = row_of(rgb);
                        unit.apply_row(0, 0, &mut fused);
                        let sequential =
                            sequential_vibrance_then_saturation(rgb, vibrance, saturation);
                        for (&fused_channel, &sequential_channel) in
                            fused[0].iter().zip(sequential.iter())
                        {
                            let difference =
                                (f64::from(fused_channel) - f64::from(sequential_channel)).abs();
                            max_difference = max_difference.max(difference);
                            let tolerance = 1e-5 + 1e-5 * f64::from(sequential_channel).abs();
                            assert!(
                                difference <= tolerance,
                                "v={vibrance} s={saturation} rgb={rgb:?}: fused {fused_channel} \
                                 against sequential {sequential_channel}, difference {difference} \
                                 exceeds tolerance {tolerance}"
                            );
                        }
                    }
                }
            }
        }
        // Recorded so a future change that silently widens the gap is visible in review, not just
        // in a passing assertion against the outer bound.
        assert!(
            max_difference < 3e-5,
            "maximum observed difference {max_difference} regressed past the recorded figure's \
             own margin"
        );
    }

    /// Greys stay grey for every `v` and `s`: every one of the 256 grey codes, run through the
    /// production units, holds every channel within the reference's documented ~2e-6 f64 spread
    /// plus f32 headroom.
    #[test]
    fn greys_stay_grey_for_every_v_and_s() {
        for code in 0u8..=255 {
            let level = code_to_linear(code);
            let rgb = [level, level, level];
            for v in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                for s in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    let spread = (out[0] - out[1])
                        .abs()
                        .max((out[1] - out[2]).abs())
                        .max((out[0] - out[2]).abs());
                    assert!(
                        spread < 1e-4,
                        "grey {code} drifted at v={v} s={s}: {out:?}, spread {spread}"
                    );
                }
            }
        }
    }

    /// Neither unit clamps: combined extremes on out-of-gamut linear input stay finite.
    #[test]
    fn combined_extremes_stay_finite() {
        let inputs: [[f32; 3]; 5] = [
            [1.5, 0.5, 0.2],
            [-0.1, 0.3, 0.8],
            [1.5, -0.1, 0.7],
            [-0.1, -0.1, -0.1],
            [1.5, 1.5, 1.5],
        ];
        for rgb in inputs {
            for v in [-100.0, 100.0] {
                for s in [-100.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    assert!(
                        out.iter().all(|c| c.is_finite()),
                        "non-finite output for {rgb:?} v={v} s={s}: {out:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_is_the_identity_gain() {
        let mut row = row_of([0.0, 0.25, 1.0]);
        ColourAdjust::new(0.0, 0.0).apply_row(0, 0, &mut row);
        let tolerance = 1e-5_f32;
        for (actual, expected) in row[0].iter().zip([0.0, 0.25, 1.0]) {
            assert!(
                (actual - expected).abs() < tolerance,
                "{actual} vs {expected}"
            );
        }
    }

    #[test]
    fn finiteness_follows_the_stored_values_and_their_derived_coefficients() {
        assert!(ColourAdjust::new(100.0, 100.0).is_finite());
        assert!(ColourAdjust::new(-100.0, -100.0).is_finite());
        assert!(ColourAdjust::new(0.0, 0.0).is_finite());
        assert!(!ColourAdjust::new(f64::NAN, 0.0).is_finite());
        assert!(!ColourAdjust::new(0.0, f64::NAN).is_finite());
        assert!(!ColourAdjust::new(f64::INFINITY, 0.0).is_finite());
        assert!(!ColourAdjust::new(0.0, f64::INFINITY).is_finite());
    }

    #[test]
    fn the_description_names_both_stored_values_at_zero_decimals() {
        assert_eq!(
            ColourAdjust::new(30.0, -20.0).describe(),
            "colour-adjust(vibrance:+30, saturation:-20)"
        );
        assert_eq!(
            ColourAdjust::new(-100.0, 0.0).describe(),
            "colour-adjust(vibrance:-100, saturation:+0)"
        );
        assert_ne!(
            ColourAdjust::new(30.0, 0.0).describe(),
            ColourAdjust::new(31.0, 0.0).describe(),
            "two different stored vibrance values never describe themselves the same way"
        );
        assert_ne!(
            ColourAdjust::new(0.0, 30.0).describe(),
            ColourAdjust::new(0.0, 31.0).describe(),
            "two different stored saturation values never describe themselves the same way"
        );
    }
}
