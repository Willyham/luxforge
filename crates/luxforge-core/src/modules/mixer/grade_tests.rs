//! The grading unit's correctness checks: whole buffers against the `f64` reference of the
//! design's initial equations (`luxforge_reference::grade`), and the properties the design
//! requires of them: neutrality, dormant settings, endpoints, the wheel's hue and seam, the tonal
//! weights, Global's independence, monotone luminance and extended input.
use super::*;
use crate::colour::srgb;

fn wheel(hue: f64, saturation: f64, luminance: f64) -> Wheel {
    Wheel {
        hue,
        saturation,
        luminance,
    }
}

fn grading(wheels: [Wheel; WHEEL_COUNT], blending: f64, balance: f64) -> Grading {
    Grading {
        wheels,
        blending,
        balance,
    }
}

/// One wheel set, the others neutral, at the default Blending and Balance.
fn only(index: usize, set: Wheel) -> Grading {
    let mut wheels = [Wheel::default(); WHEEL_COUNT];
    wheels[index] = set;
    grading(wheels, DEFAULT_BLENDING, 0.0)
}

const SHADOWS: usize = 0;
const MIDTONES: usize = 1;
const HIGHLIGHTS: usize = 2;
const GLOBAL: usize = 3;

fn apply(rgb: [f32; 3], grading: Grading) -> [f32; 3] {
    let mut row = [rgb];
    Grade::new(grading).apply_row(0, 0, &mut row);
    row[0]
}

fn lab(rgb: [f32; 3]) -> [f64; 3] {
    oklab::lab_f64(rgb.map(f64::from))
}

fn hue_deg(lab: [f64; 3]) -> f64 {
    lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0)
}

fn grey(code: u8) -> [f32; 3] {
    [srgb::decode_u8(code) as f32; 3]
}

/// The f64 reference's parameters for these fields.
fn params(grading: &Grading) -> luxforge_reference::grade::GradeParams {
    luxforge_reference::grade::GradeParams {
        wheels: grading
            .wheels
            .map(|wheel| luxforge_reference::grade::Wheel {
                hue: wheel.hue,
                saturation: wheel.saturation,
                luminance: wheel.luminance,
            }),
        blending: grading.blending,
        balance: grading.balance,
    }
}

/// The f64 reference (`luxforge_reference::grade`), written from the design's equations.
fn reference(rgb: [f64; 3], grading: &Grading) -> [f64; 3] {
    luxforge_reference::grade::apply(rgb, &params(grading))
}

/// A deterministic whole buffer: a grey ramp, an encoded RGB gradient lattice, a hue wheel at
/// three lightnesses, near-greys, deep shadows and signed and above-white linear values.
fn buffer() -> Vec<[f32; 3]> {
    let mut pixels: Vec<[f32; 3]> = (0u8..=255).step_by(5).map(grey).collect();
    for r in (0u8..=255).step_by(51) {
        for g in (0u8..=255).step_by(51) {
            for b in (0u8..=255).step_by(51) {
                pixels.push([r, g, b].map(|code| srgb::decode_u8(code) as f32));
            }
        }
    }
    for lightness in [0.25, 0.55, 0.85] {
        for step in 0..24 {
            let angle = f64::from(step) * 15.0_f64.to_radians();
            let rgb = oklab::from_lab_f64([lightness, 0.1 * angle.cos(), 0.1 * angle.sin()]);
            pixels.push(rgb.map(|value| value as f32));
        }
    }
    for code in [1u8, 3, 8, 20] {
        let base = srgb::decode_u8(code) as f32;
        pixels.push([base * 1.1, base, base * 0.9]);
    }
    pixels.extend([
        [1.5, 1.2, 0.9],
        [2.0, 2.0, 2.0],
        [-0.02, 0.01, 0.03],
        [1.2, -0.05, 0.4],
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
    ]);
    pixels
}

/// The parameter sets every buffer check runs: each wheel alone, luminance alone, opposing tints,
/// overlap and balance extremes, Global with odd Blending and Balance, a seam hue and everything
/// at once.
fn parameter_sets() -> Vec<(&'static str, Grading)> {
    let all = |blending, balance| {
        grading(
            [
                wheel(200.0, 40.0, -30.0),
                wheel(35.0, 25.0, 15.0),
                wheel(55.0, 60.0, -45.0),
                wheel(300.0, 10.0, 20.0),
            ],
            blending,
            balance,
        )
    };
    vec![
        ("shadows-teal", only(SHADOWS, wheel(190.0, 80.0, 0.0))),
        ("midtones-red", only(MIDTONES, wheel(0.0, 100.0, 0.0))),
        ("highlights-gold", only(HIGHLIGHTS, wheel(45.0, 70.0, 0.0))),
        ("global-blue", only(GLOBAL, wheel(240.0, 50.0, 0.0))),
        ("seam-360", only(MIDTONES, wheel(360.0, 100.0, 0.0))),
        (
            "shadows-luminance-up",
            only(SHADOWS, wheel(0.0, 0.0, 100.0)),
        ),
        (
            "shadows-luminance-down",
            only(SHADOWS, wheel(0.0, 0.0, -100.0)),
        ),
        (
            "highlights-luminance-down",
            only(HIGHLIGHTS, wheel(0.0, 0.0, -100.0)),
        ),
        ("global-luminance-up", only(GLOBAL, wheel(0.0, 0.0, 100.0))),
        (
            "opposing-split",
            grading(
                [
                    wheel(20.0, 100.0, 0.0),
                    Wheel::default(),
                    wheel(200.0, 100.0, 0.0),
                    Wheel::default(),
                ],
                100.0,
                0.0,
            ),
        ),
        ("everything-sharp-shadows", all(0.0, -100.0)),
        ("everything-wide-highlights", all(100.0, 100.0)),
        ("everything-default", all(DEFAULT_BLENDING, 0.0)),
    ]
}

/// The unit against the reference over the whole buffer under every set, in linear light,
/// within `1e-5 + 1e-5 * |reference|`: the same bound the HSL unit is held to, covering only the
/// unit's `f32` arithmetic.
#[test]
fn whole_buffers_match_the_documented_equations() {
    let pixels = buffer();
    let mut worst = (0.0f64, String::new());
    for (name, set) in parameter_sets() {
        assert!(set.is_active(), "{name}");
        assert!(params(&set).is_active(), "{name}");
        let unit = Grade::new(set);
        assert!(unit.is_finite(), "{name}");
        let mut produced = pixels.clone();
        unit.apply_row(0, 0, &mut produced);
        for (index, (input, output)) in pixels.iter().zip(&produced).enumerate() {
            let expected = reference(input.map(f64::from), &set);
            for channel in 0..3 {
                let deviation = (f64::from(output[channel]) - expected[channel]).abs();
                let bound = 1e-5 + 1e-5 * expected[channel].abs();
                assert!(
                    deviation <= bound,
                    "{name}, pixel {index} {input:?} channel {channel}: {} against {}",
                    output[channel],
                    expected[channel]
                );
                if deviation > worst.0 {
                    worst = (deviation, format!("{name} pixel {index} channel {channel}"));
                }
            }
        }
    }
    println!("maximum observed deviation {} at {}", worst.0, worst.1);
}

// -------------------------------------------------------------------------------------------
// Neutrality and dormant settings
// -------------------------------------------------------------------------------------------

/// Hue, Blending and Balance change no pixel on their own, so a grading holding only them is not
/// active; any saturation or luminance amount is.
#[test]
fn only_saturation_and_luminance_activate_the_unit() {
    assert!(!Grading::default().is_active());
    let dormant = grading(
        [
            wheel(120.0, 0.0, 0.0),
            wheel(360.0, 0.0, 0.0),
            wheel(10.0, 0.0, 0.0),
            wheel(200.0, 0.0, 0.0),
        ],
        0.0,
        -100.0,
    );
    assert!(!dormant.is_active());
    for index in 0..WHEEL_COUNT {
        assert!(only(index, wheel(0.0, 1.0, 0.0)).is_active());
        assert!(only(index, wheel(0.0, 0.0, -1.0)).is_active());
    }
}

/// With every tint zero the pixel's own `(a, b)` passes through untouched, so luminance alone adds
/// no colour: the output's Oklab `(a, b)` is the input's, and a grey stays one output code in all
/// three channels. A unit holding only dormant settings, which the module never compiles, would
/// still return exactly the Oklab round trip.
#[test]
fn luminance_alone_adds_no_colour() {
    for code in 0u8..=255 {
        let input = grey(code);
        let before = oklab::to_oklab(input);
        for set in [
            only(SHADOWS, wheel(90.0, 0.0, 100.0)),
            only(HIGHLIGHTS, wheel(270.0, 0.0, -100.0)),
            only(GLOBAL, wheel(0.0, 0.0, 60.0)),
        ] {
            let output = apply(input, set);
            let after = oklab::to_oklab(output);
            assert!(
                (after.a - before.a).abs() < 1e-6 && (after.b - before.b).abs() < 1e-6,
                "grey {code}: ({}, {}) became ({}, {})",
                before.a,
                before.b,
                after.a,
                after.b
            );
            let [r, g, b] = crate::colour::srgb::quantize_pixel(output);
            assert!(r == g && g == b, "grey {code}: {:?}", [r, g, b]);
        }
    }
    let dormant = grading([wheel(120.0, 0.0, 0.0); WHEEL_COUNT], 0.0, 100.0);
    for pixel in buffer() {
        assert_eq!(
            apply(pixel, dormant),
            oklab::from_oklab(oklab::to_oklab(pixel)),
            "{pixel:?}"
        );
    }
}

/// Black stays exactly black and white keeps its codes under every tint at full saturation, and
/// under luminance that does not move the endpoints; lifting black lets the shadow tint reach it.
#[test]
fn black_and_white_are_endpoints_until_luminance_moves_them() {
    for hue in [0.0, 120.0, 240.0, 300.0] {
        let tints = grading([wheel(hue, 100.0, 0.0); WHEEL_COUNT], 100.0, 0.0);
        assert_eq!(apply([0.0; 3], tints), [0.0; 3], "hue {hue}");
        assert_eq!(
            crate::colour::srgb::quantize_pixel(apply([1.0; 3], tints)),
            [255; 3],
            "hue {hue}"
        );
    }
    let lifted = apply([0.0; 3], only(SHADOWS, wheel(0.0, 100.0, 100.0)));
    let lifted_lab = lab(lifted);
    assert!(
        (lifted_lab[0] - SHADOW_LIFT).abs() < 0.01,
        "black lifted to {lifted_lab:?}"
    );
    assert!(
        lifted_lab[1].hypot(lifted_lab[2]) > 0.01,
        "the lifted black takes the shadow tint: {lifted_lab:?}"
    );
    assert!(lifted[0] > lifted[2], "toward red: {lifted:?}");
    let dimmed = lab(apply([1.0; 3], only(HIGHLIGHTS, wheel(0.0, 0.0, -100.0))));
    assert!(
        (dimmed[0] - (1.0 - HIGHLIGHT_DIM)).abs() < 1e-4,
        "white dimmed to {dimmed:?}"
    );
}

// -------------------------------------------------------------------------------------------
// Hue
// -------------------------------------------------------------------------------------------

/// The RGB wheel's primaries tint a mid grey toward the Oklab hue of the sRGB primary itself, and
/// 0 and 360 are the same direction bit for bit.
#[test]
fn wheel_hues_follow_the_rgb_colour_wheel() {
    let mid = grey(128);
    for (hue, primary) in [
        (0.0, [1.0, 0.0, 0.0]),
        (120.0, [0.0, 1.0, 0.0]),
        (240.0, [0.0, 0.0, 1.0]),
        (60.0, [1.0, 1.0, 0.0]),
        (180.0, [0.0, 1.0, 1.0]),
        (300.0, [1.0, 0.0, 1.0]),
    ] {
        let tinted = lab(apply(mid, only(MIDTONES, wheel(hue, 100.0, 0.0))));
        let expected = hue_deg(oklab::lab_f64(primary));
        let difference = (hue_deg(tinted) - expected + 540.0).rem_euclid(360.0) - 180.0;
        assert!(
            difference.abs() < 0.5,
            "wheel {hue}: Oklab hue {} against the primary's {expected}",
            hue_deg(tinted)
        );
    }
    let zero = Grade::new(only(MIDTONES, wheel(0.0, 70.0, 0.0)));
    let full = Grade::new(only(MIDTONES, wheel(360.0, 70.0, 0.0)));
    assert_eq!(zero.coefficients, full.coefficients);
}

/// The tint direction is continuous around the wheel, across the 360/0 seam included: no step of
/// 0.1 degree turns it by more than a degree.
#[test]
fn the_tint_direction_is_continuous_across_the_seam() {
    let mut previous = tint_direction(359.9);
    for step in 0..=3600 {
        let hue = f64::from(step) / 10.0;
        let direction = tint_direction(hue);
        let turn = (direction[1].atan2(direction[0]) - previous[1].atan2(previous[0]))
            .to_degrees()
            .rem_euclid(360.0);
        let turn = if turn > 180.0 { 360.0 - turn } else { turn };
        assert!(turn < 1.0, "{hue}: turned {turn} degrees");
        assert!((direction[0].hypot(direction[1]) - 1.0).abs() < 1e-12);
        previous = direction;
    }
}

// -------------------------------------------------------------------------------------------
// Tonal selection
// -------------------------------------------------------------------------------------------

/// The weights are non-negative, sum to one and move continuously for every Blending and Balance,
/// the narrowest Blending included.
#[test]
fn tonal_weights_are_a_smooth_partition_of_unity() {
    for blending in [0.0, 25.0, 50.0, 100.0] {
        for balance in [-100.0, -40.0, 0.0, 70.0, 100.0] {
            let coefficients =
                Coefficients::new(&grading([Wheel::default(); 4], blending, balance));
            for inverse in [
                coefficients.tint_inverse_width,
                coefficients.luminance_inverse_width,
            ] {
                let mut previous = coefficients.weights(0.0, inverse);
                for step in 0..=10_000 {
                    let t = step as f32 / 10_000.0;
                    let weights = coefficients.weights(t, inverse);
                    assert!(weights.iter().all(|w| *w >= 0.0), "{weights:?} at {t}");
                    assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-6);
                    for (now, before) in weights.iter().zip(previous) {
                        assert!(
                            (now - before).abs() < 2e-3,
                            "blending {blending}, balance {balance}: a step at {t}"
                        );
                    }
                    previous = weights;
                }
            }
        }
    }
}

/// Black is all shadow and white all highlight at the default Blending; Blending widens the
/// overlap, and Balance moves the split: negative extends Shadows, positive Highlights.
#[test]
fn blending_widens_and_balance_moves_the_ranges() {
    let weights = |blending, balance, t| {
        let coefficients = Coefficients::new(&grading([Wheel::default(); 4], blending, balance));
        coefficients.weights(t, coefficients.tint_inverse_width)
    };
    assert_eq!(weights(0.0, 0.0, 0.0), [1.0, 0.0, 0.0]);
    assert_eq!(weights(0.0, 0.0, 1.0), [0.0, 0.0, 1.0]);
    assert_eq!(weights(0.0, 0.0, 0.5), [0.0, 1.0, 0.0]);
    assert!(weights(100.0, 0.0, 0.5)[1] < weights(50.0, 0.0, 0.5)[1]);
    assert!(weights(50.0, 0.0, 0.5)[1] < weights(0.0, 0.0, 0.5)[1]);
    assert!(weights(50.0, -100.0, 0.45)[0] > weights(50.0, 0.0, 0.45)[0]);
    assert!(weights(50.0, 100.0, 0.55)[2] > weights(50.0, 0.0, 0.55)[2]);
}

/// Opposing tints colour opposite ends of the scale: a red shadow tint and a cyan highlight tint
/// make a dark grey red and a light grey cyan.
#[test]
fn opposing_tints_colour_opposite_ends() {
    let split = grading(
        [
            wheel(0.0, 100.0, 0.0),
            Wheel::default(),
            wheel(180.0, 100.0, 0.0),
            Wheel::default(),
        ],
        DEFAULT_BLENDING,
        0.0,
    );
    let dark = apply(grey(40), split);
    let light = apply(grey(215), split);
    assert!(dark[0] > dark[1] && dark[0] > dark[2], "{dark:?}");
    assert!(light[2] > light[0] && light[1] > light[0], "{light:?}");
}

/// Global reads neither Blending nor Balance: a Global-only grade is bit for bit the same at every
/// Blending and Balance.
#[test]
fn global_ignores_blending_and_balance() {
    let pixels = buffer();
    let global = wheel(77.0, 55.0, -35.0);
    let mut expected = pixels.clone();
    Grade::new(only(GLOBAL, global)).apply_row(0, 0, &mut expected);
    for blending in [0.0, 13.0, 100.0] {
        for balance in [-100.0, 27.0, 100.0] {
            let mut wheels = [Wheel::default(); WHEEL_COUNT];
            wheels[GLOBAL] = global;
            let mut produced = pixels.clone();
            Grade::new(grading(wheels, blending, balance)).apply_row(0, 0, &mut produced);
            assert_eq!(produced, expected, "blending {blending}, balance {balance}");
        }
    }
}

/// The lightness response is non-decreasing over the whole scale for every combination of the
/// four luminance amounts at -100, 0 and +100, at the narrowest and widest Blending and both
/// Balance extremes, sampled in `f32` exactly as the pixel path evaluates it.
#[test]
fn grade_luminance_is_monotone_for_every_extreme_combination() {
    for combination in 0..3usize.pow(4) {
        let mut code = combination;
        let amounts: [f64; 4] = std::array::from_fn(|_| {
            let level = code % 3;
            code /= 3;
            [-100.0, 0.0, 100.0][level]
        });
        for blending in [0.0, 50.0, 100.0] {
            for balance in [-100.0, 0.0, 100.0] {
                let set = grading(amounts.map(|l| wheel(0.0, 0.0, l)), blending, balance);
                let mut previous = f32::NEG_INFINITY;
                for step in 0..=4000 {
                    let l = step as f32 / 4000.0;
                    let input = oklab::from_lab_f64([f64::from(l), 0.0, 0.0]).map(|v| v as f32);
                    let output = oklab::to_oklab(apply(input, set)).l;
                    assert!(
                        output >= previous - 1e-6,
                        "{amounts:?}, blending {blending}, balance {balance}: {output} after \
                         {previous} at {l}"
                    );
                    previous = output;
                }
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Extended input
// -------------------------------------------------------------------------------------------

/// Signed and above-white input: every output is finite, the part outside `[0, 1]` is carried
/// through rather than truncated, and the response is continuous across white and black.
#[test]
fn extended_input_is_finite_continuous_and_keeps_its_headroom() {
    let everything = parameter_sets().pop().expect("a set").1;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    for _ in 0..50_000 {
        let pixel = [4.0 * next() - 1.0, 4.0 * next() - 1.0, 4.0 * next() - 1.0];
        let output = apply(pixel, everything);
        assert!(
            output.iter().all(|v| v.is_finite()),
            "{pixel:?} -> {output:?}"
        );
    }
    let bright = apply([3.0, 3.0, 3.0], only(HIGHLIGHTS, wheel(30.0, 100.0, 100.0)));
    assert!(
        (lab(bright)[0] - lab([3.0; 3])[0]).abs() < 1e-5,
        "headroom is carried: {bright:?}"
    );
    for set in [everything, only(SHADOWS, wheel(0.0, 100.0, 100.0))] {
        for edge in [0.0f32, 1.0] {
            let below = apply(
                oklab::from_lab_f64([f64::from(edge) - 1e-4, 0.0, 0.0]).map(|v| v as f32),
                set,
            );
            let above = apply(
                oklab::from_lab_f64([f64::from(edge) + 1e-4, 0.0, 0.0]).map(|v| v as f32),
                set,
            );
            for channel in 0..3 {
                assert!(
                    (below[channel] - above[channel]).abs() < 2e-3,
                    "a step at {edge}: {below:?} and {above:?}"
                );
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Description, identity and the GPU words
// -------------------------------------------------------------------------------------------

/// The description names every field away from its default; the identity is the coefficient pack,
/// so a dormant hue that changes no pixel does not change it.
#[test]
fn describe_names_moved_fields_and_identity_follows_the_pixels() {
    let unit = Grade::new(only(MIDTONES, wheel(30.0, 40.0, -5.0)));
    assert_eq!(
        unit.describe(),
        "grade(midtones-hue:+30, midtones-saturation:+40, midtones-luminance:-5)"
    );
    let mut dormant = only(MIDTONES, wheel(30.0, 40.0, -5.0));
    dormant.wheels[SHADOWS].hue = 200.0;
    assert_eq!(Grade::new(dormant).identity(), unit.identity());
    let mut changed = dormant;
    changed.blending = 10.0;
    assert_ne!(Grade::new(changed).identity(), unit.identity());
    assert_eq!(Grade::new(Grading::default()).describe(), "grade(neutral)");
}

/// The words are the unit's whole state, two units that describe themselves alike carry identical
/// words, and the program reads them in the order the CPU packs them.
#[test]
fn gpu_uniforms_follow_the_description() {
    let mut sets: Vec<Grading> = parameter_sets().into_iter().map(|(_, set)| set).collect();
    sets.push(only(SHADOWS, wheel(0.0, -0.0, 0.0)));
    sets.push(only(GLOBAL, wheel(360.0, 50.0, -0.0)));
    let first: Vec<Grade> = sets.iter().copied().map(Grade::new).collect();
    let second: Vec<Grade> = sets.iter().copied().map(Grade::new).collect();
    let units: Vec<&dyn PointwiseColor> = first
        .iter()
        .chain(&second)
        .map(|unit| unit as &dyn PointwiseColor)
        .collect();
    crate::render::gpu::testing::assert_uniforms_follow_descriptions(&units);
    for unit in &first {
        let words = unit.gpu().expect("the grading unit has a program").words;
        assert_eq!(words.len(), WORDS);
        let coefficients = unit.coefficients;
        assert_eq!(f32::from_bits(words[0]), coefficients.boundaries[0]);
        assert_eq!(
            f32::from_bits(words[3]),
            coefficients.luminance_inverse_width
        );
        assert_eq!(
            f32::from_bits(words[8]),
            coefficients.tints[HIGHLIGHTS][0] + 0.0
        );
        assert_eq!(
            f32::from_bits(words[15]),
            coefficients.luminance[GLOBAL] + 0.0
        );
        assert_eq!(f32::from_bits(words[17]), coefficients.dim);
    }
    let constant = |name: &str| crate::render::gpu::testing::wgsl_constant(&PROGRAM, name);
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    for (name, matrix) in crate::colour::oklab::MATRICES {
        for (row, values) in matrix.iter().enumerate() {
            assert_eq!(
                bits(&constant(&format!("lf_mixer_grade_{name}_{row}"))),
                bits(values),
                "{name} row {row}"
            );
        }
    }
}

/// Production against every case of the frozen fixture the reference study generates
/// (`fixtures/mixer/grade-cases.json`), in linear light within `1e-5 + 1e-5 * |reference|`.
#[test]
fn production_matches_every_frozen_fixture_case() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/mixer/grade-cases.json");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the grading fixtures"))
            .expect("valid fixture JSON");
    assert_eq!(file["wheel_order"], serde_json::json!(WHEEL_NAMES));
    let sets = file["parameter_sets"].as_array().expect("the sets");
    let cases = file["cases"].as_array().expect("the cases");
    assert!(cases.len() >= 300, "{} cases", cases.len());
    let number = |value: &serde_json::Value| value.as_f64().expect("a number");
    let mut worst = 0.0f64;
    for case in cases {
        let name = case["name"].as_str().expect("a name");
        let set = sets
            .iter()
            .find(|set| set["name"] == case["parameters"])
            .expect("the named set");
        let wheels = set["wheels"].as_array().expect("four wheels");
        let grading = grading(
            std::array::from_fn(|index| {
                let fields = wheels[index].as_array().expect("three fields");
                wheel(number(&fields[0]), number(&fields[1]), number(&fields[2]))
            }),
            number(&set["blending"]),
            number(&set["balance"]),
        );
        let input: [f32; 3] = std::array::from_fn(|channel| number(&case["input"][channel]) as f32);
        let produced = apply(input, grading);
        for channel in 0..3 {
            let reference = number(&case["expected_linear"][channel]);
            let deviation = (f64::from(produced[channel]) - reference).abs();
            assert!(
                deviation <= 1e-5 + 1e-5 * reference.abs(),
                "{name} channel {channel}: {} against {reference}",
                produced[channel]
            );
            worst = worst.max(deviation);
        }
    }
    println!("maximum observed deviation {worst}");
}
