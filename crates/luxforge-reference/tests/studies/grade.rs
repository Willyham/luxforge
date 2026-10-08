//! Property proofs, dense measurements and fixtures for the colour mixer's grading reference in
//! `crates/luxforge-reference/src/grade.rs`.
//!
//! The figures `docs/design/colour-grading.md` quotes are printed by the ignored `grade_figures`
//! study:
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies -- --ignored --nocapture grade_figures
//! ```
//!
//! `fixtures/mixer/grade-cases.json` is committed. It was produced by running the ignored
//! generator once:
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies -- --ignored generate_grade_fixtures
//! ```
//!
//! `grade_fixtures_match_reference` (not ignored) reloads the committed file and recomputes every
//! case, so a silent drift between the file and the equations fails the build.

use luxforge_reference::colour::{self, Oklab};
use luxforge_reference::grade::{
    self, GradeParams, HIGHLIGHT_DIM, LUMINANCE_WIDTH_FLOOR, SHADOW_LIFT, TINT_CHROMA, WHEEL_COUNT,
    WHEEL_NAMES, Wheel,
};
use luxforge_reference::srgb;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const SHADOWS: usize = 0;
const MIDTONES: usize = 1;
const HIGHLIGHTS: usize = 2;
const GLOBAL: usize = 3;

fn wheel(hue: f64, saturation: f64, luminance: f64) -> Wheel {
    Wheel {
        hue,
        saturation,
        luminance,
    }
}

fn only(index: usize, set: Wheel) -> GradeParams {
    let mut params = GradeParams::default();
    params.wheels[index] = set;
    params
}

fn grey_at(l: f64) -> [f64; 3] {
    colour::from_oklab(Oklab { l, a: 0.0, b: 0.0 })
}

fn lab(rgb: [f64; 3]) -> Oklab {
    colour::to_oklab(rgb)
}

fn hue_deg(lab: Oklab) -> f64 {
    lab.b.atan2(lab.a).to_degrees().rem_euclid(360.0)
}

fn circular(a: f64, b: f64) -> f64 {
    ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs()
}

// -------------------------------------------------------------------------------------------
// Tonal selection
// -------------------------------------------------------------------------------------------

/// The three weights are a partition of unity, never negative, at every Blending, Balance and
/// width floor, and the derivative of every weight is bounded by `1.5 / width`: the smoothstep's
/// steepest slope, so no Blending gives a step.
#[test]
fn weights_are_a_bounded_slope_partition_of_unity() {
    for blending in [0.0, 10.0, 50.0, 90.0, 100.0] {
        for balance in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            for floor in [0.0, LUMINANCE_WIDTH_FLOOR] {
                let width = (0.1 + 0.9 * blending / 100.0_f64).max(floor);
                let step = 1e-4;
                let mut previous = grade::weights(0.0, blending, balance, floor);
                for index in 1..=10_000 {
                    let l = f64::from(index) * step;
                    let weights = grade::weights(l, blending, balance, floor);
                    assert!(weights.iter().all(|w| *w >= -1e-15));
                    assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
                    for (now, before) in weights.iter().zip(previous) {
                        let slope = (now - before).abs() / step;
                        assert!(
                            slope <= 1.5 / width + 1e-6,
                            "blending {blending}, balance {balance}: slope {slope} at {l}"
                        );
                    }
                    previous = weights;
                }
            }
        }
    }
}

/// Balance moves the split monotonically: the shadow weight at mid grey falls and the highlight
/// weight rises as Balance goes from -100 to +100.
#[test]
fn balance_moves_the_split_monotonically() {
    let mut previous = grade::weights(0.5, 50.0, -100.0, 0.0);
    for step in -99..=100 {
        let weights = grade::weights(0.5, 50.0, f64::from(step), 0.0);
        assert!(weights[0] <= previous[0] + 1e-15);
        assert!(weights[2] >= previous[2] - 1e-15);
        previous = weights;
    }
}

// -------------------------------------------------------------------------------------------
// Luminance
// -------------------------------------------------------------------------------------------

/// The lightness response is strictly increasing over `[0, 1]` for every combination of the four
/// luminance amounts at -100, -50, 0, 50 and 100, at Blending 0, 50 and 100 and Balance -100, 0
/// and 100, measured densely in `f64` on greys.
#[test]
fn luminance_is_strictly_increasing_for_every_combination() {
    let levels = [-100.0, -50.0, 0.0, 50.0, 100.0];
    let mut tightest = f64::INFINITY;
    for combination in 0..levels.len().pow(4) {
        let mut code = combination;
        let amounts: [f64; 4] = std::array::from_fn(|_| {
            let level = levels[code % levels.len()];
            code /= levels.len();
            level
        });
        for blending in [0.0, 50.0, 100.0] {
            for balance in [-100.0, 0.0, 100.0] {
                let params = GradeParams {
                    wheels: amounts.map(|l| wheel(0.0, 0.0, l)),
                    blending,
                    balance,
                };
                let mut previous = f64::NEG_INFINITY;
                for step in 0..=1000 {
                    let l = f64::from(step) / 1000.0;
                    let out = lab(grade::apply(grey_at(l), &params)).l;
                    assert!(
                        out > previous,
                        "{amounts:?}, blending {blending}, balance {balance}: {out} after \
                         {previous} at {l}"
                    );
                    if previous.is_finite() {
                        tightest = tightest.min((out - previous) * 1000.0);
                    }
                    previous = out;
                }
            }
        }
    }
    assert!(tightest > 0.0);
}

/// Black and white are fixed under every tint and under luminance that does not lift black or dim
/// white; Shadows +100 lifts black to exactly [`SHADOW_LIFT`] and Highlights -100 dims white by
/// exactly [`HIGHLIGHT_DIM`].
#[test]
fn black_and_white_move_only_under_lift_and_dim() {
    let tints = GradeParams {
        wheels: [wheel(77.0, 100.0, 0.0); WHEEL_COUNT],
        blending: 100.0,
        balance: 0.0,
    };
    for (l, expected) in [(0.0, 0.0), (1.0, 1.0)] {
        let out = lab(grade::apply(grey_at(l), &tints));
        // The published Oklab matrices are rounded, so a round trip of white is exact to about
        // 1e-8, not to the last digit.
        assert!((out.l - expected).abs() < 1e-7, "{l}: {out:?}");
        assert!(colour::chroma(out) < 1e-6, "{l}: {out:?}");
    }
    let lifted = lab(grade::apply(
        grey_at(0.0),
        &only(SHADOWS, wheel(0.0, 0.0, 100.0)),
    ));
    assert!((lifted.l - SHADOW_LIFT).abs() < 1e-7);
    let dimmed = lab(grade::apply(
        grey_at(1.0),
        &only(HIGHLIGHTS, wheel(0.0, 0.0, -100.0)),
    ));
    assert!((dimmed.l - (1.0 - HIGHLIGHT_DIM)).abs() < 1e-7);
}

// -------------------------------------------------------------------------------------------
// Tint
// -------------------------------------------------------------------------------------------

/// The tint direction is periodic, continuous and of unit length; the wheel's six primaries and
/// secondaries point at the Oklab hue of the sRGB colour itself.
#[test]
fn the_tint_direction_follows_the_rgb_wheel() {
    for (hue, rgb) in [
        (0.0, [1.0, 0.0, 0.0]),
        (60.0, [1.0, 1.0, 0.0]),
        (120.0, [0.0, 1.0, 0.0]),
        (180.0, [0.0, 1.0, 1.0]),
        (240.0, [0.0, 0.0, 1.0]),
        (300.0, [1.0, 0.0, 1.0]),
    ] {
        let [a, b] = grade::tint_direction(hue);
        let expected = hue_deg(lab(rgb));
        let direction = b.atan2(a).to_degrees().rem_euclid(360.0);
        assert!(circular(direction, expected) < 1e-9, "{hue}");
    }
    assert_eq!(grade::tint_direction(0.0), grade::tint_direction(360.0));
    let mut previous = grade::tint_direction(0.0);
    for step in 1..=36_000 {
        let direction = grade::tint_direction(f64::from(step) / 100.0);
        let turn = circular(
            direction[1].atan2(direction[0]).to_degrees(),
            previous[1].atan2(previous[0]).to_degrees(),
        );
        assert!(turn < 0.1, "{step}");
        assert!((direction[0].hypot(direction[1]) - 1.0).abs() < 1e-12);
        previous = direction;
    }
}

/// On mid grey, where the envelope is one, a wheel at full saturation adds [`TINT_CHROMA`] times its
/// weight, in the wheel's direction: Global its full chroma, Midtones its weight's share.
#[test]
fn a_full_tint_adds_its_declared_chroma() {
    let mid = grey_at(0.5);
    let [_, midtone_weight, _] = grade::weights(0.5, 50.0, 0.0, 0.0);
    for hue in [0.0, 45.0, 200.0, 359.0] {
        let midtones = lab(grade::apply(mid, &only(MIDTONES, wheel(hue, 100.0, 0.0))));
        let global = lab(grade::apply(mid, &only(GLOBAL, wheel(hue, 100.0, 0.0))));
        assert!((colour::chroma(midtones) - TINT_CHROMA * midtone_weight).abs() < 1e-7);
        assert!((colour::chroma(global) - TINT_CHROMA).abs() < 1e-7);
        let [a, b] = grade::tint_direction(hue);
        assert!(circular(hue_deg(midtones), b.atan2(a).to_degrees()) < 1e-4);
    }
}

/// Global reads neither Blending nor Balance.
#[test]
fn global_is_independent_of_blending_and_balance() {
    let global = wheel(130.0, 60.0, -40.0);
    for l in [0.05, 0.3, 0.5, 0.8, 0.97] {
        let expected = grade::apply(grey_at(l), &only(GLOBAL, global));
        for (blending, balance) in [(0.0, -100.0), (100.0, 100.0), (17.0, 33.0)] {
            let mut params = only(GLOBAL, global);
            params.blending = blending;
            params.balance = balance;
            assert_eq!(grade::apply(grey_at(l), &params), expected);
        }
    }
}

/// Every output is finite for signed and above-white input, and the response is continuous across
/// black and white.
#[test]
fn extended_input_is_finite_and_continuous() {
    let params = GradeParams {
        wheels: [
            wheel(200.0, 70.0, 100.0),
            wheel(30.0, 50.0, -60.0),
            wheel(60.0, 90.0, -100.0),
            wheel(300.0, 20.0, 40.0),
        ],
        blending: 0.0,
        balance: 40.0,
    };
    for r in -4..=12 {
        for g in -4..=12 {
            for b in -4..=12 {
                let rgb = [f64::from(r), f64::from(g), f64::from(b)].map(|v| v * 0.25);
                assert!(grade::apply(rgb, &params).iter().all(|v| v.is_finite()));
            }
        }
    }
    for edge in [0.0, 1.0] {
        let below = grade::apply(grey_at(edge - 1e-7), &params);
        let above = grade::apply(grey_at(edge + 1e-7), &params);
        for channel in 0..3 {
            assert!((below[channel] - above[channel]).abs() < 1e-5, "{edge}");
        }
    }
}

// -------------------------------------------------------------------------------------------
// Figures
// -------------------------------------------------------------------------------------------

/// The figures the design quotes: each range's share of the tone scale at the default Blending,
/// the chroma a full tint adds on a grey wedge, and the lightness moves luminance ±100 makes.
#[test]
#[ignore = "prints the study's figures"]
fn grade_figures() {
    println!("weights at the default Blending and Balance:");
    for l in [0.0, 0.1, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75, 0.9, 1.0] {
        let [s, m, h] = grade::weights(l, 50.0, 0.0, 0.0);
        println!("  L {l:.3}: shadows {s:.3}, midtones {m:.3}, highlights {h:.3}");
    }
    for (name, index) in [
        ("shadows", SHADOWS),
        ("midtones", MIDTONES),
        ("highlights", HIGHLIGHTS),
    ] {
        let mut row = Vec::new();
        for code in [16u8, 64, 128, 192, 240] {
            let rgb = [srgb::decode(code); 3];
            let out = lab(grade::apply(rgb, &only(index, wheel(30.0, 100.0, 0.0))));
            row.push(format!("{code}: C {:.3}", colour::chroma(out)));
        }
        println!("{name} saturation 100 on grey codes: {}", row.join(", "));
        let mut row = Vec::new();
        for code in [16u8, 64, 128, 192, 240] {
            let rgb = [srgb::decode(code); 3];
            let up = srgb::code(grade::apply(rgb, &only(index, wheel(0.0, 0.0, 100.0)))[0]);
            let down = srgb::code(grade::apply(rgb, &only(index, wheel(0.0, 0.0, -100.0)))[0]);
            row.push(format!("{code}: {down}..{up}"));
        }
        println!(
            "{name} luminance -100..+100 on grey codes: {}",
            row.join(", ")
        );
    }
}

// -------------------------------------------------------------------------------------------
// Fixtures
// -------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ParameterSet {
    name: String,
    /// Hue, saturation and luminance per wheel, in `wheel_order`.
    wheels: [[f64; 3]; WHEEL_COUNT],
    blending: f64,
    balance: f64,
}

impl ParameterSet {
    fn params(&self) -> GradeParams {
        GradeParams {
            wheels: self.wheels.map(|[hue, saturation, luminance]| Wheel {
                hue,
                saturation,
                luminance,
            }),
            blending: self.blending,
            balance: self.balance,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Case {
    name: String,
    /// A linear sRGB input.
    input: [f64; 3],
    parameters: String,
    expected_linear: [f64; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FixtureFile {
    generated_by: String,
    note: String,
    wheel_order: Vec<String>,
    parameter_sets: Vec<ParameterSet>,
    cases: Vec<Case>,
}

fn set(name: &str, wheels: [[f64; 3]; WHEEL_COUNT], blending: f64, balance: f64) -> ParameterSet {
    ParameterSet {
        name: name.into(),
        wheels,
        blending,
        balance,
    }
}

fn fixture_parameter_sets() -> Vec<ParameterSet> {
    const N: [f64; 3] = [0.0, 0.0, 0.0];
    vec![
        set("shadows_teal", [[190.0, 80.0, 0.0], N, N, N], 50.0, 0.0),
        set(
            "midtones_red_seam",
            [N, [360.0, 100.0, 0.0], N, N],
            50.0,
            0.0,
        ),
        set(
            "highlights_gold_dim",
            [N, N, [45.0, 70.0, -100.0], N],
            50.0,
            0.0,
        ),
        set("global_blue", [N, N, N, [240.0, 50.0, 0.0]], 0.0, 100.0),
        set("shadows_lift", [[0.0, 0.0, 100.0], N, N, N], 50.0, 0.0),
        set(
            "split_full_blend",
            [[20.0, 100.0, 0.0], N, [200.0, 100.0, 0.0], N],
            100.0,
            -60.0,
        ),
        set(
            "everything_sharp",
            [
                [200.0, 40.0, -30.0],
                [35.0, 25.0, 15.0],
                [55.0, 60.0, -45.0],
                [300.0, 10.0, 20.0],
            ],
            0.0,
            -100.0,
        ),
        set(
            "luminance_extremes",
            [
                [0.0, 0.0, 100.0],
                [0.0, 0.0, -100.0],
                [0.0, 0.0, 100.0],
                [0.0, 0.0, -100.0],
            ],
            0.0,
            0.0,
        ),
    ]
}

fn fixture_inputs() -> Vec<(String, [f64; 3])> {
    let mut inputs = Vec::new();
    for code in [0u8, 8, 32, 64, 128, 192, 240, 255] {
        inputs.push((format!("grey_{code}"), [srgb::decode(code); 3]));
    }
    for l in [0.3, 0.6, 0.85] {
        for step in 0..8 {
            let angle = f64::from(step) * 45.0;
            let radians = angle.to_radians();
            inputs.push((
                format!("wheel_l{l}_h{angle}"),
                colour::from_oklab(Oklab {
                    l,
                    a: 0.1 * radians.cos(),
                    b: 0.1 * radians.sin(),
                }),
            ));
        }
    }
    for (name, rgb) in [
        ("skin", [255u8, 219, 172]),
        ("deep_skin", [141, 85, 36]),
        ("sky", [110, 160, 220]),
        ("foliage", [60, 110, 40]),
    ] {
        inputs.push((name.to_owned(), rgb.map(srgb::decode)));
    }
    inputs.push(("above_white".into(), [1.5, 1.2, 0.9]));
    inputs.push(("signed".into(), [1.2, -0.05, 0.4]));
    inputs.push(("bright_grey".into(), [2.0, 2.0, 2.0]));
    inputs
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mixer")
        .join("grade-cases.json")
}

#[test]
#[ignore = "run explicitly to (re)generate fixtures/mixer/grade-cases.json from the reference"]
fn generate_grade_fixtures() {
    let parameter_sets = fixture_parameter_sets();
    let mut cases = Vec::new();
    for (input_name, input) in fixture_inputs() {
        for set in &parameter_sets {
            cases.push(Case {
                name: format!("{input_name}__{}", set.name),
                input,
                parameters: set.name.clone(),
                expected_linear: grade::apply(input, &set.params()),
            });
        }
    }
    let file = FixtureFile {
        generated_by: "crates/luxforge-reference/tests/studies/grade.rs generate_grade_fixtures"
            .into(),
        note:
            "f64 reference (crates/luxforge-reference/src/grade.rs) of the mixer's initial grading \
               equations. expected_linear is linear sRGB after the grading unit, full f64 \
               precision, unclamped. Production is compared within 1e-5 + 1e-5 * |reference|."
                .into(),
        wheel_order: WHEEL_NAMES.iter().map(|name| name.to_string()).collect(),
        parameter_sets,
        cases,
    };
    let path = fixture_path();
    let json = serde_json::to_string_pretty(&file).expect("serialize fixtures");
    fs::write(&path, json).expect("write fixtures/mixer/grade-cases.json");
    println!("wrote {} cases to {}", file.cases.len(), path.display());
}

#[test]
fn grade_fixtures_match_reference() {
    let path = fixture_path();
    let json = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} is missing ({error}); run the ignored generate_grade_fixtures",
            path.display()
        )
    });
    let file: FixtureFile = serde_json::from_str(&json).expect("parse grade-cases.json");
    assert_eq!(
        file.wheel_order,
        WHEEL_NAMES.map(str::to_owned).to_vec(),
        "the fixture's wheel order is the reference's"
    );
    assert_eq!(
        file.cases.len(),
        fixture_inputs().len() * fixture_parameter_sets().len()
    );
    for case in &file.cases {
        let set = file
            .parameter_sets
            .iter()
            .find(|set| set.name == case.parameters)
            .expect("the named set");
        let fresh = grade::apply(case.input, &set.params());
        for channel in 0..3 {
            let (stored, fresh) = (case.expected_linear[channel], fresh[channel]);
            assert!(
                (stored - fresh).abs()
                    <= super::FIXTURE_ROUND_TRIP_TOLERANCE * fresh.abs().max(1.0),
                "{} channel {channel}: stored {stored}, fresh {fresh}",
                case.name
            );
        }
    }
}
