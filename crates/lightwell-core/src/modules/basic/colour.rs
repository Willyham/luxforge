//! The Basic module's colour units: Vibrance and Saturation, sharing the Oklab conversion frozen
//! in `docs/design/basic-colour.md` and the independent f64 reference at
//! `crates/lightwell-core/tests/reference/colour.rs`. Both units scale Oklab `a`/`b` about the
//! achromatic axis and leave `L` untouched; neither clamps internally, per the design's gamut
//! policy. This file never imports the reference: the two are written independently so they never
//! share a bug.
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
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
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

fn to_oklab(rgb: [f32; 3]) -> Oklab {
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

fn from_oklab(lab: Oklab) -> [f32; 3] {
    let lms_root = matvec(&M2_INV, [lab.l, lab.a, lab.b]);
    let lms = [
        lms_root[0] * lms_root[0] * lms_root[0],
        lms_root[1] * lms_root[1] * lms_root[1],
        lms_root[2] * lms_root[2] * lms_root[2],
    ];
    matvec(&M1_INV, lms)
}

fn chroma(lab: Oklab) -> f32 {
    lab.a.hypot(lab.b)
}

/// Oklab hue angle in degrees, `atan2(b, a)` mapped to `(-180, 180]` by `atan2` itself.
fn hue_degrees(lab: Oklab) -> f32 {
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

/// Saturation: scales Oklab `a`/`b` by `k = 1 + s / 100`, which scales chroma
/// `sqrt(a^2 + b^2)` by exactly that factor and preserves `atan2(b, a)` for `k >= 0`. `s = -100`
/// gives `k = 0.0` exactly (computed in f64 and cast, so the exactness survives the cast), so
/// `a = b = 0.0` exactly regardless of the input: saturation -100 is exact grey.
#[derive(Debug)]
pub(super) struct Saturation {
    /// The stored parameter value, kept for [`PointwiseColor::describe`] and the finiteness check.
    s: f64,
    k: f32,
}

impl Saturation {
    pub(super) fn new(s: f64) -> Self {
        Self {
            s,
            k: (1.0 + s / 100.0) as f32,
        }
    }
}

impl PointwiseColor for Saturation {
    fn apply_row(&self, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let lab = to_oklab(*pixel);
            *pixel = from_oklab(Oklab {
                l: lab.l,
                a: lab.a * self.k,
                b: lab.b * self.k,
            });
        }
    }

    fn is_finite(&self) -> bool {
        self.s.is_finite() && self.k.is_finite()
    }

    /// The stored value, at the parameter's declared display precision (0 decimals). The host
    /// compares compiled operations by this string, so two units that describe themselves
    /// identically process identically: `k` is a pure function of `s`.
    fn describe(&self) -> String {
        format!("saturation({:+.0})", self.s)
    }
}

/// Vibrance: scales Oklab `a`/`b` by `k = 1 + (v / 100) * w(C, h)`, the chroma- and hue-dependent
/// weight `vibrance_weight` computes per pixel. Below `CHROMA_EPSILON` the hue weight is skipped
/// (the hue of near-achromatic input is numerical noise, not a meaningful angle) and the full
/// chroma weight `1.0` is used instead, so `v = -100` still lands at exactly `a = b = 0` for every
/// near-achromatic input, matching `Saturation::new(-100.0)`.
#[derive(Debug)]
pub(super) struct Vibrance {
    /// The stored parameter value, kept for [`PointwiseColor::describe`] and the finiteness check.
    v: f64,
    /// `v / 100`, computed in f64 and cast once; the per-pixel weight `w(C, h)` cannot be
    /// precomputed because it depends on each pixel's chroma and hue.
    gain: f32,
}

impl Vibrance {
    pub(super) fn new(v: f64) -> Self {
        Self {
            v,
            gain: (v / 100.0) as f32,
        }
    }
}

impl PointwiseColor for Vibrance {
    fn apply_row(&self, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let lab = to_oklab(*pixel);
            let c = chroma(lab);
            let weight = if c < CHROMA_EPSILON {
                1.0
            } else {
                vibrance_weight(c, hue_degrees(lab))
            };
            let k = 1.0 + self.gain * weight;
            *pixel = from_oklab(Oklab {
                l: lab.l,
                a: lab.a * k,
                b: lab.b * k,
            });
        }
    }

    fn is_finite(&self) -> bool {
        self.v.is_finite() && self.gain.is_finite()
    }

    /// The stored value, at the parameter's declared display precision (0 decimals). The host
    /// compares compiled operations by this string, so two units that describe themselves
    /// identically process identically: the per-pixel weight is a pure function of the pixel and
    /// `gain`, which is itself a pure function of `v`.
    fn describe(&self) -> String {
        format!("vibrance({:+.0})", self.v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::{fs, path::PathBuf};

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

    /// The independent statement of both units composed in the frozen order (vibrance, then
    /// saturation), applied to one pixel through the production `PointwiseColor` units.
    fn apply_basic_colour(rgb: [f32; 3], vibrance: f64, saturation: f64) -> [f32; 3] {
        let mut row = row_of(rgb);
        Vibrance::new(vibrance).apply_row(&mut row);
        Saturation::new(saturation).apply_row(&mut row);
        row[0]
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

    /// Every one of the 144 frozen fixture cases, run through the production `f32` units in the
    /// frozen order (vibrance, then saturation), within the design's frozen tolerance
    /// `1e-5 + 1e-5 * |reference|`.
    ///
    /// Maximum observed error over the 144 cases: **3.478e-6** (well inside the frozen tolerance,
    /// which is at least 1e-5 for every case and grows with the magnitude of the reference value).
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

    /// Saturation −100 zeroes `a` and `b` exactly, the same property the reference proves.
    #[test]
    fn saturation_negative_100_zeroes_chroma_exactly() {
        let unit = Saturation::new(-100.0);
        assert_eq!(unit.k, 0.0);
        for [r, g, b] in [[255u8, 0, 0], [224, 172, 140], [10, 200, 30]] {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            assert_eq!(lab.a * unit.k, 0.0);
            assert_eq!(lab.b * unit.k, 0.0);
        }
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
        Vibrance::new(0.0).apply_row(&mut row);
        let tolerance = 1e-5_f32;
        for (actual, expected) in row[0].iter().zip([0.0, 0.25, 1.0]) {
            assert!(
                (actual - expected).abs() < tolerance,
                "{actual} vs {expected}"
            );
        }
        let mut row = row_of([0.0, 0.25, 1.0]);
        Saturation::new(0.0).apply_row(&mut row);
        for (actual, expected) in row[0].iter().zip([0.0, 0.25, 1.0]) {
            assert!(
                (actual - expected).abs() < tolerance,
                "{actual} vs {expected}"
            );
        }
    }

    #[test]
    fn finiteness_follows_the_stored_value_and_its_gain() {
        assert!(Vibrance::new(100.0).is_finite());
        assert!(Vibrance::new(-100.0).is_finite());
        assert!(!Vibrance::new(f64::NAN).is_finite());
        assert!(!Vibrance::new(f64::INFINITY).is_finite());
        assert!(Saturation::new(100.0).is_finite());
        assert!(Saturation::new(-100.0).is_finite());
        assert!(!Saturation::new(f64::NAN).is_finite());
        assert!(!Saturation::new(f64::INFINITY).is_finite());
    }

    #[test]
    fn the_description_carries_the_stored_value_and_its_sign_at_zero_decimals() {
        assert_eq!(Vibrance::new(30.0).describe(), "vibrance(+30)");
        assert_eq!(Vibrance::new(-100.0).describe(), "vibrance(-100)");
        assert_eq!(Saturation::new(-100.0).describe(), "saturation(-100)");
        assert_eq!(Saturation::new(20.0).describe(), "saturation(+20)");
        assert_ne!(
            Vibrance::new(30.0).describe(),
            Vibrance::new(31.0).describe(),
            "two different stored values never describe themselves the same way"
        );
    }
}
