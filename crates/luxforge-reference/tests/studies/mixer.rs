//! Property proofs, dense measurements and fixtures for the frozen colour
//! mixer reference in `crates/luxforge-reference/src/mixer.rs`.
//!
//! Every figure quoted in `docs/design/mixer-study.md` is produced here. The
//! bounds asserted below are the frozen ones; the exact measured values are
//! printed by the ignored `mixer_study_figures` test:
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies \
//!     -- --ignored --nocapture mixer_study_figures
//! ```
//!
//! `fixtures/mixer/mixer-cases.json` is committed. It was produced by running
//! the ignored generator once:
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies \
//!     -- --ignored generate_mixer_fixtures
//! ```
//!
//! `mixer_fixtures_match_reference` (not ignored) reloads the committed file
//! and recomputes every case with the same reference, so a silent drift
//! between the file and the frozen formulas fails the build.

use luxforge_reference::colour::{self, Oklab};
use luxforge_reference::mixer::{
    self, CENTRE_HUES_DEG, CHROMA_RAMP_EDGE, HUE_REACH, HueWarp, MixerParams, RANGE_COUNT,
    RANGE_NAMES, RANGE_REFERENCE_CODES,
};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::fs;
use std::path::PathBuf;

/// Decoded from the published sRGB constants directly, matching
/// `luxforge_reference::srgb_decode`, so the study's inputs do not go through the reference it
/// checks.
fn code_to_linear(code: u8) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb8_linear(rgb: [u8; 3]) -> [f64; 3] {
    [
        code_to_linear(rgb[0]),
        code_to_linear(rgb[1]),
        code_to_linear(rgb[2]),
    ]
}

fn grey_linear(code: u8) -> [f64; 3] {
    let l = code_to_linear(code);
    [l, l, l]
}

/// A linear-sRGB colour placed at an exact Oklab `(L, C, h)`, used for the hue
/// wheel. The result may lie outside the sRGB gamut, which the reference
/// accepts by contract.
fn wheel_colour(l: f64, chroma: f64, hue_deg: f64) -> [f64; 3] {
    let radians = hue_deg.to_radians();
    colour::from_oklab(Oklab {
        l,
        a: chroma * radians.cos(),
        b: chroma * radians.sin(),
    })
}

fn max_abs_difference(left: [f64; 3], right: [f64; 3]) -> f64 {
    (0..3).fold(0.0_f64, |worst, i| worst.max((left[i] - right[i]).abs()))
}

// ---------------------------------------------------------------------------
// 1. The basis: centres, gaps, partition of unity, continuity
// ---------------------------------------------------------------------------

/// The frozen centre constants are the Oklab hues of the eight sRGB reference
/// colours, recomputed here from the 8-bit codes through the accepted colour
/// study's conversion.
#[test]
fn centres_match_the_reference_colours() {
    for (range, codes) in RANGE_REFERENCE_CODES.iter().enumerate() {
        let lab = colour::to_oklab(srgb8_linear(*codes));
        let measured = mixer::normalize_hue_deg(colour::hue_degrees(lab));
        let frozen = CENTRE_HUES_DEG[range];
        assert!(
            (measured - frozen).abs() < 1e-9,
            "{}: frozen centre {frozen} but {codes:?} measures {measured}",
            RANGE_NAMES[range]
        );
    }
}

/// The eight gaps are positive, sum to exactly one turn, and the frozen
/// extremes hold: the narrowest is red -> orange and the widest aqua -> blue.
#[test]
fn hue_gaps_cover_the_wheel_once() {
    let gaps: Vec<f64> = (0..RANGE_COUNT).map(mixer::hue_gap_deg).collect();
    let total: f64 = gaps.iter().sum();
    assert!(
        (total - 360.0).abs() < 1e-9,
        "the eight gaps sum to {total}, not one turn"
    );
    assert!(gaps.iter().all(|gap| *gap > 0.0), "gaps: {gaps:?}");

    let narrowest = gaps.iter().cloned().fold(f64::INFINITY, f64::min);
    let widest = gaps.iter().cloned().fold(0.0_f64, f64::max);
    assert!(
        (narrowest - 23.750_794_401_6).abs() < 1e-6,
        "narrowest gap drifted: {narrowest}"
    );
    assert!(
        (widest - 69.283_072_706_1).abs() < 1e-6,
        "widest gap drifted: {widest}"
    );
}

/// The weights are a partition of unity: exactly `1.0` in f64 at every sampled
/// hue, each weight in `[0, 1]`, at most two non-zero, and exactly `1.0` for
/// range `i` at centre `i`.
#[test]
fn weights_are_a_partition_of_unity() {
    let samples = 200_000;
    for step in 0..=samples {
        let hue = 360.0 * f64::from(step) / f64::from(samples);
        let weights = mixer::range_weights(hue);
        let sum: f64 = weights.iter().sum();
        assert_eq!(sum, 1.0, "weights at {hue} deg sum to {sum}: {weights:?}");
        assert!(
            weights.iter().all(|w| (0.0..=1.0).contains(w)),
            "weight outside [0, 1] at {hue} deg: {weights:?}"
        );
        let non_zero = weights.iter().filter(|w| **w != 0.0).count();
        assert!(
            non_zero <= 2,
            "{non_zero} ranges influence {hue} deg: {weights:?}"
        );
    }

    for (range, centre) in CENTRE_HUES_DEG.iter().enumerate() {
        let weights = mixer::range_weights(*centre);
        assert_eq!(
            weights[range], 1.0,
            "{} is not exactly 1 at its own centre: {weights:?}",
            RANGE_NAMES[range]
        );
    }

    // The seam: 360 - eps, 0 and +eps agree.
    let before = mixer::range_weights(359.999_999);
    let at = mixer::range_weights(0.0);
    let after = mixer::range_weights(0.000_001);
    for range in 0..RANGE_COUNT {
        assert!(
            (before[range] - at[range]).abs() < 1e-6 && (at[range] - after[range]).abs() < 1e-6,
            "weights jump across the 360 degree seam for {}",
            RANGE_NAMES[range]
        );
    }
}

/// Weight continuity, measured as the largest change between neighbouring
/// samples of a dense sweep and compared against the analytic Lipschitz bound
/// `pi / (2 * narrowest gap)` per degree.
#[test]
fn weights_are_continuous_around_the_wheel() {
    let samples = 200_000;
    let step_deg = 360.0 / f64::from(samples);
    let mut worst = 0.0_f64;
    let mut previous = mixer::range_weights(0.0);
    for step in 1..=samples {
        let weights = mixer::range_weights(360.0 * f64::from(step) / f64::from(samples));
        for range in 0..RANGE_COUNT {
            worst = worst.max((weights[range] - previous[range]).abs());
        }
        previous = weights;
    }
    let narrowest = (0..RANGE_COUNT)
        .map(mixer::hue_gap_deg)
        .fold(f64::INFINITY, f64::min);
    let bound = PI / (2.0 * narrowest) * step_deg;
    assert!(
        worst <= bound * 1.001,
        "largest weight jump {worst} over a {step_deg} deg step exceeds the analytic bound {bound}"
    );
}

// ---------------------------------------------------------------------------
// 2. Identity
// ---------------------------------------------------------------------------

/// The neutral payload is the exact identity **in Oklab**: `mix` with every
/// slider at zero reproduces the unit's Oklab round trip bit for bit —
/// `to_oklab`, then [`mixer::reconstruct`], which is the plain `from_oklab`
/// for every colour but an exactly achromatic one — so the only difference
/// from the input is the conversion's own round-trip residual.
#[test]
fn neutral_parameters_are_bit_exactly_the_oklab_round_trip() {
    let neutral = MixerParams::neutral();
    let steps = 21;
    let sample = |i: i32| -0.2 + 1.7 * f64::from(i) / f64::from(steps - 1);
    for ri in 0..steps {
        for gi in 0..steps {
            for bi in 0..steps {
                let rgb = [sample(ri), sample(gi), sample(bi)];
                let mixed = mixer::mix(rgb, &neutral);
                let round_trip = mixer::reconstruct(colour::to_oklab(rgb));
                assert_eq!(
                    mixed, round_trip,
                    "the neutral payload is not bit-exactly the round trip for {rgb:?}"
                );
            }
        }
    }
}

/// The identity tolerance: with every slider at zero the reference differs
/// from its input only by the Oklab conversion's own round-trip residual,
/// which `basic-colour.md` measures at the ~1e-7 order over the same sweep.
#[test]
fn identity_residual_is_the_oklab_round_trip_residual() {
    let neutral = MixerParams::neutral();
    let steps = 21;
    let sample = |i: i32| -0.2 + 1.7 * f64::from(i) / f64::from(steps - 1);
    let mut worst = 0.0_f64;
    for ri in 0..steps {
        for gi in 0..steps {
            for bi in 0..steps {
                let rgb = [sample(ri), sample(gi), sample(bi)];
                worst = worst.max(max_abs_difference(mixer::mix(rgb, &neutral), rgb));
            }
        }
    }
    assert!(
        worst < 2e-6,
        "identity residual {worst} exceeds the documented ~1e-7 order"
    );
    assert!(
        worst > 0.0,
        "a zero residual would mean the Oklab matrices are bit-exact inverses, which basic-colour.md does not claim"
    );
}

/// The luminance response is the exact identity at `m = 0` for every sampled
/// `L`, including out-of-range ones.
#[test]
fn luminance_response_is_the_exact_identity_at_zero() {
    for step in -200..=300 {
        let l = f64::from(step) / 200.0;
        assert_eq!(
            mixer::luminance_response(l, 0.0),
            l,
            "luminance response moved L = {l} with the slider at zero"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Achromatic and zero-weight invariance
// ---------------------------------------------------------------------------

/// Every grey code is unchanged by every slider at +/-100, and by the two
/// worst combined extremes, within the conversion round trip.
#[test]
fn achromatic_ramp_is_invariant_under_every_slider() {
    let mut worst = 0.0_f64;
    for code in 0u8..=255 {
        let rgb = grey_linear(code);
        for range in 0..RANGE_COUNT {
            for value in [-100.0, 100.0] {
                for params in [
                    MixerParams::with_hue(range, value),
                    MixerParams::with_saturation(range, value),
                    MixerParams::with_luminance(range, value),
                ] {
                    worst = worst.max(max_abs_difference(mixer::mix(rgb, &params), rgb));
                }
            }
        }
        for sign in [1.0, -1.0] {
            let params = MixerParams {
                hue: [100.0 * sign; RANGE_COUNT],
                saturation: [-100.0 * sign; RANGE_COUNT],
                luminance: [100.0 * sign; RANGE_COUNT],
            };
            worst = worst.max(max_abs_difference(mixer::mix(rgb, &params), rgb));
        }
    }
    assert!(
        worst < 2e-6,
        "a grey moved by {worst}, more than the Oklab round-trip residual"
    );
}

/// The chroma ramp is exactly zero on the achromatic axis and exactly one at
/// and above its edge, and the measured chroma of a true grey is far enough
/// below the edge that the arbitrary hue of that numerical noise cannot matter.
#[test]
fn chroma_ramp_pins_the_achromatic_axis() {
    assert_eq!(mixer::chroma_ramp(0.0), 0.0);
    assert_eq!(mixer::chroma_ramp(CHROMA_RAMP_EDGE), 1.0);
    assert_eq!(mixer::chroma_ramp(1.0), 1.0);

    let mut worst_grey_chroma = 0.0_f64;
    for code in 0u8..=255 {
        worst_grey_chroma =
            worst_grey_chroma.max(colour::chroma(colour::to_oklab(grey_linear(code))));
    }
    assert!(
        worst_grey_chroma < 1e-6,
        "grey chroma noise floor {worst_grey_chroma} is larger than expected"
    );
    assert!(
        mixer::chroma_ramp(worst_grey_chroma) < 1e-9,
        "the ramp gives a true grey a weight of {}",
        mixer::chroma_ramp(worst_grey_chroma)
    );
}

/// A colour outside a slider's support is bit-identical under every setting of
/// it. A range's weight is exactly zero two centres away, so its saturation
/// and luminance sliders are checked there; its hue slider warps one segment
/// further each side (see
/// `a_single_hue_slider_warps_only_the_four_segments_around_its_centre`), so it
/// is checked three centres away.
#[test]
fn zero_weight_colours_are_bit_identical_under_that_range() {
    let neutral_output = |rgb: [f64; 3]| mixer::mix(rgb, &MixerParams::neutral());
    for range in 0..RANGE_COUNT {
        for (offset, far_offset) in [(2usize, 3usize), (RANGE_COUNT - 2, RANGE_COUNT - 3)] {
            let other = (range + offset) % RANGE_COUNT;
            let rgb = srgb8_linear(RANGE_REFERENCE_CODES[other]);
            let weights = mixer::range_weights(colour::hue_degrees(colour::to_oklab(rgb)));
            assert_eq!(
                weights[range], 0.0,
                "{} has a non-zero weight at the {} centre",
                RANGE_NAMES[range], RANGE_NAMES[other]
            );
            let far = (range + far_offset) % RANGE_COUNT;
            let far_rgb = srgb8_linear(RANGE_REFERENCE_CODES[far]);
            for value in [-100.0, -37.5, 100.0] {
                for (params, colour_rgb, name) in [
                    (
                        MixerParams::with_hue(range, value),
                        far_rgb,
                        RANGE_NAMES[far],
                    ),
                    (
                        MixerParams::with_saturation(range, value),
                        rgb,
                        RANGE_NAMES[other],
                    ),
                    (
                        MixerParams::with_luminance(range, value),
                        rgb,
                        RANGE_NAMES[other],
                    ),
                ] {
                    assert_eq!(
                        mixer::mix(colour_rgb, &params),
                        neutral_output(colour_rgb),
                        "{} at {value} moved the {name} centre",
                        RANGE_NAMES[range],
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Hue: the monotone warp
// ---------------------------------------------------------------------------

/// Every combination of the eight hue sliders at `-100`, `-50`, `0`, `+50`
/// and `+100`: the 5^8 = 390 625 settings the exhaustive proofs below sweep.
fn five_level_combinations() -> impl Iterator<Item = MixerParams> {
    const LEVELS: [f64; 5] = [-100.0, -50.0, 0.0, 50.0, 100.0];
    (0..5usize.pow(RANGE_COUNT as u32)).map(|combination| {
        let mut code = combination;
        let mut params = MixerParams::neutral();
        for range in 0..RANGE_COUNT {
            params.hue[range] = LEVELS[code % 5];
            code /= 5;
        }
        params
    })
}

/// The hue sliders at full strength, all the same way.
fn all_hue(value: f64) -> MixerParams {
    MixerParams {
        hue: [value; RANGE_COUNT],
        ..MixerParams::neutral()
    }
}

/// Range `range` at `+100` and its next neighbour at `-100`: the two centres
/// driven at each other.
fn opposing_pair(range: usize) -> MixerParams {
    let mut params = MixerParams::neutral();
    params.hue[range] = 100.0;
    params.hue[(range + 1) % RANGE_COUNT] = -100.0;
    params
}

/// `+100` on a range moves that range's own centre colour `HUE_REACH` of the
/// way to the neighbouring centre, and `-100` the same fraction of the way to
/// the other neighbour, measured on the reference colour through `mix`.
#[test]
fn full_hue_slider_moves_a_centre_the_reach_of_the_way_to_its_neighbour() {
    for range in 0..RANGE_COUNT {
        let rgb = srgb8_linear(RANGE_REFERENCE_CODES[range]);
        let start = mixer::normalize_hue_deg(colour::hue_degrees(colour::to_oklab(rgb)));
        for (value, positive) in [(100.0, true), (-100.0, false)] {
            let params = MixerParams::with_hue(range, value);
            let moved = mixer::mix(rgb, &params);
            let end = mixer::normalize_hue_deg(colour::hue_degrees(colour::to_oklab(moved)));
            let travelled = if positive {
                mixer::normalize_hue_deg(end - start)
            } else {
                mixer::normalize_hue_deg(start - end)
            };
            let expected = HUE_REACH * mixer::travel_gap_deg(range, positive);
            assert!(
                (travelled - expected).abs() < 1e-4,
                "{} at {value}: travelled {travelled} deg, expected {expected}",
                RANGE_NAMES[range]
            );
        }
    }
}

/// The warp passes through its knots, every secant keeps at least
/// `1 - HUE_REACH` after the limiting rule, and every knot slope is positive
/// and at most three times the smaller neighbouring secant — the
/// Fritsch-Carlson condition that makes each cubic segment strictly
/// increasing — for all 5^8 five-level combinations.
#[test]
fn hue_warp_satisfies_the_fritsch_carlson_conditions_for_every_combination() {
    let floor = 1.0 - HUE_REACH;
    for params in five_level_combinations() {
        let warp = HueWarp::new(&params);
        for range in 0..RANGE_COUNT {
            let next = (range + 1) % RANGE_COUNT;
            let secant = mixer::segment_secant(&warp.displacement, range);
            assert!(
                secant >= floor - 1e-12,
                "{params:?}: segment {range} secant {secant} is below {floor}"
            );
            for slope in [warp.slope[range], warp.slope[next]] {
                assert!(
                    slope > 0.0 && slope <= 3.0 * secant * (1.0 + 1e-12),
                    "{params:?}: segment {range} knot slope {slope} against secant {secant}"
                );
            }
        }
        for (range, centre) in CENTRE_HUES_DEG.iter().enumerate() {
            let displaced = warp.displacement_deg(*centre);
            assert!(
                (displaced - warp.displacement[range]).abs() < 1e-9,
                "{params:?}: D at centre {range} is {displaced}, knot {}",
                warp.displacement[range]
            );
        }
    }
}

/// The hue map is strictly increasing — no two input hues collapse onto one
/// output hue — for every one of the 5^8 five-level combinations, by the
/// exact per-segment minimum of `dH/dh`, and for 50 000 pseudo-random
/// whole-number combinations, which also meet the Fritsch-Carlson conditions
/// strictly. The floor over the five-level sweep is frozen so a later change
/// cannot make it silently worse.
#[test]
fn hue_map_is_strictly_monotone_for_every_slider_combination() {
    let mut worst = f64::INFINITY;
    let mut worst_params = MixerParams::neutral();
    for params in five_level_combinations() {
        let floor = HueWarp::new(&params).minimum_slope();
        if floor < worst {
            worst = floor;
            worst_params = params;
        }
    }
    assert!(
        worst > 0.0,
        "the hue map folds for {worst_params:?}: slope {worst}"
    );
    assert!(
        (worst - FROZEN_LATTICE_SLOPE_FLOOR).abs() < 1e-9,
        "the five-level slope floor moved to {worst} for {worst_params:?}"
    );

    for params in random_hue_combinations(50_000) {
        let warp = HueWarp::new(&params);
        for range in 0..RANGE_COUNT {
            let next = (range + 1) % RANGE_COUNT;
            let secant = mixer::segment_secant(&warp.displacement, range);
            assert!(
                secant >= 1.0 - HUE_REACH - 1e-12,
                "{params:?}: secant {secant}"
            );
            for slope in [warp.slope[range], warp.slope[next]] {
                assert!(
                    slope > 0.0 && slope < 3.0 * secant,
                    "{params:?}: knot slope {slope} against secant {secant}"
                );
            }
        }
        let floor = warp.minimum_slope();
        assert!(floor > 0.0, "{params:?} folds: slope floor {floor}");
    }
}

/// Pseudo-random whole-number settings of the eight hue sliders, each in
/// `[-100, 100]`, from a fixed xorshift seed.
fn random_hue_combinations(count: usize) -> impl Iterator<Item = MixerParams> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..count).map(move |_| {
        let mut params = MixerParams::neutral();
        for slot in &mut params.hue {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *slot = ((state >> 11) % 201) as f64 - 100.0;
        }
        params
    })
}

/// The slope floor over all 5^8 five-level combinations, measured by
/// `mixer_study_figures` and quoted in the study note.
const FROZEN_LATTICE_SLOPE_FLOOR: f64 = 0.063_955_987_6;

/// The exact per-segment minimum agrees with a dense sweep of the warp for
/// single sliders, all-same-direction settings and opposing neighbours, and
/// every one is positive.
#[test]
fn analytic_slope_floor_matches_a_dense_sweep() {
    let mut cases: Vec<MixerParams> = Vec::new();
    for range in 0..RANGE_COUNT {
        for value in [-100.0, -50.0, 100.0] {
            cases.push(MixerParams::with_hue(range, value));
        }
        cases.push(opposing_pair(range));
    }
    cases.push(all_hue(100.0));
    cases.push(all_hue(-100.0));
    for params in cases {
        let analytic = HueWarp::new(&params).minimum_slope();
        let swept = minimum_hue_slope(&params, 72_000);
        assert!(analytic > 0.0, "{params:?}: slope floor {analytic}");
        assert!(
            (analytic - swept).abs() < 2e-3,
            "{params:?}: analytic floor {analytic}, swept {swept}"
        );
    }
}

/// Two neighbours driven at each other share one slider's travel: at
/// `+100`/`-100` each centre moves half of `HUE_REACH` of their gap, the gap
/// keeps exactly `1 - HUE_REACH` of its width, and the map stays strictly
/// increasing. Before the limiting rule the two centres would cross.
#[test]
fn opposing_neighbours_share_one_sliders_travel_and_never_cross() {
    for (range, name) in RANGE_NAMES.iter().enumerate() {
        let next = (range + 1) % RANGE_COUNT;
        let gap = mixer::hue_gap_deg(range);
        let warp = HueWarp::new(&opposing_pair(range));
        assert!(
            (warp.displacement[range] - 0.5 * HUE_REACH * gap).abs() < 1e-12
                && (warp.displacement[next] + 0.5 * HUE_REACH * gap).abs() < 1e-12,
            "{name} against {}: displacements {:?}",
            RANGE_NAMES[next],
            warp.displacement
        );
        let separation = gap + warp.displacement[next] - warp.displacement[range];
        assert!(
            (separation - (1.0 - HUE_REACH) * gap).abs() < 1e-12,
            "{name} against {}: separation {separation}",
            RANGE_NAMES[next]
        );
        assert!(warp.minimum_slope() > 0.0);
    }
}

/// A single hue slider moves its own centre and changes the knot slopes there
/// and at its two neighbours, so its warp is confined to the arc between the
/// centres two ranges either side: every hue outside it is displaced by
/// exactly zero, and just inside it the displacement is not zero.
#[test]
fn a_single_hue_slider_warps_only_the_four_segments_around_its_centre() {
    for (range, name) in RANGE_NAMES.iter().enumerate() {
        for value in [-100.0, 40.0, 100.0] {
            let warp = HueWarp::new(&MixerParams::with_hue(range, value));
            for (segment, centre) in CENTRE_HUES_DEG.iter().enumerate() {
                let offset = (segment + RANGE_COUNT - range) % RANGE_COUNT;
                let inside = matches!(offset, 0 | 1 | 6 | 7);
                for step in 1..100 {
                    let hue = centre + mixer::hue_gap_deg(segment) * f64::from(step) / 100.0;
                    let displaced = warp.displacement_deg(hue);
                    if !inside {
                        assert_eq!(
                            displaced, 0.0,
                            "{name} at {value} displaced {hue} deg in segment {segment}"
                        );
                    } else if step == 50 {
                        assert!(
                            displaced != 0.0,
                            "{name} at {value} left the middle of segment {segment} unmoved"
                        );
                    }
                }
            }
        }
    }
}

/// The smallest `d(output hue) / d(input hue)` over a dense sweep of the wheel
/// at full chroma weight. Values above zero mean the hue map is injective.
fn minimum_hue_slope(params: &MixerParams, samples: u32) -> f64 {
    let warp = HueWarp::new(params);
    let step = 360.0 / f64::from(samples);
    let shifted = |hue: f64| hue + warp.displacement_deg(hue);
    let mut worst = f64::INFINITY;
    let mut previous = shifted(0.0);
    for step_index in 1..=samples {
        let hue = 360.0 * f64::from(step_index) / f64::from(samples);
        let current = shifted(hue);
        worst = worst.min((current - previous) / step);
        previous = current;
    }
    worst
}

/// Output continuity around the wheel under a deliberately hostile hue
/// setting (alternating +/-100), at several chroma levels: neighbouring
/// samples of a dense wheel stay close in linear sRGB.
#[test]
fn output_is_continuous_around_the_wheel_under_a_strong_hue_shift() {
    let params = MixerParams {
        hue: [100.0, -100.0, 100.0, -100.0, 100.0, -100.0, 100.0, -100.0],
        saturation: [50.0; RANGE_COUNT],
        luminance: [30.0; RANGE_COUNT],
    };
    let samples = 7_200;
    for chroma in [0.05, 0.10, 0.20, 0.30] {
        let mut worst = 0.0_f64;
        let mut previous = mixer::mix(wheel_colour(0.6, chroma, 0.0), &params);
        for step in 1..=samples {
            let hue = 360.0 * f64::from(step) / f64::from(samples);
            let current = mixer::mix(wheel_colour(0.6, chroma, hue), &params);
            worst = worst.max(max_abs_difference(current, previous));
            previous = current;
        }
        assert!(
            worst < 0.01,
            "chroma {chroma}: neighbouring wheel samples jumped by {worst} in linear sRGB"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Saturation
// ---------------------------------------------------------------------------

/// `-100` on a range takes that range's own centre to exactly zero chroma, and
/// `+100` doubles it.
#[test]
fn saturation_extremes_zero_and_double_a_centre_colour() {
    for range in 0..RANGE_COUNT {
        let rgb = srgb8_linear(RANGE_REFERENCE_CODES[range]);
        let lab = colour::to_oklab(rgb);
        let start = colour::chroma(lab);

        let killed = mixer::amounts(lab, &MixerParams::with_saturation(range, -100.0));
        assert_eq!(
            killed.chroma_factor, 0.0,
            "{}: saturation -100 left a chroma factor of {}",
            RANGE_NAMES[range], killed.chroma_factor
        );
        let grey = mixer::mix(rgb, &MixerParams::with_saturation(range, -100.0));
        let spread = (grey[0] - grey[1])
            .abs()
            .max((grey[1] - grey[2]).abs())
            .max((grey[0] - grey[2]).abs());
        assert!(
            spread < 1e-9,
            "{}: saturation -100 left a colour cast {grey:?} (spread {spread})",
            RANGE_NAMES[range]
        );

        let doubled = mixer::amounts(lab, &MixerParams::with_saturation(range, 100.0));
        assert_eq!(
            doubled.chroma_factor, 2.0,
            "{}: saturation +100 gave a chroma factor of {}",
            RANGE_NAMES[range], doubled.chroma_factor
        );
        assert!(
            start > CHROMA_RAMP_EDGE,
            "{} is not chromatic",
            RANGE_NAMES[range]
        );
    }
}

/// The chroma factor is never negative, for any combination of the eight
/// saturation sliders at any hue and any chroma.
#[test]
fn chroma_factor_is_never_negative() {
    let extremes = [-100.0, -60.0, 0.0, 40.0, 100.0];
    for chroma in [0.0, 0.005, 0.02, 0.1, 0.35] {
        for hue_step in 0..360 {
            let hue = f64::from(hue_step);
            let lab = Oklab {
                l: 0.6,
                a: chroma * hue.to_radians().cos(),
                b: chroma * hue.to_radians().sin(),
            };
            for (index, value) in extremes.iter().enumerate() {
                // A uniform setting, and one that alternates around the wheel.
                let uniform = MixerParams {
                    saturation: [*value; RANGE_COUNT],
                    ..MixerParams::neutral()
                };
                let mut alternating = MixerParams::neutral();
                for range in 0..RANGE_COUNT {
                    alternating.saturation[range] = extremes[(range + index) % extremes.len()];
                }
                for params in [uniform, alternating] {
                    let factor = mixer::amounts(lab, &params).chroma_factor;
                    assert!(
                        (0.0..=2.0).contains(&factor),
                        "chroma factor {factor} outside [0, 2] at hue {hue}, chroma {chroma}"
                    );
                }
            }
        }
    }
}

/// With every saturation slider at `-100` the chroma factor is exactly zero
/// for every colour, near-greys and the darkest included — a decrease is not
/// ramped — and the result reconstructs to three bit-identical channels.
#[test]
fn every_saturation_slider_at_minus_100_greys_every_colour_exactly() {
    let params = MixerParams {
        saturation: [-100.0; RANGE_COUNT],
        ..MixerParams::neutral()
    };
    for l in [0.0, 0.001, 0.05, 0.4, 0.8, 1.0, 1.2] {
        for chroma in [1e-9, 1e-4, 0.005, 0.0199, 0.1, 0.3] {
            for hue_step in 0..720 {
                let hue = f64::from(hue_step) / 2.0;
                let lab = Oklab {
                    l,
                    a: chroma * hue.to_radians().cos(),
                    b: chroma * hue.to_radians().sin(),
                };
                assert_eq!(
                    mixer::amounts(lab, &params).chroma_factor,
                    0.0,
                    "L {l}, C {chroma}, h {hue}"
                );
                let [r, g, b] = mixer::mix(colour::from_oklab(lab), &params);
                assert!(
                    r == g && g == b,
                    "L {l}, C {chroma}, h {hue}: {:?}",
                    [r, g, b]
                );
            }
        }
    }
}

/// A decrease is applied in full at any chroma, and only an increase is scaled
/// by the ramp: at a range's own hue, `-50` gives `0.5` below the ramp edge as
/// above it, while `+50` gives `1 + 0.5 * w_c`.
#[test]
fn a_saturation_decrease_is_not_ramped_and_an_increase_is() {
    for range in 0..RANGE_COUNT {
        let hue = CENTRE_HUES_DEG[range].to_radians();
        for chroma in [0.001, 0.005, 0.01, 0.019, 0.02, 0.2] {
            let lab = Oklab {
                l: 0.5,
                a: chroma * hue.cos(),
                b: chroma * hue.sin(),
            };
            let ramp = mixer::chroma_ramp(chroma);
            let down = mixer::amounts(lab, &MixerParams::with_saturation(range, -50.0));
            let up = mixer::amounts(lab, &MixerParams::with_saturation(range, 50.0));
            assert!(
                (down.chroma_factor - 0.5).abs() < 1e-12,
                "{} at C {chroma}: -50 gave {}",
                RANGE_NAMES[range],
                down.chroma_factor
            );
            assert!(
                (up.chroma_factor - (1.0 + 0.5 * ramp)).abs() < 1e-12,
                "{} at C {chroma}: +50 gave {}",
                RANGE_NAMES[range],
                up.chroma_factor
            );
        }
    }
    // Continuous at zero: the two branches meet at a factor of exactly one.
    assert_eq!(mixer::chroma_factor(0.0, 0.3), 1.0);
    assert_eq!(mixer::chroma_factor(-0.0, 0.3), 1.0);
}

// ---------------------------------------------------------------------------
// 6. Luminance
// ---------------------------------------------------------------------------

/// For each range's own centre colour, Oklab `L` is strictly increasing in
/// that range's luminance slider across the whole range, stays inside `(0, 1)`
/// and never reaches either endpoint.
#[test]
fn luminance_is_strictly_monotone_in_the_slider_for_a_centre_colour() {
    for range in 0..RANGE_COUNT {
        let rgb = srgb8_linear(RANGE_REFERENCE_CODES[range]);
        let mut previous = f64::NEG_INFINITY;
        for value in -100..=100 {
            let params = MixerParams::with_luminance(range, f64::from(value));
            let l = colour::to_oklab(mixer::mix(rgb, &params)).l;
            assert!(
                l > previous,
                "{} at luminance {value}: L = {l} did not exceed {previous}",
                RANGE_NAMES[range]
            );
            assert!(
                l > 0.0 && l < 1.000_001,
                "{} at luminance {value}: L = {l} left (0, 1)",
                RANGE_NAMES[range]
            );
            previous = l;
        }
    }
}

/// The frozen response's fixed points and bounds, independent of any colour:
/// `L = 0` and `L = 1` are exact fixed points, `(0, 1)` maps into `(0, 1)`,
/// the response is strictly increasing in `L`, and an out-of-range `L` passes
/// through unchanged.
#[test]
fn luminance_response_is_bounded_and_monotone() {
    for amount in [-1.0, -0.5, -0.1, 0.1, 0.5, 1.0] {
        assert_eq!(mixer::luminance_response(0.0, amount), 0.0);
        assert_eq!(mixer::luminance_response(1.0, amount), 1.0);
        assert_eq!(mixer::luminance_response(1.5, amount), 1.5);
        assert_eq!(mixer::luminance_response(-0.25, amount), -0.25);

        let mut previous = f64::NEG_INFINITY;
        for step in 1..1000 {
            let l = f64::from(step) / 1000.0;
            let out = mixer::luminance_response(l, amount);
            assert!(out > 0.0 && out < 1.0, "L = {l} at m = {amount} gave {out}");
            assert!(out > previous, "not increasing in L at {l}, m = {amount}");
            previous = out;
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Gamut and finiteness
// ---------------------------------------------------------------------------

/// Out-of-gamut linear inputs and extreme parameter corners stay finite; the
/// unit never clamps, per the colour study's gamut policy.
#[test]
fn out_of_gamut_inputs_and_extreme_parameters_stay_finite() {
    let inputs: [[f64; 3]; 6] = [
        [1.5, 0.5, 0.2],
        [-0.1, 0.3, 0.8],
        [1.5, -0.1, 0.7],
        [-0.1, -0.1, -0.1],
        [1.5, 1.5, 1.5],
        [0.0, 0.0, 0.0],
    ];
    let corners = [-100.0, 100.0];
    for rgb in inputs {
        for hue in corners {
            for saturation in corners {
                for luminance in corners {
                    let params = MixerParams {
                        hue: [hue; RANGE_COUNT],
                        saturation: [saturation; RANGE_COUNT],
                        luminance: [luminance; RANGE_COUNT],
                    };
                    let out = mixer::mix(rgb, &params);
                    assert!(
                        out.iter().all(|c| c.is_finite()),
                        "non-finite output for {rgb:?} at ({hue}, {saturation}, {luminance}): {out:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 8. The study note's figures
// ---------------------------------------------------------------------------

/// Prints every measured figure quoted in `docs/design/mixer-study.md`.
#[test]
#[ignore = "run explicitly to reproduce the figures quoted in docs/design/mixer-study.md"]
fn mixer_study_figures() {
    println!("centre hues, gaps and full-slider travel");
    for range in 0..RANGE_COUNT {
        let lab = colour::to_oklab(srgb8_linear(RANGE_REFERENCE_CODES[range]));
        println!(
            "  {:<8} h = {:>12.6}  C = {:.6}  L = {:.6}  gap to next = {:>10.6}  +100 = {:>9.6}  -100 = {:>9.6}",
            RANGE_NAMES[range],
            mixer::normalize_hue_deg(colour::hue_degrees(lab)),
            colour::chroma(lab),
            lab.l,
            mixer::hue_gap_deg(range),
            mixer::full_travel_deg(range, true),
            -mixer::full_travel_deg(range, false),
        );
    }

    println!("measured travel of each reference colour through mix at +/-100");
    for range in 0..RANGE_COUNT {
        let rgb = srgb8_linear(RANGE_REFERENCE_CODES[range]);
        let start = mixer::normalize_hue_deg(colour::hue_degrees(colour::to_oklab(rgb)));
        let travel = |value: f64| {
            let moved = mixer::mix(rgb, &MixerParams::with_hue(range, value));
            let end = mixer::normalize_hue_deg(colour::hue_degrees(colour::to_oklab(moved)));
            mixer::normalize_hue_deg(end - start + 180.0) - 180.0
        };
        println!(
            "  {:<8} +100: {:>+10.6}  -100: {:>+10.6}",
            RANGE_NAMES[range],
            travel(100.0),
            travel(-100.0)
        );
    }

    let samples = 200_000;
    let step_deg = 360.0 / f64::from(samples);
    let mut worst_weight_jump = 0.0_f64;
    let mut previous_weights = mixer::range_weights(0.0);
    for step in 1..=samples {
        let weights = mixer::range_weights(360.0 * f64::from(step) / f64::from(samples));
        for range in 0..RANGE_COUNT {
            worst_weight_jump =
                worst_weight_jump.max((weights[range] - previous_weights[range]).abs());
        }
        previous_weights = weights;
    }
    let narrowest = (0..RANGE_COUNT)
        .map(mixer::hue_gap_deg)
        .fold(f64::INFINITY, f64::min);
    println!(
        "largest weight change over a {step_deg:.5} deg step: {worst_weight_jump:e} \
         (analytic bound pi / (2 * {narrowest:.6}) per degree = {:.6}/deg, measured {:.6}/deg)",
        PI / (2.0 * narrowest),
        worst_weight_jump / step_deg
    );

    println!("hue-map slope floors (exact per-segment minimum of dH/dh)");
    let mut single = (f64::INFINITY, String::new());
    for (range, name) in RANGE_NAMES.iter().enumerate() {
        let plus = HueWarp::new(&MixerParams::with_hue(range, 100.0)).minimum_slope();
        let minus = HueWarp::new(&MixerParams::with_hue(range, -100.0)).minimum_slope();
        println!("  {name:<8} alone at +100: {plus:.6}   at -100: {minus:.6}");
        for (value, floor) in [(100.0, plus), (-100.0, minus)] {
            if floor < single.0 {
                single = (floor, format!("{name} at {value:+}"));
            }
        }
    }
    println!("  single slider floor: {:.6} ({})", single.0, single.1);
    println!(
        "  every hue slider +100: {:.6}   every hue slider -100: {:.6}",
        HueWarp::new(&all_hue(100.0)).minimum_slope(),
        HueWarp::new(&all_hue(-100.0)).minimum_slope()
    );
    let mut opposing = (f64::INFINITY, String::new());
    for range in 0..RANGE_COUNT {
        let floor = HueWarp::new(&opposing_pair(range)).minimum_slope();
        println!(
            "  {} +100 against {} -100: {floor:.6}",
            RANGE_NAMES[range],
            RANGE_NAMES[(range + 1) % RANGE_COUNT]
        );
        if floor < opposing.0 {
            opposing = (floor, RANGE_NAMES[range].to_string());
        }
    }
    println!(
        "  opposing neighbours floor: {:.6} ({} pair)",
        opposing.0, opposing.1
    );
    let mut global = (f64::INFINITY, MixerParams::neutral());
    for params in five_level_combinations() {
        let floor = HueWarp::new(&params).minimum_slope();
        if floor < global.0 {
            global = (floor, params);
        }
    }
    println!(
        "  every 5-level combination (5^8): floor {:.10} at hue {:?}",
        global.0, global.1.hue
    );
    let mut random = (f64::INFINITY, MixerParams::neutral());
    for params in random_hue_combinations(1_000_000) {
        let floor = HueWarp::new(&params).minimum_slope();
        if floor < random.0 {
            random = (floor, params);
        }
    }
    println!(
        "  1 000 000 random whole-number combinations: floor {:.10} at hue {:?}",
        random.0, random.1.hue
    );

    println!("single-slider spill beyond the neighbouring centres (largest |D|, degrees)");
    for range in 0..RANGE_COUNT {
        let mut line = format!("  {:<8}", RANGE_NAMES[range]);
        for value in [100.0, -100.0] {
            let warp = HueWarp::new(&MixerParams::with_hue(range, value));
            let before = (range + RANGE_COUNT - 2) % RANGE_COUNT;
            let after = (range + 1) % RANGE_COUNT;
            let spill = |segment: usize| {
                (1..1000)
                    .map(|step| {
                        warp.displacement_deg(
                            CENTRE_HUES_DEG[segment]
                                + mixer::hue_gap_deg(segment) * f64::from(step) / 1000.0,
                        )
                    })
                    .fold(
                        0.0_f64,
                        |worst, d| if d.abs() > worst.abs() { d } else { worst },
                    )
            };
            line += &format!(
                "  {value:+}: {:<8} side {:>+8.3}, {:<8} side {:>+8.3}",
                RANGE_NAMES[before],
                spill(before),
                RANGE_NAMES[(range + 2) % RANGE_COUNT],
                spill(after)
            );
        }
        println!("{line}");
    }

    let mut largest_rotation = 0.0_f64;
    for combination in 0..3usize.pow(RANGE_COUNT as u32) {
        let mut code = combination;
        let mut params = MixerParams::neutral();
        for range in 0..RANGE_COUNT {
            params.hue[range] = [-100.0, 0.0, 100.0][code % 3];
            code /= 3;
        }
        let warp = HueWarp::new(&params);
        for step in 0..3600 {
            largest_rotation =
                largest_rotation.max(warp.displacement_deg(f64::from(step) / 10.0).abs());
        }
    }
    println!("largest |rotation| over every 3-level combination: {largest_rotation:.4} deg");

    let mut grey_noise = 0.0_f64;
    for code in 0u8..=255 {
        grey_noise = grey_noise.max(colour::chroma(colour::to_oklab(grey_linear(code))));
    }
    println!(
        "grey chroma noise floor {grey_noise:e} -> w_c = {:e}",
        mixer::chroma_ramp(grey_noise)
    );

    let neutral = MixerParams::neutral();
    let steps = 21;
    let sample = |i: i32| -0.2 + 1.7 * f64::from(i) / f64::from(steps - 1);
    let mut identity = 0.0_f64;
    for ri in 0..steps {
        for gi in 0..steps {
            for bi in 0..steps {
                let rgb = [sample(ri), sample(gi), sample(bi)];
                identity = identity.max(max_abs_difference(mixer::mix(rgb, &neutral), rgb));
            }
        }
    }
    println!("identity residual over a 21^3 sweep of [-0.2, 1.5]^3: {identity:e}");

    let mut grey = 0.0_f64;
    for code in 0u8..=255 {
        let rgb = grey_linear(code);
        for range in 0..RANGE_COUNT {
            for value in [-100.0, 100.0] {
                for params in [
                    MixerParams::with_hue(range, value),
                    MixerParams::with_saturation(range, value),
                    MixerParams::with_luminance(range, value),
                ] {
                    grey = grey.max(max_abs_difference(mixer::mix(rgb, &params), rgb));
                }
            }
        }
    }
    println!("worst achromatic deviation over all 48 single-slider extremes: {grey:e}");

    let hostile = MixerParams {
        hue: [100.0, -100.0, 100.0, -100.0, 100.0, -100.0, 100.0, -100.0],
        saturation: [50.0; RANGE_COUNT],
        luminance: [30.0; RANGE_COUNT],
    };
    let samples = 7_200;
    for chroma in [0.05, 0.10, 0.20, 0.30] {
        let mut worst = 0.0_f64;
        let mut previous = mixer::mix(wheel_colour(0.6, chroma, 0.0), &hostile);
        for step in 1..=samples {
            let hue = 360.0 * f64::from(step) / f64::from(samples);
            let current = mixer::mix(wheel_colour(0.6, chroma, hue), &hostile);
            worst = worst.max(max_abs_difference(current, previous));
            previous = current;
        }
        println!(
            "wheel continuity at chroma {chroma:.2}: max jump {worst:e} per {:.3} deg step",
            360.0 / f64::from(samples)
        );
    }

    println!("luminance response at a range's own centre colour");
    for range in 0..RANGE_COUNT {
        let rgb = srgb8_linear(RANGE_REFERENCE_CODES[range]);
        let start = colour::to_oklab(rgb).l;
        let down = colour::to_oklab(mixer::mix(rgb, &MixerParams::with_luminance(range, -100.0))).l;
        let up = colour::to_oklab(mixer::mix(rgb, &MixerParams::with_luminance(range, 100.0))).l;
        println!(
            "  {:<8} L {start:.4} -> -100: {down:.4}   +100: {up:.4}",
            RANGE_NAMES[range]
        );
    }

    println!(
        "near-black and low-chroma samples (residual chroma under every saturation slider at \
         -100: now, and had the ramp also scaled the decrease)"
    );
    let desaturated = MixerParams {
        saturation: [-100.0; RANGE_COUNT],
        ..MixerParams::neutral()
    };
    for (name, codes) in [
        ("near_black_blue", [2u8, 2, 6]),
        ("near_black_warm", [6, 4, 2]),
        ("near_black_green", [3, 5, 3]),
        ("near_grey_warm", [130, 128, 126]),
        ("near_grey_cool", [100, 101, 104]),
        ("skin_light", [255, 219, 172]),
        ("skin_mid", [224, 172, 140]),
        ("skin_dark", [141, 85, 36]),
    ] {
        let lab = colour::to_oklab(srgb8_linear(codes));
        let chroma = colour::chroma(lab);
        let ramp = mixer::chroma_ramp(chroma);
        let now = colour::chroma(colour::to_oklab(mixer::mix(
            srgb8_linear(codes),
            &desaturated,
        )));
        println!(
            "  {name:<16} C = {chroma:.6}  w_c = {ramp:.6}  h = {:.3}  residual C now {now:.2e}, \
             ramped {:.6}",
            mixer::normalize_hue_deg(colour::hue_degrees(lab)),
            chroma * (1.0 - ramp)
        );
    }

    println!("frozen examples (unclamped linear, then output codes)");
    for (name, codes, params) in [
        (
            "red at red_hue_plus_100",
            [255u8, 0, 0],
            MixerParams::with_hue(0, 100.0),
        ),
        (
            "aqua at aqua_hue_minus_100",
            [0, 255, 255],
            MixerParams::with_hue(4, -100.0),
        ),
        (
            "red at red_plus_orange_minus_100",
            [255, 0, 0],
            opposing_pair(0),
        ),
        (
            "orange at red_plus_orange_minus_100",
            [255, 128, 0],
            opposing_pair(0),
        ),
        ("yellow at all_hue_plus_100", [255, 255, 0], all_hue(100.0)),
        ("blue at all_hue_plus_100", [0, 0, 255], all_hue(100.0)),
        (
            "(2, 2, 6) at all_saturation_minus_100",
            [2, 2, 6],
            desaturated,
        ),
        (
            "(128, 128, 128) at all_ranges",
            [128, 128, 128],
            MixerParams {
                hue: [25.0; RANGE_COUNT],
                saturation: [-40.0; RANGE_COUNT],
                luminance: [30.0; RANGE_COUNT],
            },
        ),
        (
            "(2, 2, 6) at all_ranges",
            [2, 2, 6],
            MixerParams {
                hue: [25.0; RANGE_COUNT],
                saturation: [-40.0; RANGE_COUNT],
                luminance: [30.0; RANGE_COUNT],
            },
        ),
    ] {
        let out = mixer::mix(srgb8_linear(codes), &params);
        let hue = mixer::normalize_hue_deg(colour::hue_degrees(colour::to_oklab(out)));
        println!(
            "  {name}: [{:.6}, {:.6}, {:.6}] -> codes {:?}, Oklab hue {hue:.3}",
            out[0],
            out[1],
            out[2],
            out.map(luxforge_reference::linear_to_srgb_code)
        );
    }
}

// ---------------------------------------------------------------------------
// 9. Fixtures
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Input {
    /// An 8-bit sRGB-encoded input, decoded through the reference sRGB
    /// transfer function before the mixer runs.
    Srgb8 { rgb: [u8; 3] },
    /// A linear sRGB input given directly: the hue wheel and the out-of-gamut
    /// cases, which no 8-bit code represents.
    Linear { rgb: [f64; 3] },
}

impl Input {
    fn to_linear(&self) -> [f64; 3] {
        match self {
            Input::Srgb8 { rgb } => srgb8_linear(*rgb),
            Input::Linear { rgb } => *rgb,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ParameterSet {
    name: String,
    hue: [f64; RANGE_COUNT],
    saturation: [f64; RANGE_COUNT],
    luminance: [f64; RANGE_COUNT],
}

impl ParameterSet {
    fn params(&self) -> MixerParams {
        MixerParams {
            hue: self.hue,
            saturation: self.saturation,
            luminance: self.luminance,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Case {
    name: String,
    input: Input,
    parameters: String,
    /// Linear sRGB output of `mixer::mix`, at full f64 precision
    /// (`serde_json`'s shortest round-trippable representation), unclamped.
    expected_linear: [f64; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FixtureFile {
    generated_by: String,
    note: String,
    range_order: Vec<String>,
    parameter_sets: Vec<ParameterSet>,
    cases: Vec<Case>,
}

fn fixture_inputs() -> Vec<(String, Input)> {
    let mut inputs: Vec<(String, Input)> = Vec::new();

    for (range, codes) in RANGE_REFERENCE_CODES.iter().enumerate() {
        inputs.push((
            format!("centre_{}", RANGE_NAMES[range]),
            Input::Srgb8 { rgb: *codes },
        ));
    }

    for code in [0u8, 32, 64, 128, 192, 255] {
        inputs.push((
            format!("grey_{code}"),
            Input::Srgb8 {
                rgb: [code, code, code],
            },
        ));
    }

    for (name, codes) in [
        ("near_black_blue", [2u8, 2, 6]),
        ("near_black_warm", [6, 4, 2]),
        ("near_black_green", [3, 5, 3]),
        ("near_grey_warm", [130, 128, 126]),
        ("near_grey_cool", [100, 101, 104]),
    ] {
        inputs.push((name.to_string(), Input::Srgb8 { rgb: codes }));
    }

    for (name, codes) in [
        ("skin_light", [255u8, 219, 172]),
        ("skin_mid", [224, 172, 140]),
        ("skin_dark", [141, 85, 36]),
    ] {
        inputs.push((name.to_string(), Input::Srgb8 { rgb: codes }));
    }

    for chroma in [0.05, 0.12, 0.20] {
        for step in 0..8 {
            let hue = 45.0 * f64::from(step);
            inputs.push((
                format!("wheel_c{:03}_h{:03}", (chroma * 1000.0) as i32, hue as i32),
                Input::Linear {
                    rgb: wheel_colour(0.6, chroma, hue),
                },
            ));
        }
    }

    for (name, rgb) in [
        ("out_of_gamut_bright", [1.5, 0.5, 0.2]),
        ("out_of_gamut_mixed", [1.5, -0.1, 0.7]),
    ] {
        inputs.push((name.to_string(), Input::Linear { rgb }));
    }

    inputs
}

fn fixture_parameter_sets() -> Vec<ParameterSet> {
    let mut sets = Vec::new();
    let mut push = |name: &str, params: MixerParams| {
        sets.push(ParameterSet {
            name: name.to_string(),
            hue: params.hue,
            saturation: params.saturation,
            luminance: params.luminance,
        });
    };

    push("neutral", MixerParams::neutral());
    push("red_hue_plus_100", MixerParams::with_hue(0, 100.0));
    push("aqua_hue_minus_100", MixerParams::with_hue(4, -100.0));
    push(
        "blue_saturation_minus_100",
        MixerParams::with_saturation(5, -100.0),
    );
    push(
        "green_saturation_plus_100",
        MixerParams::with_saturation(3, 100.0),
    );
    push(
        "orange_luminance_plus_100",
        MixerParams::with_luminance(1, 100.0),
    );
    push(
        "purple_luminance_minus_100",
        MixerParams::with_luminance(6, -100.0),
    );
    push(
        "orange_combined",
        MixerParams {
            hue: [0.0, -50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            saturation: [0.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            luminance: [0.0, -50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
    );
    push(
        "all_ranges",
        MixerParams {
            hue: [25.0; RANGE_COUNT],
            saturation: [-40.0; RANGE_COUNT],
            luminance: [30.0; RANGE_COUNT],
        },
    );
    push("all_hue_plus_100", all_hue(100.0));
    push("red_plus_orange_minus_100", opposing_pair(0));
    push(
        "all_saturation_minus_100",
        MixerParams {
            saturation: [-100.0; RANGE_COUNT],
            ..MixerParams::neutral()
        },
    );

    sets
}

fn fixture_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/luxforge-reference; fixtures/ is repo-root-level.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mixer")
        .join("mixer-cases.json")
}

#[test]
#[ignore = "run explicitly to (re)generate fixtures/mixer/mixer-cases.json from the frozen reference"]
fn generate_mixer_fixtures() {
    let parameter_sets = fixture_parameter_sets();
    let mut cases = Vec::new();
    for (input_name, input) in fixture_inputs() {
        let linear = input.to_linear();
        for set in &parameter_sets {
            cases.push(Case {
                name: format!("{input_name}__{}", set.name),
                input: input.clone(),
                parameters: set.name.clone(),
                expected_linear: mixer::mix(linear, &set.params()),
            });
        }
    }

    let file = FixtureFile {
        generated_by: "crates/luxforge-reference/tests/studies/mixer.rs generate_mixer_fixtures"
            .to_string(),
        note: "Independent f64 reference (crates/luxforge-reference/src/mixer.rs). expected_linear is linear \
               sRGB after the frozen mixer unit (hue warp, then chroma, then luminance), full f64 \
               precision, unclamped. Production is compared against this file within the \
               tolerance frozen in docs/design/mixer-study.md."
            .to_string(),
        range_order: RANGE_NAMES.iter().map(|name| name.to_string()).collect(),
        parameter_sets,
        cases,
    };

    let path = fixture_path();
    fs::create_dir_all(path.parent().unwrap()).expect("create fixtures/mixer");
    let json = serde_json::to_string_pretty(&file).expect("serialize fixtures");
    fs::write(&path, json).expect("write fixtures/mixer/mixer-cases.json");
    println!("wrote {} cases to {}", file.cases.len(), path.display());
}

#[test]
fn mixer_fixtures_match_reference() {
    let path = fixture_path();
    let json = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is missing ({err}); run `cargo test --package luxforge-core \
             -p luxforge-reference --test studies -- --ignored generate_mixer_fixtures` to (re)create it",
            path.display()
        )
    });
    let file: FixtureFile = serde_json::from_str(&json).expect("parse mixer-cases.json");
    assert!(!file.cases.is_empty(), "mixer-cases.json has no cases");
    assert_eq!(
        file.range_order,
        RANGE_NAMES
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>(),
        "the fixture's range order no longer matches the reference"
    );

    for case in &file.cases {
        let set = file
            .parameter_sets
            .iter()
            .find(|set| set.name == case.parameters)
            .unwrap_or_else(|| panic!("case {:?} names an unknown parameter set", case.name));
        let recomputed = mixer::mix(case.input.to_linear(), &set.params());
        for (channel, (expected, actual)) in case
            .expected_linear
            .iter()
            .zip(recomputed.iter())
            .enumerate()
        {
            assert!(
                (expected - actual).abs() < 1e-12,
                "case {:?} channel {channel}: fixture {expected} but the reference now computes \
                 {actual} (regenerate the fixture if this is an intentional formula change)",
                case.name
            );
        }
    }
}
