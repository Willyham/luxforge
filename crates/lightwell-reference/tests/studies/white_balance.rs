//! Proofs for `docs/design/basic-white-balance.md` against the independent f64 reference in
//! `reference/white_balance.rs`, and generation/reload of `fixtures/basic/white-balance-cases.json`.
//!
//! No production code exists yet for Temperature, Tint or the neutral picker; this file is the
//! numerical study's evidence, run ahead of that implementation. `cargo test -p lightwell-core
//! -p lightwell-reference --test studies -- --ignored regenerate_white_balance_fixtures` rewrites
//! the checked-in fixture from the current reference; the non-ignored fixture test below reloads
//! it and recomputes every case, so a constant change that is not also reflected in the fixture
//! (or vice versa) fails the suite.

use lightwell_reference::white_balance::{
    self, PARAMETER_RANGE, RejectReason, apply, average_patch, gains, gather_patch, solve_neutral,
};
use lightwell_reference::{srgb_decode, srgb_encode, srgb_quantize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/basic/white-balance-cases.json"
    ))
}

// -------------------------------------------------------------------------------------------
// 0/0 is the identity, exactly, to within 1e-12.
// -------------------------------------------------------------------------------------------

#[test]
fn identity_at_zero_zero_is_exact_to_1e12() {
    for rgb in [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.9, 0.05, 0.2],
        [0.05, 0.9, 0.2],
        [0.05, 0.2, 0.9],
        [1.3, -0.2, 0.4], // out-of-gamut input must also round-trip exactly
    ] {
        let out = apply(0.0, 0.0, rgb);
        for c in 0..3 {
            assert!(
                (out[c] - rgb[c]).abs() < 1e-12,
                "channel {c}: {rgb:?} -> {out:?}"
            );
        }
    }
    let g = gains(0.0, 0.0);
    assert_eq!(g, [1.0, 1.0, 1.0]);
}

#[test]
fn identity_on_all_256_greys_through_quantizer() {
    for code in 0u8..=255 {
        let linear = srgb_decode(code);
        let out = apply(0.0, 0.0, [linear, linear, linear]);
        for c in 0..3 {
            let round_tripped = srgb_quantize(srgb_encode(out[c]));
            assert_eq!(
                round_tripped, code,
                "grey code {code} channel {c}: linear={linear} out={out:?}"
            );
        }
    }
}

// -------------------------------------------------------------------------------------------
// Sign conventions: positive temperature warms, negative cools; positive tint is magenta.
// -------------------------------------------------------------------------------------------

#[test]
fn positive_temperature_warms_negative_cools() {
    let mid = [0.5, 0.5, 0.5];
    let warm = apply(50.0, 0.0, mid);
    let cool = apply(-50.0, 0.0, mid);
    let neutral = apply(0.0, 0.0, mid);
    assert!(warm[0] > neutral[0], "warm R should rise: {warm:?}");
    assert!(warm[2] < neutral[2], "warm B should fall: {warm:?}");
    assert!(cool[0] < neutral[0], "cool R should fall: {cool:?}");
    assert!(cool[2] > neutral[2], "cool B should rise: {cool:?}");
}

#[test]
fn temperature_monotonic_over_full_range() {
    // R/B ratio of a mid-grey strictly increases as temperature increases, at every integer step,
    // for several tint values (the axes are not independent of each other's presence).
    for &tint in &[-100.0, -25.0, 0.0, 25.0, 100.0] {
        let mut previous_ratio = f64::NEG_INFINITY;
        for t in -100..=100 {
            let out = apply(t as f64, tint, [0.5, 0.5, 0.5]);
            let ratio = out[0] / out[2];
            assert!(
                ratio > previous_ratio,
                "not monotonic at t={t} tint={tint}: ratio={ratio} previous={previous_ratio}"
            );
            previous_ratio = ratio;
        }
    }
}

#[test]
fn positive_tint_is_magenta() {
    let mid = [0.5, 0.5, 0.5];
    let magenta = apply(0.0, 50.0, mid);
    let green = apply(0.0, -50.0, mid);
    let neutral = apply(0.0, 0.0, mid);
    assert!(magenta[0] > neutral[0], "R should rise: {magenta:?}");
    assert!(magenta[2] > neutral[2], "B should rise: {magenta:?}");
    assert!(magenta[1] < neutral[1], "G should fall: {magenta:?}");
    assert!(green[0] < neutral[0], "R should fall: {green:?}");
    assert!(green[2] < neutral[2], "B should fall: {green:?}");
    assert!(green[1] > neutral[1], "G should rise: {green:?}");
}

#[test]
fn tint_monotonic_over_full_range() {
    for &temperature in &[-100.0, -25.0, 0.0, 25.0, 100.0] {
        let mut previous = f64::NEG_INFINITY;
        for tint in -100..=100 {
            let out = apply(temperature, tint as f64, [0.5, 0.5, 0.5]);
            let magenta_axis = (out[0] + out[2]) / 2.0 - out[1];
            assert!(
                magenta_axis > previous,
                "not monotonic at temperature={temperature} tint={tint}"
            );
            previous = magenta_axis;
        }
    }
}

// -------------------------------------------------------------------------------------------
// Finite, positive gains and predictable finite output for saturated colour, over the full range.
// -------------------------------------------------------------------------------------------

#[test]
fn gains_finite_and_positive_over_full_range() {
    for t in (-100..=100).step_by(5) {
        for tint in (-100..=100).step_by(5) {
            let g = gains(t as f64, tint as f64);
            for (c, value) in g.iter().enumerate() {
                assert!(
                    value.is_finite() && *value > 0.0,
                    "gain[{c}] at t={t} tint={tint} is {value}"
                );
            }
        }
    }
}

#[test]
fn saturated_patch_produces_predictable_finite_output() {
    let saturated = [
        [0.8, 0.1, 0.05],
        [0.05, 0.1, 0.8],
        [0.1, 0.8, 0.1],
        [1.3, -0.1, 0.02], // out-of-gamut input is legal between units; must stay finite
    ];
    for rgb in saturated {
        for &(t, tint) in &[(100.0, 0.0), (-100.0, 0.0), (0.0, 100.0), (0.0, -100.0)] {
            let out = apply(t, tint, rgb);
            assert!(
                out.iter().all(|v| v.is_finite()),
                "non-finite output for {rgb:?} at t={t} tint={tint}: {out:?}"
            );
        }
    }
    // Warming a strongly blue patch drives its red channel down and can go negative; that is
    // preserved, not clamped, inside the unit.
    let out = apply(-100.0, 0.0, [0.05, 0.1, 0.8]);
    assert!(out[0] < 0.0, "expected a preserved negative red: {out:?}");
}

// -------------------------------------------------------------------------------------------
// Neutral-picker rejection: near-black, clipped, non-finite, out-of-range, non-convergent.
// -------------------------------------------------------------------------------------------

#[test]
fn rejects_near_black_patch() {
    // Uniform very dark grey, well under NEAR_BLACK_LUMINANCE, no clipped channel.
    let patch = vec![[8u8, 8, 8]; 25];
    assert_eq!(
        average_patch(&patch),
        Err(RejectReason::NearBlack),
        "code 8 grey should be rejected as near-black"
    );
}

#[test]
fn rejects_clipped_patch() {
    let mut patch = vec![[120u8, 120, 120]; 25];
    patch[12] = [255, 120, 120]; // one clipped highlight pixel among otherwise fine ones
    assert_eq!(average_patch(&patch), Err(RejectReason::Clipped));

    let mut patch2 = vec![[120u8, 120, 120]; 25];
    patch2[0] = [0, 120, 120]; // one clipped shadow pixel
    assert_eq!(average_patch(&patch2), Err(RejectReason::Clipped));
}

#[test]
fn rejects_non_finite_sample() {
    assert_eq!(
        solve_neutral([f64::NAN, 0.5, 0.5]),
        Err(RejectReason::NonFinite)
    );
    assert_eq!(
        solve_neutral([0.0, 0.0, 0.0]), // degenerate: XYZ sums to zero, chromaticity undefined
        Err(RejectReason::NonFinite)
    );
}

#[test]
fn rejects_out_of_range_correction_without_clamping() {
    // Construct a patch whose chromaticity is the target for temperature = 120, well outside the
    // representable range. The solver must converge (the underlying map is defined there) and
    // then report OutOfRange rather than silently returning +/-100.
    let patch = synthetic_cast_patch(120.0, 0.0);
    match solve_from_patch(&patch) {
        Err(RejectReason::OutOfRange { temperature, value }) => {
            assert!(temperature, "temperature axis should be flagged");
            assert!(value > PARAMETER_RANGE);
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
}

// -------------------------------------------------------------------------------------------
// Solver round trip over the required grid, and its own convergence/iteration bounds.
// -------------------------------------------------------------------------------------------

#[test]
fn solver_round_trips_over_grid_within_one_unit() {
    for t_true in (-100..=100).step_by(20) {
        for tint_true in (-100..=100).step_by(20) {
            let patch = synthetic_cast_patch(t_true as f64, tint_true as f64);
            match solve_from_patch(&patch) {
                Ok((t_solved, tint_solved)) => {
                    assert!(
                        (t_solved - t_true).abs() <= 1,
                        "temperature off by more than 1 unit: true={t_true} solved={t_solved}"
                    );
                    assert!(
                        (tint_solved - tint_true).abs() <= 1,
                        "tint off by more than 1 unit: true={tint_true} solved={tint_solved}"
                    );
                }
                Err(reason) => {
                    panic!("expected a solution at true=({t_true},{tint_true}), got {reason:?}")
                }
            }
        }
    }
}

use lightwell_reference::white_balance::solve_from_patch;

/// Build a 5x5 patch whose 25 samples' linear-light *average* has the chromaticity that is
/// exactly the target for `(temperature, tint)`, at a luminance safe from both rejection
/// thresholds.
///
/// The 25 bytes are not identical: they carry an ordered dither (see [`dithered_patch`]) so their
/// average recovers the exact float target far more closely than any single 8-bit sample could.
/// This mirrors why the picker contract averages a 5x5 patch instead of reading one pixel — a
/// single quantized code's chromaticity noise is large enough, relative to one Tint step's tiny
/// (0.0002 uv-unit) movement, to misplace the solved Tint by several units on its own (see the
/// design doc's "why a patch, not a pixel" note). `solver_round_trips_over_grid_within_one_unit`
/// and the generated fixtures exercise this dithered construction, not a uniform swatch.
fn synthetic_cast_patch(temperature: f64, tint: f64) -> Vec<[u8; 3]> {
    let (u, v, _) = white_balance::target_uv(temperature, tint);
    let (x, y) = white_balance::uv_to_xy(u, v);
    let luminance = 0.5;
    let xyz = [x / y * luminance, luminance, (1.0 - x - y) / y * luminance];
    let rgb = white_balance::mat3_vec(&rgb_to_xyz_inverse(), xyz);
    dithered_patch(rgb)
}

/// Ordered dither of one linear-light colour into 25 8-bit samples: sample `k` of 25 rounds up
/// whenever the code's fractional part reaches threshold `(k + 0.5) / 25`, so across all 25
/// samples the fraction that round up matches the fractional part almost exactly, and the
/// resulting average is close to unquantized. A real photographed patch gets the same
/// noise-averaging benefit from ordinary pixel-to-pixel texture; this stands in for that.
fn dithered_patch(rgb: [f64; 3]) -> Vec<[u8; 3]> {
    let n = 25usize;
    (0..n)
        .map(|k| {
            let tau = (k as f64 + 0.5) / n as f64;
            let mut pixel = [0u8; 3];
            for c in 0..3 {
                let encoded = srgb_encode(rgb[c].clamp(0.0, 1.0));
                let scaled = 255.0 * encoded;
                let base = scaled.floor();
                let frac = scaled - base;
                let code = if frac >= 1.0 - tau { base + 1.0 } else { base };
                pixel[c] = code.clamp(0.0, 255.0) as u8;
            }
            pixel
        })
        .collect()
}

fn rgb_to_xyz_inverse() -> white_balance::Mat3 {
    white_balance::mat3_inverse(&white_balance::RGB_TO_XYZ)
}

// -------------------------------------------------------------------------------------------
// Edge clipping of the 5x5 patch gather.
// -------------------------------------------------------------------------------------------

#[test]
fn gather_patch_clips_at_image_edges() {
    let width = 10u32;
    let height = 10u32;
    // Top-left corner: only dx,dy in 0..=2 are in bounds -> a 3x3 = 9-pixel patch.
    let patch = gather_patch(width, height, 0, 0, |x, y| [x as u8, y as u8, 0]);
    assert_eq!(patch.len(), 9, "corner patch should clip to 9 pixels");

    // Interior point: full 5x5 = 25 pixels.
    let patch = gather_patch(width, height, 5, 5, |x, y| [x as u8, y as u8, 0]);
    assert_eq!(
        patch.len(),
        25,
        "interior patch should be the full 25 pixels"
    );
}

// -------------------------------------------------------------------------------------------
// Fixtures: generate (ignored) and verify (always run) fixtures/basic/white-balance-cases.json.
// -------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct TransformCase {
    input_rgb_u8: [u8; 3],
    temperature: f64,
    tint: f64,
    expected_linear_f64: [f64; 3],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct SolverCase {
    description: String,
    patch_u8: Vec<[u8; 3]>,
    expected_solution: Option<(i32, i32)>,
    expected_rejection: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Fixtures {
    transform_cases: Vec<TransformCase>,
    solver_cases: Vec<SolverCase>,
}

fn transform_case(input_rgb_u8: [u8; 3], temperature: f64, tint: f64) -> TransformCase {
    let linear = [
        srgb_decode(input_rgb_u8[0]),
        srgb_decode(input_rgb_u8[1]),
        srgb_decode(input_rgb_u8[2]),
    ];
    let expected_linear_f64 = apply(temperature, tint, linear);
    TransformCase {
        input_rgb_u8,
        temperature,
        tint,
        expected_linear_f64,
    }
}

fn build_fixtures() -> Fixtures {
    let colors: [[u8; 3]; 5] = [
        [0, 0, 0],
        [64, 64, 64],
        [128, 128, 128],
        [230, 60, 40],
        [40, 90, 230],
    ];
    let params: [(f64, f64); 9] = [
        (0.0, 0.0),
        (50.0, 0.0),
        (-50.0, 0.0),
        (100.0, 0.0),
        (-100.0, 0.0),
        (0.0, 50.0),
        (0.0, -50.0),
        (0.0, 100.0),
        (0.0, -100.0),
    ];
    let mut transform_cases = Vec::new();
    for color in colors {
        for &(t, tint) in &params {
            transform_cases.push(transform_case(color, t, tint));
        }
    }

    let mut solver_cases = Vec::new();
    for t_true in (-100..=100).step_by(20) {
        for tint_true in (-100..=100).step_by(20) {
            let patch = synthetic_cast_patch(t_true as f64, tint_true as f64);
            let solved = solve_from_patch(&patch).expect("grid case must solve");
            solver_cases.push(SolverCase {
                description: format!("round trip true=({t_true},{tint_true})"),
                patch_u8: patch,
                expected_solution: Some(solved),
                expected_rejection: None,
            });
        }
    }
    solver_cases.push(SolverCase {
        description: "near-black uniform patch".to_string(),
        patch_u8: vec![[8, 8, 8]; 25],
        expected_solution: None,
        expected_rejection: Some(format!("{:?}", RejectReason::NearBlack)),
    });
    {
        let mut patch = vec![[120u8, 120, 120]; 25];
        patch[12] = [255, 120, 120];
        solver_cases.push(SolverCase {
            description: "one clipped highlight pixel".to_string(),
            patch_u8: patch,
            expected_solution: None,
            expected_rejection: Some(format!("{:?}", RejectReason::Clipped)),
        });
    }
    {
        let patch = synthetic_cast_patch(120.0, 0.0);
        let rejection = solve_from_patch(&patch).unwrap_err();
        solver_cases.push(SolverCase {
            description: "cast requiring temperature=120, outside range".to_string(),
            patch_u8: patch,
            expected_solution: None,
            expected_rejection: Some(format!("{rejection:?}")),
        });
    }
    {
        // Edge-clipped 3x3 corner patch of a mild warm cast, still solvable from 9 samples.
        let (u, v, _) = white_balance::target_uv(30.0, -10.0);
        let (x, y) = white_balance::uv_to_xy(u, v);
        let luminance = 0.3;
        let xyz = [x / y * luminance, luminance, (1.0 - x - y) / y * luminance];
        let rgb = white_balance::mat3_vec(&rgb_to_xyz_inverse(), xyz);
        let byte = |v: f64| srgb_quantize(srgb_encode(v.clamp(0.0, 1.0)));
        let pixel = [byte(rgb[0]), byte(rgb[1]), byte(rgb[2])];
        let patch = gather_patch(10, 10, 0, 0, |_, _| pixel);
        let solved = solve_from_patch(&patch).expect("edge-clipped patch must still solve");
        solver_cases.push(SolverCase {
            description: "edge-clipped 3x3 corner patch (9 of 25 pixels)".to_string(),
            patch_u8: patch,
            expected_solution: Some(solved),
            expected_rejection: None,
        });
    }

    Fixtures {
        transform_cases,
        solver_cases,
    }
}

#[test]
#[ignore = "regenerates the checked-in fixture from the current reference; run explicitly after a constant change"]
fn regenerate_white_balance_fixtures() {
    let fixtures = build_fixtures();
    let json = serde_json::to_string_pretty(&fixtures).unwrap();
    std::fs::write(fixture_path(), json + "\n").unwrap();
}

#[test]
fn fixtures_match_reference_recomputation() {
    let raw = std::fs::read_to_string(fixture_path()).expect(
        "fixtures/basic/white-balance-cases.json must exist; run the ignored regenerate test",
    );
    let loaded: Fixtures = serde_json::from_str(&raw).unwrap();
    let recomputed = build_fixtures();

    assert_eq!(
        loaded.transform_cases.len(),
        recomputed.transform_cases.len()
    );
    for (loaded_case, fresh_case) in loaded
        .transform_cases
        .iter()
        .zip(recomputed.transform_cases.iter())
    {
        assert_eq!(loaded_case.input_rgb_u8, fresh_case.input_rgb_u8);
        assert_eq!(loaded_case.temperature, fresh_case.temperature);
        assert_eq!(loaded_case.tint, fresh_case.tint);
        for c in 0..3 {
            assert!(
                (loaded_case.expected_linear_f64[c] - fresh_case.expected_linear_f64[c]).abs()
                    < 1e-9,
                "fixture drifted from reference for {loaded_case:?}"
            );
        }
    }

    assert_eq!(loaded.solver_cases.len(), recomputed.solver_cases.len());
    for (loaded_case, fresh_case) in loaded
        .solver_cases
        .iter()
        .zip(recomputed.solver_cases.iter())
    {
        assert_eq!(loaded_case.patch_u8, fresh_case.patch_u8);
        assert_eq!(
            loaded_case.expected_solution, fresh_case.expected_solution,
            "solved-parameter fixture drifted for: {}",
            loaded_case.description
        );
        assert_eq!(
            loaded_case.expected_rejection, fresh_case.expected_rejection,
            "rejection-reason fixture drifted for: {}",
            loaded_case.description
        );
    }
}
