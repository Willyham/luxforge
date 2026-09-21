//! TASK-011: independent proofs for the frozen global Tone algorithm, and the
//! oracle fixture a later production implementation is checked against.
//!
//! This binary shares no code with `lightwell-core`'s production sources. The
//! frozen equations live in `tests/reference/tone.rs`; the maths and every
//! constant used below are written out in full in `docs/design/basic-tone.md`,
//! which this file's test names and comments track.

mod reference;

use reference::tone::{ToneParams, luminance, tone_curve, tone_pixel};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Test-only helpers. Private to this file, independent of `reference::tone`'s
// own private srgb helpers, on purpose (see AGENTS.md and the task brief: each
// file keeps its own copy so parallel reference work merges cleanly).
// ---------------------------------------------------------------------------

/// The standard, clamped sRGB OETF, used only to quantize a *finished* linear
/// value into an output 8-bit code exactly as the host's output boundary does:
/// `floor(255 * encode(clamp(v, 0, 1)) + 0.5)`. This is not the extended,
/// unclamped encode the curve's working domain uses internally.
fn encode_srgb_u8(linear: f64) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        12.92 * clamped
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (255.0 * encoded + 0.5).floor().clamp(0.0, 255.0) as u8
}

fn decode_srgb_u8(code: u8) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// A tiny, dependency-free, fully reproducible PRNG (SplitMix64) so the
/// "random sample of 200 combinations with a fixed seed" needs no new crate
/// and is byte-for-byte reproducible across machines and Rust versions.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + u * (hi - lo)
    }
}

fn to_params(c: [f64; 5]) -> ToneParams {
    ToneParams {
        contrast: c[0],
        highlights: c[1],
        shadows: c[2],
        whites: c[3],
        blacks: c[4],
    }
}

/// The 32 corners of the 5-parameter +-100 cube.
fn cube_corners() -> Vec<[f64; 5]> {
    (0u32..32)
        .map(|bits| std::array::from_fn(|i| if (bits >> i) & 1 == 1 { 100.0 } else { -100.0 }))
        .collect()
}

/// The 80 edge midpoints of the cube: one axis held at 0, the other four at
/// every +-100 combination.
fn cube_edge_midpoints() -> Vec<[f64; 5]> {
    let mut out = Vec::with_capacity(5 * 16);
    for zero_axis in 0..5 {
        for bits in 0u32..16 {
            let mut c = [0.0; 5];
            let mut used_bits = 0;
            for (i, slot) in c.iter_mut().enumerate() {
                if i == zero_axis {
                    *slot = 0.0;
                } else {
                    *slot = if (bits >> used_bits) & 1 == 1 {
                        100.0
                    } else {
                        -100.0
                    };
                    used_bits += 1;
                }
            }
            out.push(c);
        }
    }
    out
}

/// 200 uniform-random combinations in `[-100, 100]^5` from a fixed seed.
fn random_combinations(seed: u64, n: usize) -> Vec<[f64; 5]> {
    let mut rng = SplitMix64(seed);
    (0..n)
        .map(|_| std::array::from_fn(|_| rng.next_range(-100.0, 100.0)))
        .collect()
}

/// Corners + edge midpoints + 200 fixed-seed random samples: 312 combinations.
fn all_cube_samples() -> Vec<[f64; 5]> {
    let mut all = cube_corners();
    all.extend(cube_edge_midpoints());
    all.extend(random_combinations(42, 200));
    all
}

fn neutral_ramp(steps: usize, lo: f64, hi: f64) -> Vec<f64> {
    (0..=steps)
        .map(|i| lo + (hi - lo) * (i as f64) / (steps as f64))
        .collect()
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic/tone-cases.json")
}

/// A named single-parameter constructor, e.g. `("contrast", |v| ToneParams {
/// contrast: v, ..ToneParams::NEUTRAL })`, used by several tests and the
/// fixture generator below.
type NamedSingleParam = (&'static str, fn(f64) -> ToneParams);

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[test]
fn identity_at_all_zero_parameters_is_exact_on_all_256_grey_codes() {
    for code in 0u16..=255 {
        let code = code as u8;
        let linear = decode_srgb_u8(code);
        let rgb = [linear, linear, linear];
        let out = tone_pixel(rgb, ToneParams::NEUTRAL);
        assert_eq!(
            out, rgb,
            "grey code {code}: all-neutral Tone must be the identity before quantization"
        );
        for channel in out {
            assert_eq!(
                encode_srgb_u8(channel),
                code,
                "grey code {code}: quantized round trip must be bit-exact"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Monotonicity
// ---------------------------------------------------------------------------

#[test]
fn nondecreasing_on_a_1024_step_neutral_ramp_for_every_cube_corner_edge_midpoint_and_200_random_samples()
 {
    let ramp = neutral_ramp(1024, 0.0, 1.0);
    let samples = all_cube_samples();
    assert_eq!(samples.len(), 32 + 80 + 200);
    for combo in &samples {
        let params = to_params(*combo);
        let mut previous: Option<f64> = None;
        for &l in &ramp {
            let out = tone_pixel([l, l, l], params);
            assert!(
                (out[0] - out[1]).abs() < 1e-9 && (out[1] - out[2]).abs() < 1e-9,
                "{params:?} at l={l}: a neutral (grey) input must stay grey, got {out:?}"
            );
            if let Some(previous_value) = previous {
                assert!(
                    out[0] >= previous_value - 1e-9,
                    "{params:?}: the ramp decreased at l={l}: {previous_value} -> {}",
                    out[0]
                );
            }
            previous = Some(out[0]);
        }
    }
}

#[test]
fn combined_extremes_stay_finite_and_monotone_on_an_extended_ramp() {
    // The extended domain covers a stop below black and a stop above white in
    // curve-domain terms, well past what white balance and exposure can leave
    // an ordinary photograph in, to prove the extrapolation rule holds, not
    // only the in-range behaviour.
    let ramp = neutral_ramp(512, -0.5, 2.0);
    for combo in cube_corners() {
        let params = to_params(combo);
        let mut previous: Option<f64> = None;
        for &l in &ramp {
            let out = tone_pixel([l, l, l], params);
            assert!(
                out.iter().all(|v| v.is_finite()),
                "{params:?} at l={l}: non-finite output {out:?}"
            );
            if let Some(previous_value) = previous {
                assert!(
                    out[0] >= previous_value - 1e-7,
                    "{params:?}: the extended ramp decreased at l={l}"
                );
            }
            previous = Some(out[0]);
        }
    }
}

/// The TASK-011 revision's required dense monotonicity proof: forward
/// differences of `tone_curve` on a 4001-point grid over `[-0.5, 2.0]`, for
/// all 32 cube corners, all 80 edge midpoints and 200 fixed-seed random
/// combinations (312 total, matching `all_cube_samples`).
///
/// The minimum slope actually observed over this exact grid is **not**
/// bounded by 0.02: it is `~2.03e-7`, at the corner
/// `(contrast=-100, highlights=-100, shadows=-100, whites=100, blacks=100)`,
/// x close to the domain's right edge. This is not a property of the revised
/// Highlights/Shadows family -- isolated, its own worst-case slope over this
/// same extended domain is `~0.225`, far above 0.02 (see
/// `highlights_shadows_stage_alone_has_a_strong_worst_case_slope` below). The
/// bottleneck is the unchanged Contrast stage: at Whites = Blacks = +100 the
/// endpoint remap's gap is only 0.5 (a 2x amplification), which combined with
/// Contrast = -100 (alpha = -6) pushes the value Contrast receives far enough
/// from the pivot (past x = 3 in the curve domain) that the logistic
/// saturates to within float noise of its asymptote -- the same saturation
/// documented in "Contrast" above, and present at the same order of magnitude
/// (`2.055e-7`) in this design's first (bump-windowed) Highlights/Shadows
/// family, before this revision. Restricted to the primary `[0, 1]` working
/// domain (still all 312 combinations, still the same dense grid density),
/// the minimum observed slope is `~0.0327`, clearing 0.02.
///
/// This is recorded here, in the design doc and in the revision's report
/// rather than resolved silently: whether to also retune Contrast's
/// `ALPHA_MAX` so the *extended*-domain bound holds too is a decision for
/// whoever owns that trade-off, not this task (which was scoped to
/// Highlights/Shadows and told to keep Contrast as it is).
#[test]
fn dense_forward_differences_prove_monotonicity_and_record_the_minimum_slope() {
    let grid = neutral_ramp(4000, -0.5, 2.0);
    assert_eq!(grid.len(), 4001);
    let samples = all_cube_samples();
    assert_eq!(samples.len(), 312);

    let mut worst_full_domain = f64::INFINITY;
    let mut worst_unit_domain = f64::INFINITY;
    for combo in &samples {
        let params = to_params(*combo);
        let mut previous: Option<(f64, f64)> = None;
        for &x in &grid {
            let y = tone_curve(x, params);
            if let Some((prev_x, prev_y)) = previous {
                assert!(
                    y >= prev_y - 1e-9,
                    "{params:?}: forward difference decreased between x={prev_x} and x={x}"
                );
                let slope = (y - prev_y) / (x - prev_x);
                worst_full_domain = worst_full_domain.min(slope);
                if prev_x >= 0.0 && x <= 1.0 {
                    worst_unit_domain = worst_unit_domain.min(slope);
                }
            }
            previous = Some((x, y));
        }
    }

    println!(
        "minimum forward-difference slope over [-0.5, 2.0], 312 combinations: {worst_full_domain}"
    );
    println!("minimum forward-difference slope over [0, 1] alone: {worst_unit_domain}");

    // The provable, honest guarantee over the full required domain: strictly
    // positive everywhere (see the doc comment above for why 0.02 does not
    // hold there, and why that is a pre-existing Contrast property).
    assert!(
        worst_full_domain > 1e-9,
        "monotonicity must hold (strictly positive slope) everywhere on the extended domain, \
         got minimum {worst_full_domain}"
    );
    // The requested 0.02 floor holds within the primary [0, 1] working domain.
    assert!(
        worst_unit_domain >= 0.02,
        "minimum slope within [0, 1] must be at least 0.02, got {worst_unit_domain}"
    );
}

/// Isolates the revised Highlights/Shadows family's own worst-case slope
/// (Contrast and Whites/Blacks held neutral), over the same extended domain
/// and the same dense grid, to show the family itself is not the bottleneck
/// in the test above.
#[test]
fn highlights_shadows_stage_alone_has_a_strong_worst_case_slope() {
    let grid = neutral_ramp(4000, -0.5, 2.0);
    let mut worst = f64::INFINITY;
    for highlights in [-100.0, -50.0, 50.0, 100.0] {
        for shadows in [-100.0, -50.0, 50.0, 100.0] {
            let params = ToneParams {
                highlights,
                shadows,
                ..ToneParams::NEUTRAL
            };
            let mut previous: Option<(f64, f64)> = None;
            for &x in &grid {
                let y = tone_curve(x, params);
                if let Some((prev_x, prev_y)) = previous {
                    let slope = (y - prev_y) / (x - prev_x);
                    worst = worst.min(slope);
                }
                previous = Some((x, y));
            }
        }
    }
    println!("Highlights/Shadows-alone minimum forward-difference slope: {worst}");
    assert!(
        worst > 0.02,
        "the revised family's own worst-case slope should be far above the bump family's \
         former ~0.0575 bound, got {worst}"
    );
}

// ---------------------------------------------------------------------------
// Smoothness
// ---------------------------------------------------------------------------

#[test]
fn bounded_second_differences_for_each_single_parameter_at_plus_minus_50_and_100() {
    let ramp = neutral_ramp(1024, 0.0, 1.0);
    let step = ramp[1] - ramp[0];
    // Each stage is built from bounded-curvature pieces (a logistic, a raised
    // cosine, a straight line), so the second difference of the sampled curve
    // is bounded by (max |f''|) * step^2, plus float noise. This threshold is
    // generous relative to that product for every stage at the tested
    // amounts; see docs/design/basic-tone.md "Smoothness" for the derivation.
    let bound = 0.01;
    let settings: [NamedSingleParam; 5] = [
        ("contrast", |v| ToneParams {
            contrast: v,
            ..ToneParams::NEUTRAL
        }),
        ("highlights", |v| ToneParams {
            highlights: v,
            ..ToneParams::NEUTRAL
        }),
        ("shadows", |v| ToneParams {
            shadows: v,
            ..ToneParams::NEUTRAL
        }),
        ("whites", |v| ToneParams {
            whites: v,
            ..ToneParams::NEUTRAL
        }),
        ("blacks", |v| ToneParams {
            blacks: v,
            ..ToneParams::NEUTRAL
        }),
    ];
    for (name, make) in settings {
        for amount in [-100.0, -50.0, 50.0, 100.0] {
            let params = make(amount);
            let values: Vec<f64> = ramp
                .iter()
                .map(|&l| tone_pixel([l, l, l], params)[0])
                .collect();
            let mut worst = 0.0f64;
            for window in values.windows(3) {
                let second_difference = window[0] - 2.0 * window[1] + window[2];
                worst = worst.max(second_difference.abs());
            }
            assert!(
                worst < bound,
                "{name}={amount}: worst second difference {worst} over step {step} exceeds {bound}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Distinct effects
// ---------------------------------------------------------------------------

// These two tests work directly in the curve's own working domain (encoded
// luminance, via `tone_curve`), not in linear light: the sRGB OETF maps linear
// 0.5 to an encoded ~0.735, so bucketing a *linear* ramp at 0.5 would split
// the highlight/shadow windows in the wrong place relative to where the
// design actually places them (curve-domain 0.5, the same pivot Contrast
// uses). Splitting by encoded value is what "above/below midtone" means here.

/// Quantitative Highlights/Shadows targets (TASK-011 revision): shadows +100
/// must lift encoded 0.10 by at least 0.12, and by the mirror symmetry,
/// highlights -100 must lower encoded 0.90 by at least 0.12. Measured with the
/// frozen K_HS = 1.5: shadows +100 lifts 0.10 to ~0.288 (delta ~0.188);
/// highlights -100 lowers 0.90 to ~0.712 (delta ~-0.188). Both comfortably
/// clear the 0.12 floor.
#[test]
fn shadows_and_highlights_meet_their_quantitative_lift_targets() {
    let shadows_lift = tone_curve(
        0.10,
        ToneParams {
            shadows: 100.0,
            ..ToneParams::NEUTRAL
        },
    ) - 0.10;
    assert!(
        shadows_lift >= 0.12,
        "shadows=100 must lift encoded 0.10 by at least 0.12, got {shadows_lift}"
    );

    let highlights_drop = tone_curve(
        0.90,
        ToneParams {
            highlights: -100.0,
            ..ToneParams::NEUTRAL
        },
    ) - 0.90;
    assert!(
        highlights_drop <= -0.12,
        "highlights=-100 must lower encoded 0.90 by at least 0.12, got {highlights_drop}"
    );
}

#[test]
fn highlights_change_the_ramp_above_midtone_far_more_than_below() {
    let ramp = neutral_ramp(1024, 0.0, 1.0);
    // The odds-bias family's own ratio, measured directly (not a target this
    // test imposes): lifting (highlights = +100) gives ~1.31x, crushing
    // (highlights = -100) gives ~2.68x. 1.2 is a safe floor below both.
    for amount in [-100.0, 100.0] {
        let params = ToneParams {
            highlights: amount,
            ..ToneParams::NEUTRAL
        };
        let mut below = 0.0f64;
        let mut above = 0.0f64;
        for &x in &ramp {
            let delta = (tone_curve(x, params) - x).abs();
            if x < 0.5 {
                below = below.max(delta);
            } else {
                above = above.max(delta);
            }
        }
        assert!(
            above > below * 1.2,
            "highlights={amount}: expected the upper half to move far more; below={below} above={above}"
        );
    }
}

#[test]
fn shadows_change_the_ramp_below_midtone_far_more_than_above() {
    let ramp = neutral_ramp(1024, 0.0, 1.0);
    // Mirror of the highlights ratio above: shadows = +100 (lift) gives
    // ~2.68x, shadows = -100 (crush) gives ~1.31x; 1.2 is a safe floor.
    for amount in [-100.0, 100.0] {
        let params = ToneParams {
            shadows: amount,
            ..ToneParams::NEUTRAL
        };
        let mut below = 0.0f64;
        let mut above = 0.0f64;
        for &x in &ramp {
            let delta = (tone_curve(x, params) - x).abs();
            if x < 0.5 {
                below = below.max(delta);
            } else {
                above = above.max(delta);
            }
        }
        assert!(
            below > above * 1.2,
            "shadows={amount}: expected the lower half to move far more; below={below} above={above}"
        );
    }
}

#[test]
fn whites_moves_the_raw_value_at_white_while_highlights_leaves_it_practically_unchanged() {
    let white = [1.0, 1.0, 1.0];
    for amount in [-100.0, -50.0, 50.0, 100.0] {
        let highlights_only = ToneParams {
            highlights: amount,
            ..ToneParams::NEUTRAL
        };
        let out = tone_pixel(white, highlights_only);
        // `highlights_stage`/`shadows_stage` fix both endpoints exactly in
        // the curve domain (see the internal_tests proof), but `encode(1.0)`
        // itself is one ULP below 1.0 in f64 (a property of the sRGB OETF's
        // floating-point evaluation, unrelated to Highlights/Shadows), so the
        // mirrored `1.0 - x` at the extreme highlights=-100 amplifies that
        // sub-ULP residual by up to exp(K_HS) before mirroring back. The
        // result is exact to about 1e-15, not bit-for-bit; 1e-9 is a safe,
        // generous margin over that.
        assert!(
            (out[0] - white[0]).abs() < 1e-9,
            "highlights={amount}: white must stay unchanged to within float noise, got {out:?}"
        );

        let whites_only = ToneParams {
            whites: amount,
            ..ToneParams::NEUTRAL
        };
        let out = tone_pixel(white, whites_only);
        assert!(
            (out[0] - 1.0).abs() > 1e-3,
            "whites={amount}: expected a measurable raw change at white, got {out:?}"
        );
    }
}

#[test]
fn blacks_moves_the_neighbourhood_of_code_zero_far_more_than_highlights_does() {
    // Unlike the earlier bump-windowed family (which was exactly zero outside
    // a fixed window), the odds-bias family has no hard window: every stage
    // has *some* effect everywhere except exactly at 0 and 1 (see
    // `odds_bias`). So Highlights does move a near-black pixel a little; the
    // claim this test proves is that Blacks moves it far more, not that
    // Highlights leaves it untouched. Measured at encoded ~0.02 (near-black):
    // Blacks' effect is 18x-107x Highlights' effect across -100/-50/50/100;
    // 5x is a safe floor.
    let near_black = [0.02, 0.02, 0.02];
    for amount in [-100.0, -50.0, 50.0, 100.0] {
        let blacks_only = ToneParams {
            blacks: amount,
            ..ToneParams::NEUTRAL
        };
        let blacks_delta = (tone_pixel(near_black, blacks_only)[0] - near_black[0]).abs();
        assert!(
            blacks_delta > 1e-4,
            "blacks={amount}: expected a measurable change near code 0, got delta {blacks_delta}"
        );

        let highlights_only = ToneParams {
            highlights: amount,
            ..ToneParams::NEUTRAL
        };
        let highlights_delta = (tone_pixel(near_black, highlights_only)[0] - near_black[0]).abs();
        assert!(
            blacks_delta > highlights_delta * 5.0,
            "amount={amount}: expected blacks to dominate near code 0; \
             blacks_delta={blacks_delta} highlights_delta={highlights_delta}"
        );
    }

    // The exact code-0 pixel, which goes through the near-black additive rule
    // rather than the luminance-ratio rule, also moves with Blacks.
    let black = [0.0, 0.0, 0.0];
    for amount in [-100.0, 100.0] {
        let out = tone_pixel(
            black,
            ToneParams {
                blacks: amount,
                ..ToneParams::NEUTRAL
            },
        );
        assert!(
            out[0].abs() > 1e-4,
            "blacks={amount}: expected code 0 to move via the near-black additive rule, got {out:?}"
        );
    }
}

#[test]
fn whites_moves_the_neighbourhood_of_white_far_more_than_shadows_does() {
    // The mirror of the test above: Shadows still moves a near-white pixel a
    // little (no hard window), but Whites moves it far more. Measured at
    // encoded ~0.9547 (near-white, linear 0.9): Whites' effect is 620x-5470x
    // Shadows' effect across -100/-50/50/100; 50x is a safe floor.
    let near_white = [0.9, 0.9, 0.9];
    for amount in [-100.0, -50.0, 50.0, 100.0] {
        let whites_only = ToneParams {
            whites: amount,
            ..ToneParams::NEUTRAL
        };
        let whites_delta = (tone_pixel(near_white, whites_only)[0] - near_white[0]).abs();
        assert!(
            whites_delta > 1e-4,
            "whites={amount}: expected a measurable change near white, got delta {whites_delta}"
        );

        let shadows_only = ToneParams {
            shadows: amount,
            ..ToneParams::NEUTRAL
        };
        let shadows_delta = (tone_pixel(near_white, shadows_only)[0] - near_white[0]).abs();
        assert!(
            whites_delta > shadows_delta * 50.0,
            "amount={amount}: expected whites to dominate near white; \
             whites_delta={whites_delta} shadows_delta={shadows_delta}"
        );
    }
}

// ---------------------------------------------------------------------------
// Hue preservation and the global (pointwise) claim
// ---------------------------------------------------------------------------

#[test]
fn hue_is_preserved_for_a_saturated_colour_away_from_the_near_black_rule() {
    let saturated = [0.82, 0.11, 0.04];
    assert!(
        luminance(saturated) > 1e-3,
        "fixture must be well clear of the near-black threshold"
    );
    let cases = [
        ToneParams {
            contrast: 80.0,
            ..ToneParams::NEUTRAL
        },
        ToneParams {
            highlights: -100.0,
            shadows: 100.0,
            ..ToneParams::NEUTRAL
        },
        ToneParams {
            whites: 60.0,
            blacks: -60.0,
            ..ToneParams::NEUTRAL
        },
        ToneParams {
            contrast: -70.0,
            highlights: 40.0,
            shadows: -40.0,
            whites: 20.0,
            blacks: -20.0,
        },
    ];
    for params in cases {
        let out = tone_pixel(saturated, params);
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            let expected_ratio = saturated[i] / saturated[j];
            let actual_ratio = out[i] / out[j];
            assert!(
                (actual_ratio - expected_ratio).abs() <= 1e-9 + 1e-9 * expected_ratio.abs(),
                "{params:?}: channel ratio ({i},{j}) drifted: expected {expected_ratio}, got {actual_ratio}"
            );
        }
    }
}

#[test]
fn equal_luminance_patches_in_different_surroundings_map_identically() {
    let patch = [0.4, 0.3, 0.1];
    let params = ToneParams {
        contrast: 40.0,
        highlights: -30.0,
        shadows: 20.0,
        whites: 10.0,
        blacks: -15.0,
    };
    let mut bright_buffer = vec![[0.9, 0.9, 0.9]; 8];
    bright_buffer.insert(3, patch);
    let mut dark_buffer = vec![[0.02, 0.02, 0.02]; 8];
    dark_buffer.insert(5, patch);

    let out_in_bright_surroundings = tone_pixel(bright_buffer[3], params);
    let out_in_dark_surroundings = tone_pixel(dark_buffer[5], params);
    assert_eq!(
        out_in_bright_surroundings, out_in_dark_surroundings,
        "a pointwise curve must map the same pixel identically regardless of its \
         surrounding pixels; this is the explicit 'global' claim the design records"
    );
}

// ---------------------------------------------------------------------------
// Oracle fixture: fixtures/basic/tone-cases.json
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct FixtureParams {
    contrast: f64,
    highlights: f64,
    shadows: f64,
    whites: f64,
    blacks: f64,
}

impl From<FixtureParams> for ToneParams {
    fn from(p: FixtureParams) -> Self {
        ToneParams {
            contrast: p.contrast,
            highlights: p.highlights,
            shadows: p.shadows,
            whites: p.whites,
            blacks: p.blacks,
        }
    }
}

impl From<ToneParams> for FixtureParams {
    fn from(p: ToneParams) -> Self {
        FixtureParams {
            contrast: p.contrast,
            highlights: p.highlights,
            shadows: p.shadows,
            whites: p.whites,
            blacks: p.blacks,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct ToneCase {
    label: String,
    input_linear_rgb: [f64; 3],
    params: FixtureParams,
    expected_linear_rgb: [f64; 3],
}

fn build_tone_cases() -> Vec<ToneCase> {
    let mut cases = Vec::new();
    let mut push = |label: String, input: [f64; 3], params: ToneParams| {
        let expected = tone_pixel(input, params);
        cases.push(ToneCase {
            label,
            input_linear_rgb: input,
            params: params.into(),
            expected_linear_rgb: expected,
        });
    };

    // Identity sanity at a handful of grey codes.
    for code in [0u8, 64, 128, 192, 255] {
        let linear = decode_srgb_u8(code);
        push(
            format!("identity/grey-code-{code}"),
            [linear, linear, linear],
            ToneParams::NEUTRAL,
        );
    }

    // Single-parameter sweeps at mid-grey (linear 0.18, close to a photographic
    // 18% grey card and squarely inside every stage's active window).
    let mid_grey = [0.18, 0.18, 0.18];
    let single_param: [NamedSingleParam; 5] = [
        ("contrast", |v| ToneParams {
            contrast: v,
            ..ToneParams::NEUTRAL
        }),
        ("highlights", |v| ToneParams {
            highlights: v,
            ..ToneParams::NEUTRAL
        }),
        ("shadows", |v| ToneParams {
            shadows: v,
            ..ToneParams::NEUTRAL
        }),
        ("whites", |v| ToneParams {
            whites: v,
            ..ToneParams::NEUTRAL
        }),
        ("blacks", |v| ToneParams {
            blacks: v,
            ..ToneParams::NEUTRAL
        }),
    ];
    for (name, make) in single_param {
        for amount in [-100.0, -50.0, 50.0, 100.0] {
            push(
                format!("single/{name}/{amount}/mid-grey"),
                mid_grey,
                make(amount),
            );
        }
    }

    // Region cases, chosen per control so each shows both a real effect and a
    // boundary the design proves exactly: Highlights and Shadows are each
    // exactly zero just past their own window (so pure white/black and near
    // it are provably untouched), while Whites and Blacks are a global
    // endpoint remap and move every input, including the far end from the one
    // named after it.
    let near_white = [0.9, 0.9, 0.9];
    let near_black = [0.02, 0.02, 0.02];
    let upper_tone = [0.6, 0.6, 0.6]; // encodes to ~0.80, inside the highlight window's peak
    let lower_tone = near_black; // encodes to ~0.15, inside the shadow window's peak

    for (region, input) in [("upper-tone", upper_tone), ("near-black", near_black)] {
        for amount in [-100.0, 100.0] {
            push(
                format!("region/highlights/{amount}/{region}"),
                input,
                single_param[1].1(amount),
            );
        }
    }
    for (region, input) in [("near-white", near_white), ("lower-tone", lower_tone)] {
        for amount in [-100.0, 100.0] {
            push(
                format!("region/shadows/{amount}/{region}"),
                input,
                single_param[2].1(amount),
            );
        }
    }
    for (region, input) in [("near-white", near_white), ("near-black", near_black)] {
        for (name, make) in [("whites", single_param[3].1), ("blacks", single_param[4].1)] {
            for amount in [-100.0, 100.0] {
                push(
                    format!("region/{name}/{amount}/{region}"),
                    input,
                    make(amount),
                );
            }
        }
    }

    // Combined extremes.
    let saturated = [0.82, 0.11, 0.04];
    for (label, params) in [
        (
            "all-plus-100",
            ToneParams {
                contrast: 100.0,
                highlights: 100.0,
                shadows: 100.0,
                whites: 100.0,
                blacks: 100.0,
            },
        ),
        (
            "all-minus-100",
            ToneParams {
                contrast: -100.0,
                highlights: -100.0,
                shadows: -100.0,
                whites: -100.0,
                blacks: -100.0,
            },
        ),
        (
            "mixed-signs",
            ToneParams {
                contrast: -70.0,
                highlights: 40.0,
                shadows: -40.0,
                whites: 20.0,
                blacks: -20.0,
            },
        ),
    ] {
        push(format!("combined/{label}/mid-grey"), mid_grey, params);
        push(format!("combined/{label}/saturated"), saturated, params);
    }

    // Near-black exact-zero via the additive rule.
    for amount in [-100.0, 100.0] {
        push(
            format!("near-black-additive-rule/blacks/{amount}"),
            [0.0, 0.0, 0.0],
            ToneParams {
                blacks: amount,
                ..ToneParams::NEUTRAL
            },
        );
    }

    cases
}

/// A JSON text/parse round trip is not guaranteed bit-for-bit identical to the
/// `f64` a fresh computation produces (the written text is the shortest
/// string that round-trips to the written value, but the parser's correctly-
/// rounded result can land one ULP away in rare cases). `1e-12` relative is
/// four orders of magnitude tighter than the production-vs-reference
/// tolerance this fixture exists to police, so it still catches any real
/// staleness while tolerating that round trip.
const FIXTURE_ROUND_TRIP_TOLERANCE: f64 = 1e-12;

fn approximately_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= FIXTURE_ROUND_TRIP_TOLERANCE + FIXTURE_ROUND_TRIP_TOLERANCE * b.abs()
}

#[test]
fn committed_tone_case_fixture_matches_the_reference() {
    let path = fixture_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let committed: Vec<ToneCase> =
        serde_json::from_str(&contents).expect("fixtures/basic/tone-cases.json must be valid JSON");
    let fresh = build_tone_cases();
    assert_eq!(
        committed.len(),
        fresh.len(),
        "the committed fixture has a different case count than the reference produces; \
         regenerate it with `cargo test --test basic_tone_reference -- --ignored \
         regenerate_committed_tone_case_fixture`"
    );
    for (committed_case, fresh_case) in committed.iter().zip(fresh.iter()) {
        assert_eq!(
            committed_case.label, fresh_case.label,
            "fixtures/basic/tone-cases.json case order or labels drifted; regenerate it"
        );
        assert_eq!(
            committed_case.params, fresh_case.params,
            "fixtures/basic/tone-cases.json is stale for case {:?}; regenerate it",
            committed_case.label
        );
        for (a, b) in committed_case
            .input_linear_rgb
            .iter()
            .zip(fresh_case.input_linear_rgb.iter())
            .chain(
                committed_case
                    .expected_linear_rgb
                    .iter()
                    .zip(fresh_case.expected_linear_rgb.iter()),
            )
        {
            assert!(
                approximately_equal(*a, *b),
                "fixtures/basic/tone-cases.json is stale for case {:?}: committed {a}, reference {b}; regenerate it",
                committed_case.label
            );
        }
    }
}

#[test]
#[ignore = "regenerates the committed oracle fixture; run explicitly after changing the frozen \
            equations in tests/reference/tone.rs, and re-freeze docs/design/basic-tone.md to match"]
fn regenerate_committed_tone_case_fixture() {
    let cases = build_tone_cases();
    let json = serde_json::to_string_pretty(&cases).expect("serializable");
    std::fs::write(fixture_path(), json).expect("write fixtures/basic/tone-cases.json");
}

// ---------------------------------------------------------------------------
// Visual review (TASK-011 deliverable 3): writes PNGs to a temp directory for
// manual inspection. Ignored by default; never part of `cargo test` or
// `cargo xtask check`. No image is committed; delete the temp directory after
// looking at its contents.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "writes PNGs to a temp dir for manual visual review; not part of cargo xtask check"]
fn visual_review_writes_tone_sweeps_to_a_temp_dir() {
    use image::{RgbImage, open};

    let out_dir = std::env::temp_dir().join("lightwell-tone-visual-review");
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    fn apply_to_linear_buffer(
        pixels: &[[f64; 3]],
        width: u32,
        height: u32,
        params: ToneParams,
    ) -> RgbImage {
        let mut image = RgbImage::new(width, height);
        for (pixel_in, pixel_out) in pixels.iter().zip(image.pixels_mut()) {
            let out = tone_pixel(*pixel_in, params);
            *pixel_out = image::Rgb([
                encode_srgb_u8(out[0]),
                encode_srgb_u8(out[1]),
                encode_srgb_u8(out[2]),
            ]);
        }
        image
    }

    fn apply_to_srgb_image(source: &RgbImage, params: ToneParams) -> RgbImage {
        let mut out = RgbImage::new(source.width(), source.height());
        for (pixel_in, pixel_out) in source.pixels().zip(out.pixels_mut()) {
            let linear = [
                decode_srgb_u8(pixel_in[0]),
                decode_srgb_u8(pixel_in[1]),
                decode_srgb_u8(pixel_in[2]),
            ];
            let result = tone_pixel(linear, params);
            *pixel_out = image::Rgb([
                encode_srgb_u8(result[0]),
                encode_srgb_u8(result[1]),
                encode_srgb_u8(result[2]),
            ]);
        }
        out
    }

    // A smooth linear-light step wedge, so posterization and hue shift are
    // easy to see across the whole tonal range.
    let (wedge_w, wedge_h) = (512u32, 96u32);
    let mut wedge = Vec::with_capacity((wedge_w * wedge_h) as usize);
    for _ in 0..wedge_h {
        for x in 0..wedge_w {
            let l = f64::from(x) / f64::from(wedge_w - 1);
            wedge.push([l, l, l]);
        }
    }

    // A backlit synthetic subject: a bright sky-like background and a dark,
    // slightly noisy foreground disc, so shadow lift, highlight recovery and
    // local-contrast flattening are all visible in one frame.
    let (photo_w, photo_h) = (480u32, 320u32);
    let mut rng = SplitMix64(7);
    let mut backlit = Vec::with_capacity((photo_w * photo_h) as usize);
    let (cx, cy) = (f64::from(photo_w) * 0.5, f64::from(photo_h) * 0.58);
    let radius = f64::from(photo_w.min(photo_h)) * 0.3;
    for y in 0..photo_h {
        for x in 0..photo_w {
            let (dx, dy) = (f64::from(x) - cx, f64::from(y) - cy);
            let rgb = if (dx * dx + dy * dy).sqrt() < radius {
                let noise = rng.next_range(-0.012, 0.012);
                let l = (0.035 + noise).max(0.0005);
                [l, l * 0.95, l * 0.88]
            } else {
                let l = 0.72 + 0.18 * (f64::from(y) / f64::from(photo_h));
                [l, l * 0.99, l * 0.93]
            };
            backlit.push(rgb);
        }
    }

    let sweeps: [(&str, ToneParams); 11] = [
        ("neutral", ToneParams::NEUTRAL),
        (
            "contrast+100",
            ToneParams {
                contrast: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "contrast-100",
            ToneParams {
                contrast: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "highlights+100",
            ToneParams {
                highlights: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "highlights-100",
            ToneParams {
                highlights: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "shadows+100",
            ToneParams {
                shadows: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "shadows-100",
            ToneParams {
                shadows: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "whites+100",
            ToneParams {
                whites: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "whites-100",
            ToneParams {
                whites: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "blacks+100",
            ToneParams {
                blacks: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "blacks-100",
            ToneParams {
                blacks: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
    ];

    for (name, params) in sweeps {
        apply_to_linear_buffer(&wedge, wedge_w, wedge_h, params)
            .save(out_dir.join(format!("wedge-{name}.png")))
            .expect("save wedge png");
        apply_to_linear_buffer(&backlit, photo_w, photo_h, params)
            .save(out_dir.join(format!("backlit-{name}.png")))
            .expect("save backlit png");
    }

    let source =
        open(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"))
            .expect("open orientation-1.jpg")
            .to_rgb8();
    for (name, params) in [
        ("neutral", ToneParams::NEUTRAL),
        (
            "shadows+100",
            ToneParams {
                shadows: 100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "highlights-100",
            ToneParams {
                highlights: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "blacks-100",
            ToneParams {
                blacks: -100.0,
                ..ToneParams::NEUTRAL
            },
        ),
        (
            "contrast+60",
            ToneParams {
                contrast: 60.0,
                ..ToneParams::NEUTRAL
            },
        ),
    ] {
        apply_to_srgb_image(&source, params)
            .save(out_dir.join(format!("orientation-1-{name}.png")))
            .expect("save orientation-1 png");
    }

    println!("wrote visual review PNGs to {}", out_dir.display());
}
