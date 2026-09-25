//! Fixture generation and reload for the frozen Saturation/Vibrance
//! reference in `tests/reference/colour.rs`. The property proofs
//! themselves live as unit tests inside that module; this file only builds
//! and checks `fixtures/basic/colour-cases.json`.
//!
//! `fixtures/basic/colour-cases.json` is committed. It was produced by
//! running the ignored `generate_colour_fixtures` test once:
//!
//! ```sh
//! cargo test --package luxforge-core --test basic_colour_reference \
//!     -- --ignored generate_colour_fixtures
//! ```
//!
//! `colour_fixtures_match_reference` (not ignored, runs in `cargo xtask
//! check`) reloads the committed file and recomputes every case with the
//! same reference, so a silent drift between the file and the frozen
//! formulas fails the build instead of going unnoticed.

mod reference;

use reference::colour;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Input {
    /// An 8-bit sRGB-encoded input, decoded through the reference sRGB
    /// transfer function before the colour units run.
    Srgb8 { rgb: [u8; 3] },
    /// A linear sRGB input given directly, used for the out-of-gamut cases
    /// (values above 1.0 or below 0.0 that no 8-bit code represents).
    Linear { rgb: [f64; 3] },
}

impl Input {
    fn to_linear(&self) -> [f64; 3] {
        match self {
            Input::Srgb8 { rgb } => [
                reference_code_to_linear(rgb[0]),
                reference_code_to_linear(rgb[1]),
                reference_code_to_linear(rgb[2]),
            ],
            Input::Linear { rgb } => *rgb,
        }
    }
}

// `tests/reference/mod.rs` keeps its sRGB helpers module-private so several
// numerical-task branches can each add their own without a merge conflict;
// this file recomputes the one conversion it needs from the published sRGB
// constants directly, matching `reference::srgb_decode` exactly.
fn reference_code_to_linear(code: u8) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Case {
    name: String,
    input: Input,
    vibrance: f64,
    saturation: f64,
    /// Linear sRGB output of `colour::apply_basic_colour`, at full f64
    /// precision (`serde_json`'s shortest round-trippable representation).
    /// Vibrance runs before saturation, the frozen order.
    expected_linear: [f64; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FixtureFile {
    generated_by: String,
    note: String,
    cases: Vec<Case>,
}

fn colour_inputs() -> Vec<(&'static str, Input)> {
    vec![
        // Primaries
        ("red", Input::Srgb8 { rgb: [255, 0, 0] }),
        ("green", Input::Srgb8 { rgb: [0, 255, 0] }),
        ("blue", Input::Srgb8 { rgb: [0, 0, 255] }),
        // Secondaries
        ("cyan", Input::Srgb8 { rgb: [0, 255, 255] }),
        ("magenta", Input::Srgb8 { rgb: [255, 0, 255] }),
        ("yellow", Input::Srgb8 { rgb: [255, 255, 0] }),
        // Pastels
        (
            "pastel_pink",
            Input::Srgb8 {
                rgb: [255, 200, 200],
            },
        ),
        (
            "pastel_green",
            Input::Srgb8 {
                rgb: [200, 255, 200],
            },
        ),
        (
            "pastel_blue",
            Input::Srgb8 {
                rgb: [200, 200, 255],
            },
        ),
        // Skin-like patches named in the task
        (
            "skin_light",
            Input::Srgb8 {
                rgb: [255, 219, 172],
            },
        ),
        (
            "skin_mid",
            Input::Srgb8 {
                rgb: [224, 172, 140],
            },
        ),
        ("skin_dark", Input::Srgb8 { rgb: [141, 85, 36] }),
        // Greys
        ("black", Input::Srgb8 { rgb: [0, 0, 0] }),
        (
            "grey50",
            Input::Srgb8 {
                rgb: [128, 128, 128],
            },
        ),
        (
            "white",
            Input::Srgb8 {
                rgb: [255, 255, 255],
            },
        ),
        // Out-of-gamut linear inputs: above 1.0 and below 0.0, preserved
        // between units per the integration contract.
        (
            "out_of_gamut_bright",
            Input::Linear {
                rgb: [1.5, 0.5, 0.2],
            },
        ),
        (
            "out_of_gamut_dark",
            Input::Linear {
                rgb: [-0.1, 0.3, 0.8],
            },
        ),
        (
            "out_of_gamut_mixed",
            Input::Linear {
                rgb: [1.5, -0.1, 0.7],
            },
        ),
    ]
}

fn vibrance_saturation_combinations() -> Vec<(f64, f64)> {
    vec![
        (0.0, 0.0),
        (0.0, -100.0),
        (0.0, 100.0),
        (100.0, 0.0),
        (-100.0, 0.0),
        (50.0, 50.0),
        (-100.0, -100.0),
        (100.0, 100.0),
    ]
}

fn fixture_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/luxforge-core; fixtures/ is repo-root-level.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("basic")
        .join("colour-cases.json")
}

#[test]
#[ignore = "run explicitly to (re)generate fixtures/basic/colour-cases.json from the frozen reference"]
fn generate_colour_fixtures() {
    let mut cases = Vec::new();
    for (colour_name, input) in colour_inputs() {
        let linear = input.to_linear();
        for (vibrance, saturation) in vibrance_saturation_combinations() {
            let expected_linear = colour::apply_basic_colour(linear, vibrance, saturation);
            cases.push(Case {
                name: format!("{colour_name}_v{}_s{}", vibrance as i64, saturation as i64),
                input: input.clone(),
                vibrance,
                saturation,
                expected_linear,
            });
        }
    }

    let file = FixtureFile {
        generated_by:
            "crates/luxforge-core/tests/basic_colour_reference.rs generate_colour_fixtures"
                .to_string(),
        note: "Independent f64 reference (tests/reference/colour.rs). expected_linear is linear \
               sRGB after vibrance then saturation (the frozen order), full f64 precision, \
               unclamped. Production is compared against this file within the tolerance frozen \
               in docs/design/basic-colour.md."
            .to_string(),
        cases,
    };

    let path = fixture_path();
    fs::create_dir_all(path.parent().unwrap()).expect("create fixtures/basic");
    let json = serde_json::to_string_pretty(&file).expect("serialize fixtures");
    fs::write(&path, json).expect("write fixtures/basic/colour-cases.json");
    println!("wrote {} cases to {}", file.cases.len(), path.display());
}

#[test]
fn colour_fixtures_match_reference() {
    let path = fixture_path();
    let json = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is missing ({err}); run `cargo test --package luxforge-core \
             --test basic_colour_reference -- --ignored generate_colour_fixtures` \
             to (re)create it",
            path.display()
        )
    });
    let file: FixtureFile = serde_json::from_str(&json).expect("parse colour-cases.json");
    assert!(!file.cases.is_empty(), "colour-cases.json has no cases");

    for case in &file.cases {
        let linear = case.input.to_linear();
        let recomputed = colour::apply_basic_colour(linear, case.vibrance, case.saturation);
        for (channel, (&expected, &actual)) in case
            .expected_linear
            .iter()
            .zip(recomputed.iter())
            .enumerate()
        {
            assert!(
                (expected - actual).abs() < 1e-12,
                "case {:?} channel {channel}: fixture {expected} but the reference now computes {actual} \
                 (regenerate the fixture if this is an intentional formula change)",
                case.name
            );
        }
    }
}
