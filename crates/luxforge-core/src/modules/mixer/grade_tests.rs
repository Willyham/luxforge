//! The grading stage's correctness checks, through the mixer's colour unit as the host runs it:
//! production against the `f64` reference of the design's initial equations
//! (`luxforge_reference::grade`, whose study proves their properties) on the frozen fixture and on
//! every per-compile path, and what is particular to the `f32` unit: exact endpoints, dormant
//! settings and the round trip, bit-identical Global independence, monotone luminance in `f32`,
//! finite extended input, the coefficient pack and the identity.
use super::*;
use crate::{
    colour::srgb,
    modules::{
        PointwiseColor,
        mixer::unit::{self, Hsl, Mixer},
    },
};
use luxforge_reference::grade as reference;

fn wheel(hue: f64, saturation: f64, luminance: f64) -> WheelValues {
    WheelValues {
        hue,
        saturation,
        luminance,
    }
}

fn grading(wheels: [WheelValues; WHEEL_COUNT], blending: f64, balance: f64) -> Grading {
    Grading {
        wheels,
        blending,
        balance,
    }
}

/// One wheel set, the others neutral, at the default Blending and Balance.
fn only(index: usize, set: WheelValues) -> Grading {
    let mut wheels = [WheelValues::default(); WHEEL_COUNT];
    wheels[index] = set;
    grading(wheels, DEFAULT_BLENDING, 0.0)
}

/// Every wheel moved at once.
fn everything(blending: f64, balance: f64) -> Grading {
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
}

const SHADOWS: usize = 0;
const MIDTONES: usize = 1;
const HIGHLIGHTS: usize = 2;
const GLOBAL: usize = 3;

/// The mixer's colour unit with this grading and no HSL stage.
fn graded(grading: Grading) -> Mixer {
    Mixer::new(None, Some(Grade::new(grading)))
}

fn apply(rgb: [f32; 3], grading: Grading) -> [f32; 3] {
    let mut row = [rgb];
    graded(grading).apply_row(0, 0, &mut row);
    row[0]
}

fn lab(rgb: [f32; 3]) -> [f64; 3] {
    oklab::lab_f64(rgb.map(f64::from))
}

fn grey(code: u8) -> [f32; 3] {
    [srgb::decode_u8(code) as f32; 3]
}

/// A grey ramp, a hue wheel at three lightnesses, and signed and above-white linear values.
fn buffer() -> Vec<[f32; 3]> {
    let mut pixels: Vec<[f32; 3]> = (0u8..=255).step_by(5).map(grey).collect();
    for lightness in [0.25, 0.55, 0.85] {
        for step in 0..24 {
            let angle = f64::from(step) * 15.0_f64.to_radians();
            let rgb = oklab::from_lab_f64([lightness, 0.1 * angle.cos(), 0.1 * angle.sin()]);
            pixels.push(rgb.map(|value| value as f32));
        }
    }
    pixels.extend([
        [1.5, 1.2, 0.9],
        [2.0, 2.0, 2.0],
        [-0.02, 0.01, 0.03],
        [1.2, -0.05, 0.4],
    ]);
    pixels
}

/// The f64 reference's parameters for these fields.
fn params(grading: &Grading) -> reference::GradeParams {
    reference::GradeParams {
        wheels: grading.wheels.map(|wheel| reference::Wheel {
            hue: wheel.hue,
            saturation: wheel.saturation,
            luminance: wheel.luminance,
        }),
        blending: grading.blending,
        balance: grading.balance,
    }
}

// -------------------------------------------------------------------------------------------
// Against the reference
// -------------------------------------------------------------------------------------------

/// The grading constants are the reference's, so the equivalence below compares one set of
/// equations.
#[test]
fn constants_match_the_reference() {
    assert_eq!(WHEEL_COUNT, reference::WHEEL_COUNT);
    assert_eq!(WHEEL_NAMES, reference::WHEEL_NAMES);
    assert_eq!(DEFAULT_BLENDING, reference::GradeParams::default().blending);
    assert_eq!(
        Grading::default().balance,
        reference::GradeParams::default().balance
    );
    for (production, expected) in [
        (SHADOW_BOUNDARY, reference::SHADOW_BOUNDARY),
        (HIGHLIGHT_BOUNDARY, reference::HIGHLIGHT_BOUNDARY),
        (BALANCE_REACH, reference::BALANCE_REACH),
        (WIDTH_AT_NO_BLENDING, reference::WIDTH_AT_NO_BLENDING),
        (WIDTH_AT_FULL_BLENDING, reference::WIDTH_AT_FULL_BLENDING),
        (LUMINANCE_WIDTH_FLOOR, reference::LUMINANCE_WIDTH_FLOOR),
        (LUMINANCE_STRENGTH, reference::LUMINANCE_STRENGTH),
        (SHADOW_LIFT, reference::SHADOW_LIFT),
        (HIGHLIGHT_DIM, reference::HIGHLIGHT_DIM),
        (TINT_CHROMA, reference::TINT_CHROMA),
    ] {
        assert_eq!(production.to_bits(), expected.to_bits());
    }
}

/// The frozen fixture the reference study generates (`fixtures/mixer/grade-cases.json`): its
/// parameter sets and inputs (greys, a hue wheel, skin, sky and foliage, above-white and signed
/// values).
struct Fixture {
    sets: Vec<(String, Grading)>,
    /// Each case's name, its set's index, its input and the reference's linear output.
    cases: Vec<(String, usize, [f32; 3], [f64; 3])>,
}

fn fixture() -> Fixture {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/mixer/grade-cases.json");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the grading fixtures"))
            .expect("valid fixture JSON");
    assert_eq!(file["wheel_order"], serde_json::json!(WHEEL_NAMES));
    let number = |value: &serde_json::Value| value.as_f64().expect("a number");
    let sets: Vec<(String, Grading)> = file["parameter_sets"]
        .as_array()
        .expect("the sets")
        .iter()
        .map(|set| {
            let wheels = set["wheels"].as_array().expect("four wheels");
            let grading = grading(
                std::array::from_fn(|index| {
                    let fields = wheels[index].as_array().expect("three fields");
                    wheel(number(&fields[0]), number(&fields[1]), number(&fields[2]))
                }),
                number(&set["blending"]),
                number(&set["balance"]),
            );
            (set["name"].as_str().expect("a name").to_owned(), grading)
        })
        .collect();
    let cases: Vec<_> = file["cases"]
        .as_array()
        .expect("the cases")
        .iter()
        .map(|case| {
            let set = sets
                .iter()
                .position(|(name, _)| case["parameters"] == name.as_str())
                .expect("the named set");
            (
                case["name"].as_str().expect("a name").to_owned(),
                set,
                std::array::from_fn(|channel| number(&case["input"][channel]) as f32),
                std::array::from_fn(|channel| number(&case["expected_linear"][channel])),
            )
        })
        .collect();
    assert!(cases.len() >= 300, "{} cases", cases.len());
    Fixture { sets, cases }
}

/// The bound production is held to in linear light: the HSL stage's, `1e-5 + 1e-5 * |reference|`,
/// covering only the unit's `f32` arithmetic.
fn assert_within(name: &str, produced: [f32; 3], expected: [f64; 3], worst: &mut f64) {
    for channel in 0..3 {
        let deviation = (f64::from(produced[channel]) - expected[channel]).abs();
        assert!(
            deviation <= 1e-5 + 1e-5 * expected[channel].abs(),
            "{name} channel {channel}: {} against {}",
            produced[channel],
            expected[channel]
        );
        *worst = worst.max(deviation);
    }
}

/// Production against every case of the frozen fixture.
#[test]
fn production_matches_every_frozen_fixture_case() {
    let fixture = fixture();
    let mut worst = 0.0f64;
    for (name, set, input, expected) in &fixture.cases {
        let set = fixture.sets[*set].1;
        assert!(set.is_active(), "{name}");
        assert!(graded(set).is_finite(), "{name}");
        assert_within(name, apply(*input, set), *expected, &mut worst);
    }
    println!("maximum observed deviation {worst}");
}

/// The per-compile paths the fixture's sets leave out — Global folded into the tonal exponent at a
/// shared and at a separate width, Global's luminance alone, luminance without a tint, and every
/// wheel at the default Blending — against the reference on the fixture's inputs. With the
/// fixture's own sets, each flag combination the pack can hold is held to the reference.
#[test]
fn every_per_compile_path_matches_the_reference() {
    let folded = |blending, balance| {
        grading(
            [
                wheel(190.0, 60.0, -50.0),
                wheel(35.0, 25.0, 60.0),
                wheel(55.0, 60.0, 30.0),
                wheel(300.0, 10.0, -40.0),
            ],
            blending,
            balance,
        )
    };
    let sets = [
        ("folded, shared width", folded(DEFAULT_BLENDING, 0.0)),
        ("folded, separate widths", folded(10.0, 30.0)),
        (
            "global luminance alone",
            only(GLOBAL, wheel(0.0, 0.0, 100.0)),
        ),
        (
            "global tint and luminance",
            only(GLOBAL, wheel(120.0, 30.0, -70.0)),
        ),
        (
            "midtones luminance alone",
            only(MIDTONES, wheel(0.0, 0.0, -80.0)),
        ),
        (
            "everything, default Blending",
            everything(DEFAULT_BLENDING, 0.0),
        ),
        ("everything, wide", everything(100.0, 100.0)),
    ];
    let fixture = fixture();
    // The paths taken: the flags, and whether Global's own gamma runs.
    let paths: std::collections::BTreeSet<(u32, bool)> = fixture
        .sets
        .iter()
        .map(|(_, set)| *set)
        .chain(sets.iter().map(|(_, set)| *set))
        .map(|set| {
            let coefficients = Coefficients::new(&set);
            (coefficients.flags, coefficients.global_exponent != 1.0)
        })
        .collect();
    let expected = [
        (0, false),
        (SHARED_WIDTH, false),
        (SHARED_WIDTH, true),
        (TONAL_LUMINANCE, false),
        (TONAL_LUMINANCE | SHARED_WIDTH, false),
        (TONAL_LUMINANCE | AFFINE, true),
        (TONAL_LUMINANCE | SHARED_WIDTH | AFFINE, false),
        (TONAL_LUMINANCE | SHARED_WIDTH | AFFINE, true),
    ];
    assert!(
        expected.iter().all(|path| paths.contains(path)),
        "{paths:?}"
    );
    let inputs: Vec<[f32; 3]> = fixture
        .cases
        .iter()
        .map(|(_, _, input, _)| *input)
        .chain(buffer())
        .collect();
    let mut worst = 0.0f64;
    for (name, set) in sets {
        let parameters = params(&set);
        for input in &inputs {
            let expected = reference::apply(input.map(f64::from), &parameters);
            assert_within(name, apply(*input, set), expected, &mut worst);
        }
    }
    println!("maximum observed deviation {worst}");
}

/// HSL then grading in one round trip against the HSL reference composed with the grading
/// reference, each in `f64`, on the fixture's inputs and sets, within the same bound.
#[test]
fn hsl_then_grading_matches_the_composed_references() {
    let mut hue = [0.0; unit::RANGE_COUNT];
    let mut saturation = [0.0; unit::RANGE_COUNT];
    let mut luminance = [0.0; unit::RANGE_COUNT];
    (hue[0], saturation[1], saturation[3]) = (30.0, -40.0, 40.0);
    (hue[4], luminance[5], saturation[7]) = (-25.0, -30.0, 25.0);
    let mixer = luxforge_reference::mixer::MixerParams {
        hue,
        saturation,
        luminance,
    };
    let fixture = fixture();
    let mut worst = 0.0f64;
    for (name, set) in &fixture.sets {
        let both = Mixer::new(
            Some(Hsl::new(hue, saturation, luminance)),
            Some(Grade::new(*set)),
        );
        let parameters = params(set);
        for (_, _, input, _) in &fixture.cases {
            let mut row = [*input];
            both.apply_row(0, 0, &mut row);
            let mixed = luxforge_reference::mixer::mix(input.map(f64::from), &mixer);
            let expected = reference::apply(mixed, &parameters);
            assert_within(name, row[0], expected, &mut worst);
        }
    }
    println!("maximum observed deviation {worst}");
}

/// The wheel's directions are the reference's, around the whole wheel and across its seam: the
/// production conversion is the same `f64` arithmetic in another order. 0 and 360 give the same
/// coefficients bit for bit.
#[test]
fn the_tint_direction_is_the_references() {
    let mut worst = 0.0f64;
    for step in 0..=3600 {
        let hue = f64::from(step) / 10.0;
        let [a, b] = tint_direction(hue);
        let [ra, rb] = reference::tint_direction(hue);
        worst = worst.max((a - ra).abs()).max((b - rb).abs());
    }
    assert!(worst < 1e-12, "the direction differs by {worst}");
    let zero = Coefficients::new(&only(MIDTONES, wheel(0.0, 70.0, 0.0)));
    let full = Coefficients::new(&only(MIDTONES, wheel(360.0, 70.0, 0.0)));
    assert_eq!(zero, full);
}

// -------------------------------------------------------------------------------------------
// Neutrality, dormant settings and endpoints
// -------------------------------------------------------------------------------------------

/// Hue, Blending and Balance change no pixel on their own, so a grading holding only them is not
/// active; any saturation or luminance amount is.
#[test]
fn only_saturation_and_luminance_activate_the_stage() {
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
/// three channels. A stage holding only dormant settings, which the module never compiles, would
/// still return exactly the Oklab round trip, and a unit with no stage returns its input.
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
            let [r, g, b] = srgb::quantize_pixel(output);
            assert!(r == g && g == b, "grey {code}: {:?}", [r, g, b]);
        }
    }
    let dormant = grading([wheel(120.0, 0.0, 0.0); WHEEL_COUNT], 0.0, 100.0);
    let mut untouched = buffer();
    Mixer::new(None, None).apply_row(0, 0, &mut untouched);
    assert_eq!(untouched, buffer());
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
            srgb::quantize_pixel(apply([1.0; 3], tints)),
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
// Tonal selection and luminance in f32
// -------------------------------------------------------------------------------------------

/// The `f32` weights are non-negative, sum to one and move continuously for every Blending and
/// Balance, the narrowest Blending included, and the luminance weights reuse the tint weights
/// exactly when both widths are the same.
#[test]
fn tonal_weights_are_a_smooth_partition_of_unity() {
    for blending in [0.0, 25.0, 100.0 / 3.0, 50.0, 100.0] {
        for balance in [-100.0, -40.0, 0.0, 70.0, 100.0] {
            let coefficients =
                Coefficients::new(&grading([WheelValues::default(); 4], blending, balance));
            assert_eq!(
                coefficients.flags & SHARED_WIDTH != 0,
                coefficients.luminance_inverse_width == coefficients.tint_inverse_width,
                "blending {blending}"
            );
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
    assert_ne!(
        Coefficients::new(&grading([WheelValues::default(); 4], DEFAULT_BLENDING, 0.0)).flags
            & SHARED_WIDTH,
        0,
        "the default Blending evaluates one set of weights"
    );
}

/// Global reads neither Blending nor Balance: a Global-only grade is bit for bit the same at every
/// Blending and Balance.
#[test]
fn global_ignores_blending_and_balance() {
    let pixels = buffer();
    let global = wheel(77.0, 55.0, -35.0);
    let mut expected = pixels.clone();
    graded(only(GLOBAL, global)).apply_row(0, 0, &mut expected);
    for blending in [0.0, 13.0, 100.0] {
        for balance in [-100.0, 27.0, 100.0] {
            let mut wheels = [WheelValues::default(); WHEEL_COUNT];
            wheels[GLOBAL] = global;
            let mut produced = pixels.clone();
            graded(grading(wheels, blending, balance)).apply_row(0, 0, &mut produced);
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
    let everything = everything(DEFAULT_BLENDING, 0.0);
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
// Description, identity and the pack
// -------------------------------------------------------------------------------------------

/// The description names every field away from its default; the identity is the coefficient pack,
/// so a dormant hue that changes no pixel does not change it.
#[test]
fn describe_names_moved_fields_and_identity_follows_the_pixels() {
    let moved = only(MIDTONES, wheel(30.0, 40.0, -5.0));
    assert_eq!(
        graded(moved).describe(),
        "mixer(grade-midtones-hue:+30, grade-midtones-saturation:+40, \
         grade-midtones-luminance:-5)"
    );
    let mut dormant = moved;
    dormant.wheels[SHADOWS].hue = 200.0;
    assert_eq!(graded(dormant).identity(), graded(moved).identity());
    let mut changed = dormant;
    changed.blending = 10.0;
    assert_ne!(graded(changed).identity(), graded(moved).identity());
}

/// The pack is the stage's whole state in the order the program reads it, the flags last, and the
/// per-compile choices are the documented ones: Global folds into the tonal exponent unless the
/// lift or dim runs, and is packed as its own exponent otherwise.
#[test]
fn the_pack_holds_the_per_compile_choices() {
    for set in [
        everything(DEFAULT_BLENDING, 0.0),
        everything(0.0, -100.0),
        only(GLOBAL, wheel(360.0, 50.0, -0.0)),
        only(SHADOWS, wheel(0.0, -0.0, 0.0)),
    ] {
        let stage = Grade::new(set);
        let words: Vec<u32> = stage.words().collect();
        assert_eq!(words.len(), WORDS);
        let coefficients = stage.coefficients;
        assert_eq!(f32::from_bits(words[0]), coefficients.boundaries[0]);
        assert_eq!(
            f32::from_bits(words[3]),
            coefficients.luminance_inverse_width
        );
        assert_eq!(
            f32::from_bits(words[8]),
            coefficients.tints[HIGHLIGHTS][0] + 0.0
        );
        assert_eq!(f32::from_bits(words[14]), coefficients.luminance[2] + 0.0);
        assert_eq!(f32::from_bits(words[16]), coefficients.global_exponent);
        assert_eq!(f32::from_bits(words[18]), coefficients.scale);
        assert_eq!(words[19], coefficients.flags);
    }
    // Midtones and Global luminance with neither lift nor dim: one gamma, Global folded in.
    let folded = Coefficients::new(&grading(
        [
            WheelValues::default(),
            wheel(0.0, 0.0, 40.0),
            WheelValues::default(),
            wheel(0.0, 0.0, -60.0),
        ],
        DEFAULT_BLENDING,
        0.0,
    ));
    assert_eq!(folded.flags, TONAL_LUMINANCE | SHARED_WIDTH);
    assert_eq!(folded.folded_global, -0.3);
    assert_eq!(folded.global_exponent, 1.0);
    // A lift between the two gammas keeps Global's own, packed as its exponent.
    let lifted = Coefficients::new(&grading(
        [
            wheel(0.0, 0.0, 50.0),
            WheelValues::default(),
            WheelValues::default(),
            wheel(0.0, 0.0, -60.0),
        ],
        0.0,
        0.0,
    ));
    assert_eq!(lifted.flags, TONAL_LUMINANCE | AFFINE);
    assert_eq!(lifted.folded_global, 0.0);
    assert_eq!(lifted.global_exponent, 2f64.powf(0.3) as f32);
    // Global alone forms no tonal exponent.
    let global = Coefficients::new(&only(GLOBAL, wheel(0.0, 0.0, 100.0)));
    assert_eq!(global.flags & (TONAL_LUMINANCE | AFFINE), 0);
    assert_eq!(global.global_exponent, 2f64.powf(-0.5) as f32);
}
