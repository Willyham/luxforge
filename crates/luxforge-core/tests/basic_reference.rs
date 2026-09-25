//! Cross-checks the independent f64 `reference` module (see `tests/reference/mod.rs`) against
//! `fixtures/basic/exposure-cases.json`, an independently computed corpus (inline `python3` f64
//! implementation of the same formulas, not the Rust module). Agreement between the two proves
//! neither independent implementation has a one-off formula mistake.

mod reference;

use reference::{RefOp, evaluate_pixel};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Deserialize)]
struct ExposureCase {
    input: [u8; 3],
    ev: f64,
    expected: [u8; 3],
    #[serde(default)]
    #[allow(dead_code)]
    note: String,
}

#[test]
fn reference_module_agrees_with_the_independent_exposure_corpus() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic/exposure-cases.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    let cases: Vec<ExposureCase> = serde_json::from_str(&raw).unwrap_or_else(|error| {
        panic!("failed to parse {}: {error}", path.display());
    });
    assert!(!cases.is_empty(), "expected at least one exposure case");
    for case in &cases {
        let actual = evaluate_pixel(case.input, &[RefOp::Exposure(case.ev)]);
        assert_eq!(
            actual, case.expected,
            "input={:?} ev={} expected={:?} actual={:?}",
            case.input, case.ev, case.expected, actual
        );
    }
}
