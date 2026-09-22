//! The colour mixer's one pointwise unit.
//!
//! This is the production transcription of the equations frozen in
//! `docs/design/mixer-study.md`: the eight-centre raised-cosine hue basis, the chroma ramp, the
//! per-range and per-direction bounded rotation, the chroma factor and the Oklab `L` gamma
//! response, composed hue then chroma then luminance from weights evaluated once on the **input**
//! pixel. The study is the only place the justifications live; this file restates the constants
//! and nothing else.
//!
//! The Oklab conversion is **not** restated here: it is the one accepted in
//! `docs/design/basic-colour.md` and implemented in [`crate::modules::basic::colour`], reused
//! unchanged so the two colour modules cannot drift apart and the matrices exist once.
//!
//! Coefficients are computed in f64 — the frozen centres, the eight gaps derived from them and the
//! twenty-four slider amounts — and cast once into the small fixed-size f32 arrays the unit holds.
//! The per-pixel path is f32 throughout, ignores the row coordinates and touches nothing but the
//! pixel it was given and those arrays.
use crate::modules::{
    PointwiseColor,
    basic::colour::{self, Oklab},
};

/// The eight hue ranges, in wheel order (ascending Oklab hue angle).
pub(super) const RANGE_COUNT: usize = 8;

/// Range names in wheel order, used by the parameter names and by [`PointwiseColor::describe`].
pub(super) const RANGE_NAMES: [&str; RANGE_COUNT] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

/// The sRGB reference colour whose Oklab hue defines each range centre. The control rails are
/// built from these same codes, so the slider a person drags is painted in the colour whose hue
/// defines the range it addresses.
pub(super) const RANGE_REFERENCE_CODES: [[u8; 3]; RANGE_COUNT] = [
    [255, 0, 0],
    [255, 128, 0],
    [255, 255, 0],
    [0, 255, 0],
    [0, 255, 255],
    [0, 0, 255],
    [128, 0, 255],
    [255, 0, 255],
];

/// The frozen range centres from `docs/design/mixer-study.md`: the Oklab hue angle in degrees,
/// `[0, 360)`, of each [`RANGE_REFERENCE_CODES`] entry. The study's
/// `centres_match_the_reference_colours` recomputes every one of them from the 8-bit codes and
/// holds it to 1e-9, and `centres_match_the_frozen_study_table` below holds this copy to the same
/// literals, so neither table can move without the other failing.
const CENTRE_HUES_DEG: [f64; RANGE_COUNT] = [
    29.233_885_192_3,
    52.984_679_594_0,
    109.769_232_076_5,
    142.495_338_887_8,
    194.768_947_932_0,
    264.052_020_638_1,
    293.937_640_814_5,
    328.363_417_923_5,
];

/// Chroma ramp edge `C0`: the Oklab chroma at and above which a colour is fully affected. Below it
/// the three effects fade smoothly to nothing and reach exactly zero on the achromatic axis, which
/// is what keeps a grey, a near-grey and shadow noise uncoloured by every slider.
const CHROMA_RAMP_EDGE_F64: f64 = 0.02;

/// Saturation gain `k_s`: `-100` at a range's own centre gives the factor `0` (exact grey) and
/// `+100` gives `2` (double chroma), the `[0, 2]` gain range Basic's saturation already uses.
const SATURATION_GAIN_F64: f64 = 1.0;

/// Luminance response base: the Oklab `L` exponent is `2^(-m)` for the weighted amount `m` in
/// `[-1, 1]`, so `+100` is a square root and `-100` a square.
const LUMINANCE_GAMMA_BASE_F64: f64 = 2.0;

const CHROMA_RAMP_EDGE: f32 = CHROMA_RAMP_EDGE_F64 as f32;
const LUMINANCE_GAMMA_BASE: f32 = LUMINANCE_GAMMA_BASE_F64 as f32;

/// Wrap a difference of two angles already in `[0, 360)` — so in `(-360, 360)` — back into
/// `[0, 360)`. One addition suffices for that input, which keeps this usable in a const context.
const fn wrap_turn(degrees: f64) -> f64 {
    if degrees < 0.0 {
        degrees + 360.0
    } else {
        degrees
    }
}

/// The eight gaps, each from one centre to the next counter-clockwise, derived from
/// [`CENTRE_HUES_DEG`] rather than frozen a second time. They sum to 360 degrees.
const fn gaps_deg() -> [f64; RANGE_COUNT] {
    let mut gaps = [0.0; RANGE_COUNT];
    let mut range = 0;
    while range < RANGE_COUNT {
        gaps[range] = wrap_turn(CENTRE_HUES_DEG[(range + 1) % RANGE_COUNT] - CENTRE_HUES_DEG[range]);
        range += 1;
    }
    gaps
}

const GAPS_DEG: [f64; RANGE_COUNT] = gaps_deg();

/// The rotation a range's hue slider reaches at `±100`: half the gap to the neighbouring centre
/// **in the direction of travel**, so `+100` means the same fraction of the way to the next colour
/// everywhere, and the hue map stays strictly monotone under any one slider.
const fn max_rotation_deg(range: usize, positive: bool) -> f64 {
    let gap = if positive {
        GAPS_DEG[range]
    } else {
        GAPS_DEG[(range + RANGE_COUNT - 1) % RANGE_COUNT]
    };
    0.5 * gap
}

const fn as_f32(values: [f64; RANGE_COUNT]) -> [f32; RANGE_COUNT] {
    let mut out = [0.0f32; RANGE_COUNT];
    let mut range = 0;
    while range < RANGE_COUNT {
        out[range] = values[range] as f32;
        range += 1;
    }
    out
}

const CENTRES: [f32; RANGE_COUNT] = as_f32(CENTRE_HUES_DEG);
const GAPS: [f32; RANGE_COUNT] = as_f32(GAPS_DEG);

/// The chroma ramp `w_c(C)`: exactly `0` on the achromatic axis, smoothstep to exactly `1` at and
/// above [`CHROMA_RAMP_EDGE`].
fn chroma_ramp(chroma: f32) -> f32 {
    let t = (chroma / CHROMA_RAMP_EDGE).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// An Oklab hue angle, which [`colour::hue_degrees`] reports in `(-180, 180]`, wrapped to
/// `[0, 360)`.
fn normalize_hue_deg(hue_deg: f32) -> f32 {
    if hue_deg < 0.0 {
        hue_deg + 360.0
    } else {
        hue_deg
    }
}

/// The segment a hue falls in: the centre most recently passed going counter-clockwise, and the
/// fraction `t` of the way across that pair's gap.
///
/// Chosen by `argmin` over the wrapped distances rather than by an interval test, exactly as the
/// reference does, so no hue is left unassigned whatever rounding does at a segment edge: a hue a
/// hair *below* a centre lands in the previous segment at `t = 1`, where that segment's weight is
/// `0` and the whole weight goes to the centre itself.
fn segment(hue_deg: f32) -> (usize, f32) {
    let mut lower = 0usize;
    let mut lower_distance = f32::INFINITY;
    for (range, centre) in CENTRES.iter().enumerate() {
        let distance = normalize_hue_deg(hue_deg - centre);
        if distance < lower_distance {
            lower_distance = distance;
            lower = range;
        }
    }
    (lower, (lower_distance / GAPS[lower]).clamp(0.0, 1.0))
}

/// The Oklab `L` response for a weighted amount `m`: a gamma on the `[0, 1]` part of `L` with any
/// excess passed through, so `0` and `1` are exact fixed points and out-of-gamut `L` is preserved
/// rather than folded back.
fn luminance_response(l: f32, amount: f32) -> f32 {
    let gamma = LUMINANCE_GAMMA_BASE.powf(-amount);
    let core = l.clamp(0.0, 1.0);
    core.powf(gamma) + (l - core)
}

/// The colour mixer unit: twenty-four sliders reduced to three per-range f32 coefficient arrays.
///
/// The stored f64 values are kept only for [`PointwiseColor::describe`] and the finiteness check;
/// nothing per-pixel reads them. The three coefficient arrays are the whole of the unit's state:
/// 24 f32 values, a fixed 96 bytes, allocated once when a layer compiles and never again.
#[derive(Debug)]
pub(super) struct Mixer {
    hue: [f64; RANGE_COUNT],
    saturation: [f64; RANGE_COUNT],
    luminance: [f64; RANGE_COUNT],
    /// `(hue_i / 100) * theta_i(sign)`: the rotation in degrees this range contributes at weight
    /// one. The direction of travel, and so which half-gap bounds it, depends only on the slider's
    /// sign, never on the pixel, so the whole product is a per-range constant.
    rotation_deg: [f32; RANGE_COUNT],
    /// `(saturation_i / 100) * k_s`: this range's contribution to the chroma factor's sum.
    chroma_gain: [f32; RANGE_COUNT],
    /// `luminance_i / 100`: this range's contribution to the luminance amount `m`.
    luminance_amount: [f32; RANGE_COUNT],
}

impl Mixer {
    /// Build the unit from the twenty-four slider values, in the wheel order of [`RANGE_NAMES`].
    pub(super) fn new(
        hue: [f64; RANGE_COUNT],
        saturation: [f64; RANGE_COUNT],
        luminance: [f64; RANGE_COUNT],
    ) -> Self {
        let rotation_deg = std::array::from_fn(|range| {
            let amount = hue[range] / 100.0;
            (amount * max_rotation_deg(range, amount >= 0.0)) as f32
        });
        let chroma_gain =
            std::array::from_fn(|range| (saturation[range] / 100.0 * SATURATION_GAIN_F64) as f32);
        let luminance_amount = std::array::from_fn(|range| (luminance[range] / 100.0) as f32);
        Self {
            hue,
            saturation,
            luminance,
            rotation_deg,
            chroma_gain,
            luminance_amount,
        }
    }

    /// The three weighted amounts one pixel sees, from weights evaluated once on that pixel: the
    /// rotation in degrees, the chroma factor and the luminance amount `m`.
    ///
    /// Exactly two range weights are non-zero, so this reads two entries of each coefficient array
    /// rather than summing over eight; the other six terms are `+0.0` in the frozen equations and
    /// adding `+0.0` is exact.
    fn amounts(&self, ramp: f32, hue_deg: f32) -> (f32, f32, f32) {
        let (lower, t) = segment(hue_deg);
        let upper = (lower + 1) % RANGE_COUNT;
        let weight = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
        let other = 1.0 - weight;
        let rotation =
            ramp * (weight * self.rotation_deg[lower] + other * self.rotation_deg[upper]);
        let saturation = ramp * (weight * self.chroma_gain[lower] + other * self.chroma_gain[upper]);
        let amount =
            ramp * (weight * self.luminance_amount[lower] + other * self.luminance_amount[upper]);
        // The factor cannot be negative in exact arithmetic; the clamp is the frozen guard against
        // a rounded sum landing a hair below -1.
        (rotation, (1.0 + saturation).max(0.0), amount)
    }
}

impl PointwiseColor for Mixer {
    /// One Oklab round trip per pixel, with the row coordinates ignored: the mixer is pointwise in
    /// the strict sense.
    ///
    /// The two branches are exactness, not approximation. A pixel whose chroma ramp is zero — every
    /// grey and near-grey — has all three amounts exactly zero, where the frozen equations
    /// multiply by `cos 0 = 1`, `sin 0 = 0`, factor `1` and `L^1`; skipping the rotation and the
    /// response computes the identical numbers without the transcendental calls. The same holds
    /// per effect when only some of the sliders are set.
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let lab = colour::to_oklab(*pixel);
            let ramp = chroma_ramp(colour::chroma(lab));
            let (rotation, factor, amount) = if ramp == 0.0 {
                (0.0, 1.0, 0.0)
            } else {
                self.amounts(ramp, normalize_hue_deg(colour::hue_degrees(lab)))
            };
            // Hue, then chroma, applied to the `(a, b)` vector directly — rotate, then scale —
            // rather than by recomposing `C` and `h`: a rotation and a non-negative scalar commute,
            // so this realizes the frozen `(C, h)` equations exactly while leaving a zero rotation
            // and a unit factor as the exact identity in Oklab.
            let (a, b) = if rotation == 0.0 {
                (factor * lab.a, factor * lab.b)
            } else {
                let (sin, cos) = rotation.to_radians().sin_cos();
                (
                    factor * (lab.a * cos - lab.b * sin),
                    factor * (lab.a * sin + lab.b * cos),
                )
            };
            let l = if amount == 0.0 {
                lab.l
            } else {
                luminance_response(lab.l, amount)
            };
            *pixel = colour::from_oklab(Oklab { l, a, b });
        }
    }

    fn is_finite(&self) -> bool {
        let stored = self
            .hue
            .iter()
            .chain(&self.saturation)
            .chain(&self.luminance)
            .all(|value| value.is_finite());
        let derived = self
            .rotation_deg
            .iter()
            .chain(&self.chroma_gain)
            .chain(&self.luminance_amount)
            .all(|value| value.is_finite());
        stored && derived
    }

    /// The unit and its non-neutral sliders, at the parameters' declared display precision. The
    /// host compares compiled operations by this string, so two units that describe themselves
    /// identically must process identically: every coefficient is a pure function of the values
    /// named here, and a slider left out is exactly zero.
    fn describe(&self) -> String {
        let mut fields = Vec::new();
        for (property, values) in [
            ("hue", &self.hue),
            ("saturation", &self.saturation),
            ("luminance", &self.luminance),
        ] {
            for (range, value) in values.iter().enumerate() {
                if *value != 0.0 {
                    fields.push(format!("{}-{property}:{value:+.0}", RANGE_NAMES[range]));
                }
            }
        }
        if fields.is_empty() {
            return "mixer(neutral)".into();
        }
        format!("mixer({})", fields.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{fs, path::PathBuf};

    /// The frozen study's own centre table, restated here so this file's copy cannot drift from
    /// the document without a test naming it.
    const STUDY_CENTRES: [f64; RANGE_COUNT] = [
        29.233_885_192_3,
        52.984_679_594_0,
        109.769_232_076_5,
        142.495_338_887_8,
        194.768_947_932_0,
        264.052_020_638_1,
        293.937_640_814_5,
        328.363_417_923_5,
    ];

    /// The frozen study's gap table, to six decimals as the document prints it.
    const STUDY_GAPS: [f64; RANGE_COUNT] = [
        23.750_794, 56.784_552, 32.726_107, 52.273_609, 69.283_073, 29.885_620, 34.425_777,
        60.870_467,
    ];

    fn code_to_linear(code: u8) -> f64 {
        let encoded = f64::from(code) / 255.0;
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }

    fn apply(rgb: [f32; 3], unit: &Mixer) -> [f32; 3] {
        let mut row = [rgb];
        unit.apply_row(0, 0, &mut row);
        row[0]
    }

    fn neutral() -> Mixer {
        Mixer::new([0.0; RANGE_COUNT], [0.0; RANGE_COUNT], [0.0; RANGE_COUNT])
    }

    #[test]
    fn centres_match_the_frozen_study_table() {
        assert_eq!(CENTRE_HUES_DEG, STUDY_CENTRES);
        for (range, gap) in GAPS_DEG.iter().enumerate() {
            assert!(
                (gap - STUDY_GAPS[range]).abs() < 5e-7,
                "{}: derived gap {gap} against the study's {}",
                RANGE_NAMES[range],
                STUDY_GAPS[range]
            );
        }
        let total: f64 = GAPS_DEG.iter().sum();
        assert!((total - 360.0).abs() < 1e-9, "the eight gaps span one turn");
    }

    /// `+100` reaches half the gap to the neighbour in the direction of travel, and `-100` half the
    /// gap behind: the study's per-range, per-direction bound, read off the coefficients the unit
    /// actually holds.
    #[test]
    fn the_rotation_coefficient_is_half_the_gap_in_the_direction_of_travel() {
        for range in 0..RANGE_COUNT {
            let mut hue = [0.0; RANGE_COUNT];
            hue[range] = 100.0;
            let forward = Mixer::new(hue, [0.0; RANGE_COUNT], [0.0; RANGE_COUNT]);
            assert!(
                (f64::from(forward.rotation_deg[range]) - 0.5 * GAPS_DEG[range]).abs() < 1e-4,
                "{}", RANGE_NAMES[range]
            );
            hue[range] = -100.0;
            let backward = Mixer::new(hue, [0.0; RANGE_COUNT], [0.0; RANGE_COUNT]);
            let behind = GAPS_DEG[(range + RANGE_COUNT - 1) % RANGE_COUNT];
            assert!(
                (f64::from(backward.rotation_deg[range]) + 0.5 * behind).abs() < 1e-4,
                "{}", RANGE_NAMES[range]
            );
        }
    }

    /// Every one of the 256 grey codes is returned bit for bit by every single slider at both
    /// extremes: the chroma ramp is exactly zero on the achromatic axis, so the unit takes the
    /// identity branch and the only arithmetic left is the Oklab round trip the neutral unit does.
    #[test]
    fn greys_are_bit_identical_to_the_neutral_round_trip_under_every_slider() {
        let neutral = neutral();
        for code in 0u8..=255 {
            let linear = code_to_linear(code) as f32;
            let grey = [linear, linear, linear];
            let expected = apply(grey, &neutral);
            for range in 0..RANGE_COUNT {
                for value in [-100.0, 100.0] {
                    for property in 0..3 {
                        let mut sliders = [[0.0; RANGE_COUNT]; 3];
                        sliders[property][range] = value;
                        let unit = Mixer::new(sliders[0], sliders[1], sliders[2]);
                        assert_eq!(
                            apply(grey, &unit),
                            expected,
                            "grey {code} moved under {} {property} at {value}",
                            RANGE_NAMES[range]
                        );
                    }
                }
            }
        }
    }

    /// A range's weight is exactly `+0.0` outside the two segments beside its centre, so its three
    /// sliders change nothing there — equality, not a tolerance.
    #[test]
    fn a_zero_weight_colour_is_bit_identical_under_that_range() {
        let neutral = neutral();
        for range in 0..RANGE_COUNT {
            for offset in [2usize, RANGE_COUNT - 2] {
                let codes = RANGE_REFERENCE_CODES[(range + offset) % RANGE_COUNT];
                let rgb = [
                    code_to_linear(codes[0]) as f32,
                    code_to_linear(codes[1]) as f32,
                    code_to_linear(codes[2]) as f32,
                ];
                let expected = apply(rgb, &neutral);
                for value in [-100.0, 50.0, 100.0] {
                    for property in 0..3 {
                        let mut sliders = [[0.0; RANGE_COUNT]; 3];
                        sliders[property][range] = value;
                        let unit = Mixer::new(sliders[0], sliders[1], sliders[2]);
                        assert_eq!(
                            apply(rgb, &unit),
                            expected,
                            "{} at {value} (property {property}) moved a two-centres-away colour",
                            RANGE_NAMES[range]
                        );
                    }
                }
            }
        }
    }

    /// `describe` names the unit and exactly its non-neutral fields, and two different payloads
    /// never describe themselves the same way.
    #[test]
    fn describe_names_the_non_neutral_fields() {
        assert_eq!(neutral().describe(), "mixer(neutral)");
        let mut hue = [0.0; RANGE_COUNT];
        hue[0] = 20.0;
        let mut luminance = [0.0; RANGE_COUNT];
        luminance[4] = -15.0;
        let unit = Mixer::new(hue, [0.0; RANGE_COUNT], luminance);
        assert_eq!(unit.describe(), "mixer(red-hue:+20, aqua-luminance:-15)");
        assert!(unit.is_finite());
    }

    // -------------------------------------------------------------------------------------------
    // The frozen fixtures
    // -------------------------------------------------------------------------------------------

    fn fixture_file() -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/mixer/mixer-cases.json");
        serde_json::from_str(&fs::read_to_string(&path).expect("the mixer fixtures"))
            .expect("valid fixture JSON")
    }

    fn slider_array(set: &Value, property: &str) -> [f64; RANGE_COUNT] {
        let values = set[property].as_array().expect("a slider array");
        std::array::from_fn(|range| values[range].as_f64().expect("a number"))
    }

    /// Production against every one of the 414 frozen cases, in linear light, within the study's
    /// frozen tolerance `1e-5 + 1e-5 * |reference|`. The 8-bit inputs are decoded in f64 exactly as
    /// the reference decodes them and cast to f32, so what this measures is the unit's own f32
    /// error and nothing else; the quantization boundary is checked through the real render path in
    /// `tests/mixer_module.rs`.
    #[test]
    fn production_matches_every_frozen_fixture_case_within_the_frozen_tolerance() {
        let file = fixture_file();
        assert_eq!(
            file["range_order"]
                .as_array()
                .expect("the range order")
                .iter()
                .map(|name| name.as_str().expect("a name"))
                .collect::<Vec<_>>(),
            RANGE_NAMES,
            "the fixture's range order is the unit's wheel order"
        );
        let sets = file["parameter_sets"].as_array().expect("the sets");
        let cases = file["cases"].as_array().expect("the cases");
        assert_eq!(cases.len(), 414, "every frozen case is checked");

        let mut worst = 0.0f64;
        let mut worst_case = String::new();
        for case in cases {
            let name = case["name"].as_str().expect("a case name");
            let set = sets
                .iter()
                .find(|set| set["name"] == case["parameters"])
                .unwrap_or_else(|| panic!("{name}: the named parameter set"));
            let unit = Mixer::new(
                slider_array(set, "hue"),
                slider_array(set, "saturation"),
                slider_array(set, "luminance"),
            );
            assert!(unit.is_finite(), "{name}");
            let input = &case["input"];
            let rgb: [f32; 3] = match input["kind"].as_str().expect("an input kind") {
                "srgb8" => {
                    let codes = input["rgb"].as_array().expect("three codes");
                    std::array::from_fn(|channel| {
                        code_to_linear(codes[channel].as_u64().expect("a code") as u8) as f32
                    })
                }
                "linear" => {
                    let values = input["rgb"].as_array().expect("three values");
                    std::array::from_fn(|channel| values[channel].as_f64().expect("a value") as f32)
                }
                other => panic!("{name}: unknown input kind {other}"),
            };
            let produced = apply(rgb, &unit);
            let expected = case["expected_linear"].as_array().expect("three values");
            for channel in 0..3 {
                let reference = expected[channel].as_f64().expect("a value");
                let deviation = (f64::from(produced[channel]) - reference).abs();
                let tolerance = 1e-5 + 1e-5 * reference.abs();
                assert!(
                    deviation <= tolerance,
                    "{name} channel {channel}: production {} against reference {reference}, \
                     deviation {deviation} over the tolerance {tolerance}",
                    produced[channel]
                );
                if deviation > worst {
                    worst = deviation;
                    worst_case = format!("{name} channel {channel}");
                }
            }
        }
        // Printed with --nocapture so the handoff can quote a measured figure rather than a bound.
        println!("maximum observed deviation {worst} at {worst_case}");
        assert!(worst < 1e-5 + 1e-5, "the worst case stays inside the bound");
    }
}
