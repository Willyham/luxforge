//! The look unit against the frozen `f64` reference fixture (`fixtures/look/look-cases.json`, from
//! `crates/luxforge-reference/src/look.rs`) at every step, its properties, its description and its
//! GPU program's uniforms.
use super::*;
use crate::modules::look::standard::{STANDARD_CHROMA, STANDARD_KNEE, STANDARD_KNOTS};
use serde_json::Value;
use std::{collections::HashMap, fs, path::PathBuf};

/// The colour tolerance every frozen pointwise fixture uses: `1e-5 + 1e-5 · |reference|` in linear
/// float.
fn tolerance(reference: f64) -> f64 {
    1e-5 + 1e-5 * reference.abs()
}

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/look/look-cases.json");
    serde_json::from_str(&fs::read_to_string(&path).expect("the look fixture"))
        .expect("valid fixture JSON")
}

fn knots_of(value: &Value) -> Vec<[f64; 2]> {
    value
        .as_array()
        .expect("a knot list")
        .iter()
        .map(|knot| [knot[0].as_f64().unwrap(), knot[1].as_f64().unwrap()])
        .collect()
}

fn triple(value: &Value) -> [f64; 3] {
    std::array::from_fn(|channel| value[channel].as_f64().expect("a value"))
}

fn unit(knots: &[[f64; 2]], chroma: f64, knee: f64, amount: f64) -> Look {
    Look::new(LookParameters {
        knots,
        chroma,
        knee,
        amount,
    })
}

fn standard(amount: f64) -> Look {
    unit(&STANDARD_KNOTS, STANDARD_CHROMA, STANDARD_KNEE, amount)
}

fn apply(unit: &Look, rgb: [f32; 3]) -> [f32; 3] {
    let mut row = [rgb];
    unit.apply_row(0, 0, &mut row);
    row[0]
}

/// The greatest deviation of one step: absolute, in tolerances, and where.
#[derive(Default)]
struct Worst {
    deviation: f64,
    tolerances: f64,
    at: String,
}

impl Worst {
    fn check(&mut self, step: &str, case: &str, produced: [f32; 3], expected: [f64; 3]) {
        for channel in 0..3 {
            let value = f64::from(produced[channel]);
            let reference = expected[channel];
            let deviation = (value - reference).abs();
            assert!(
                deviation <= tolerance(reference),
                "{case} {step} channel {channel}: production {value} against {reference}, \
                 deviation {deviation:e} over {:e}",
                tolerance(reference)
            );
            if deviation / tolerance(reference) > self.tolerances {
                *self = Self {
                    deviation,
                    tolerances: deviation / tolerance(reference),
                    at: format!("{case} channel {channel}"),
                };
            }
        }
    }
}

/// Production against every frozen case at every step, chained through the unit's own steps as
/// `apply_row` runs them: the tone step from the case's input, the chroma step from production's
/// tone, the path to white from production's chroma, and the whole unit's output, each within the
/// colour tolerance of the reference's value at that step. The greatest deviation of each step is
/// printed (`--nocapture`) for the handoff.
#[test]
fn production_matches_the_frozen_look_fixture_at_every_step() {
    let file = fixture();
    let looks: HashMap<&str, Look> = file["looks"]
        .as_array()
        .expect("the looks")
        .iter()
        .map(|look| {
            (
                look["name"].as_str().expect("a name"),
                unit(
                    &knots_of(&look["knots"]),
                    look["chroma"].as_f64().unwrap(),
                    look["knee"].as_f64().unwrap(),
                    look["amount"].as_f64().unwrap(),
                ),
            )
        })
        .collect();
    assert_eq!(looks.len(), 8, "every frozen look is checked");
    let cases = file["cases"].as_array().expect("the cases");
    assert_eq!(cases.len(), 496, "every frozen case is checked");
    let mut worst: [Worst; 4] = Default::default();
    for (index, case) in cases.iter().enumerate() {
        let name = case["look"].as_str().expect("a look name");
        let look = &looks[name];
        let label = format!("case {index} ({name})");
        let input = triple(&case["input"]);
        let rgb = input.map(|channel| channel as f32);
        assert_eq!(rgb.map(f64::from), input, "{label}: the input is f32-exact");
        assert!(look.is_finite(), "{name}");
        let toned = look.tone(rgb);
        worst[0].check("tone", &label, toned, triple(&case["tone"]));
        let coloured = look.chroma(toned);
        worst[1].check("chroma", &label, coloured, triple(&case["chroma"]));
        let white = look.white(coloured);
        worst[2].check("white", &label, white, triple(&case["white"]));
        let output = apply(look, rgb);
        assert_eq!(
            output,
            look.blend(rgb, white),
            "{label}: the steps are the unit"
        );
        worst[3].check("output", &label, output, triple(&case["output"]));
    }
    for (step, worst) in ["tone", "chroma", "white", "output"].iter().zip(&worst) {
        println!(
            "look {step}: largest deviation {:e} ({:.3} tolerances) at {}",
            worst.deviation, worst.tolerances, worst.at
        );
    }
}

/// A dense grey ramp over `[-0.5, x_max + 0.5]` in encoded luminance, decoded to linear `f32`.
fn grey_ramp() -> Vec<f32> {
    let top = STANDARD_KNOTS[STANDARD_KNOTS.len() - 1][0] + 0.5;
    (0..=20_000)
        .map(|k| srgb::decode(-0.5 + (top + 0.5) * f64::from(k) / 20_000.0) as f32)
        .collect()
}

/// Greys stay grey and the tone stays monotone on an extended ramp, negative and above-white
/// luminance included, at every amount up to 100; every output is finite, extreme inputs included.
#[test]
fn greys_stay_grey_monotone_and_finite() {
    for amount in [35.0, 100.0] {
        let look = standard(amount);
        let mut previous: Option<f32> = None;
        for grey in grey_ramp() {
            let out = apply(&look, [grey; 3]);
            assert!(out.iter().all(|c| c.is_finite()), "{grey} -> {out:?}");
            // The tone step keeps a grey's channels equal exactly; the chroma gain scales the few
            // ulps of `a` and `b` the f32 Oklab round trip leaves on a grey, which stay within the
            // colour tolerance (2.2e-6 relative at worst on this ramp, far below a code).
            let spread = out[0].max(out[1]).max(out[2]) - out[0].min(out[1]).min(out[2]);
            assert!(
                f64::from(spread) <= 1e-5 * f64::from(out[0].abs()).max(1e-3),
                "grey {grey} -> {out:?}"
            );
            if let Some(before) = previous {
                assert!(
                    out[0] >= before - 8.0 * f32::EPSILON * before.abs().max(f32::MIN_POSITIVE),
                    "{grey} maps to {} below the previous grey's {before}",
                    out[0]
                );
            }
            previous = Some(out[0]);
        }
    }
    let look = standard(180.0);
    for rgb in [
        [65_504.0f32, 0.0, 0.0],
        [-65_504.0, 1.0, 2.0],
        [1e-30, -1e-30, 0.0],
        [3.0, 3.0, -2.0],
        [0.0, 0.0, 0.0],
    ] {
        let out = apply(&look, rgb);
        assert!(out.iter().all(|c| c.is_finite()), "{rgb:?} -> {out:?}");
    }
}

/// The path to white keeps luminance, never grows chroma, leaves a colour at or below the knee
/// untouched and keeps every channel of a colour past it below 1.
#[test]
fn the_path_to_white_keeps_luminance_and_rolls_off_below_white() {
    let look = standard(100.0);
    let below = [0.7f32, 0.3, 0.1];
    assert_eq!(look.white(below), below);
    for rgb in [
        [1.6f32, 0.4, 0.2],
        [0.9, 0.85, 0.1],
        [3.0, 0.0, 0.5],
        [0.2, 0.3, 1.4],
    ] {
        let out = look.white(rgb);
        let (y_in, y_out) = (luma::rec709(rgb), luma::rec709(out));
        assert!((y_in - y_out).abs() <= 1e-6, "{rgb:?}: {y_in} -> {y_out}");
        assert!(out.iter().all(|&c| c < 1.0), "{rgb:?} -> {out:?}");
        let spread = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        assert!(spread(out) <= spread(rgb), "{rgb:?} -> {out:?}");
    }
}

/// Amount 0, which only the GPU shape compiles, is the identity bit for bit; amount 100 is the
/// look's own output and past 100 extrapolates from the input through it.
#[test]
fn amount_zero_is_the_identity_and_amount_blends_with_the_input() {
    let zero = standard(0.0);
    let full = standard(100.0);
    let more = standard(200.0);
    for rgb in [[0.3f32, 0.1, 0.05], [1.5, 1.2, 0.9], [-0.1, 0.0, 0.2]] {
        assert_eq!(
            apply(&zero, rgb).map(f32::to_bits),
            rgb.map(f32::to_bits),
            "{rgb:?}"
        );
        let look = apply(&full, rgb);
        let doubled = apply(&more, rgb);
        for c in 0..3 {
            let expected = rgb[c] + 2.0 * (look[c] - rgb[c]);
            assert!((doubled[c] - expected).abs() <= 1e-6, "{rgb:?}");
        }
    }
}

/// The description names the effect and writes every value exactly; two units built apart that
/// describe themselves alike carry identical uniforms and blocks, and the words and block hold the
/// `f32` values the CPU unit evaluates, laid out as the Tone curve's.
#[test]
fn the_unit_describes_itself_exactly_and_its_uniforms_follow() {
    let third = 1.0 / 3.0;
    let described = unit(
        &[[0.0, 0.0], [0.1 + 0.2, third], [1.5, 1.0]],
        1.2,
        0.8,
        80.0,
    );
    assert_eq!(
        described.describe(),
        "luxforge.look.look(tone [[0, 0], [0.30000000000000004, 0.3333333333333333], [1.5, 1]], \
         chroma 1.2, knee 0.8, amount 80)"
    );
    let sets: Vec<(Vec<[f64; 2]>, f64, f64, f64)> = vec![
        (
            STANDARD_KNOTS.to_vec(),
            STANDARD_CHROMA,
            STANDARD_KNEE,
            100.0,
        ),
        (
            STANDARD_KNOTS.to_vec(),
            STANDARD_CHROMA,
            STANDARD_KNEE,
            35.0,
        ),
        (vec![[0.0, 0.1], [0.6, 0.5], [1.2, 0.9]], 0.75, 0.9, 100.0),
        (vec![[0.0, 0.0], [1.0, 1.0]], 1.0, 0.95, 180.0),
    ];
    let build = || -> Vec<Look> {
        sets.iter()
            .map(|(knots, chroma, knee, amount)| unit(knots, *chroma, *knee, *amount))
            .collect()
    };
    let (first, second) = (build(), build());
    let units: Vec<&dyn PointwiseColor> = first
        .iter()
        .chain(&second)
        .map(|unit| unit as &dyn PointwiseColor)
        .collect();
    crate::render::gpu::testing::assert_uniforms_follow_descriptions(&units);
    for look in &first {
        let description = look.gpu().expect("the look has a program");
        let n = look.x32.len();
        assert_eq!(
            description.words,
            vec![
                n as u32,
                look.first.to_bits(),
                look.last.to_bits(),
                look.floor.to_bits(),
                look.slope.to_bits(),
                look.chroma.to_bits(),
                look.knee.to_bits(),
                look.amount.to_bits(),
            ]
        );
        let block = description.block.expect("the knots are a block");
        assert_eq!(block.len(), n + (n - 1) + 4 * (n - 1));
        assert_eq!(f32::from_bits(block[n - 1]), look.x32[n - 1]);
        assert_eq!(f32::from_bits(block[n]), look.inv_h[0]);
        assert_eq!(f32::from_bits(block[2 * n - 1]), look.coefficients[0][0]);
        assert_eq!(
            f32::from_bits(block[block.len() - 1]),
            look.coefficients[n - 2][3]
        );
    }
}

/// The program restates the core's Oklab matrices as the same `f32` values.
#[test]
fn the_program_restates_the_cores_oklab_matrices() {
    let constant = |name: &str| {
        crate::render::gpu::testing::wgsl_constant(&PROGRAM, &format!("lf_look_look_{name}"))
    };
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    for (name, matrix) in crate::colour::oklab::MATRICES {
        for (row, values) in matrix.iter().enumerate() {
            assert_eq!(
                bits(&constant(&format!("{name}_{row}"))),
                bits(values),
                "{name} row {row}"
            );
        }
    }
    assert_eq!(
        bits(&constant("near_black")),
        bits(&[crate::colour::luma::NEAR_BLACK])
    );
}
