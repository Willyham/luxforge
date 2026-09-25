//! TASK-004: independent proofs for the frozen post-crop vignette mask and
//! amount equations, and the oracle fixtures a later production implementation
//! is checked against.
//!
//! This study shares no code with `lightwell-core`'s production sources. The
//! frozen equations live in `crates/lightwell-reference/src/vignette.rs`; the maths and every
//! constant used below are written out in full in
//! `docs/design/vignette-study.md`, which this file's test names and comments
//! track.

use super::approximately_equal;
use lightwell_reference::SplitMix64;
use lightwell_reference::vignette::{VignetteParams, apply, corner_radius, mask, vignette_pixel};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// The study's own inputs.
// ---------------------------------------------------------------------------

/// The three aspect ratios the study fixes: 3:2, 2:3 and 1:1, at the small
/// sizes the task brief names.
const ASPECTS: [(u32, u32); 3] = [(24, 16), (16, 24), (20, 20)];

fn defaults() -> VignetteParams {
    VignetteParams::NEUTRAL
}

/// The four corner pixel coordinates of a `width x height` frame.
fn corners(width: u32, height: u32) -> [(u32, u32); 4] {
    [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
    ]
}

/// One pixel at or adjacent to the frame's centre: for an even dimension
/// there is no pixel centred exactly on the frame, so this returns the pixel
/// just past the midpoint on each axis (matching the "centre invariance"
/// precise definition in the study note: `mask = 0` wherever `r <= r0`, not
/// only at an exact geometric centre).
fn near_centre(width: u32, height: u32) -> (u32, u32) {
    (width / 2, height / 2)
}

// ---------------------------------------------------------------------------
// Property: amount = 0 is the exact identity, for every mask.
// ---------------------------------------------------------------------------

#[test]
fn amount_zero_is_identity_for_every_mask_and_position() {
    let rgb_samples = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.5, 0.3, 0.1],
        [-0.2, 1.4, 0.02],
    ];
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            for midpoint in [0.0, 25.0, 50.0, 75.0, 100.0] {
                for feather in [0.0, 25.0, 50.0, 75.0, 100.0] {
                    let params = VignetteParams {
                        amount: 0.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    for (x, y) in corners(width, height)
                        .into_iter()
                        .chain([near_centre(width, height)])
                    {
                        let m = mask(x, y, width, height, &params);
                        for rgb in rgb_samples {
                            assert_eq!(
                                vignette_pixel(x, y, width, height, rgb, &params),
                                rgb,
                                "{width}x{height} ({x},{y}) {params:?}: amount=0 must be identity"
                            );
                            assert_eq!(apply(rgb, m, 0.0), rgb);
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: mask is symmetric under horizontal mirror and vertical flip, for
// both even and odd frame sizes.
// ---------------------------------------------------------------------------

#[test]
fn mask_is_symmetric_under_horizontal_mirror_and_vertical_flip() {
    // Even sizes (the three frozen aspects) and odd sizes on both axes, so the
    // "no pixel sits exactly on the centre line" and "one pixel sits exactly
    // on the centre line" cases are both exercised.
    let sizes: [(u32, u32); 5] = [(24, 16), (16, 24), (20, 20), (21, 15), (15, 21)];
    for (width, height) in sizes {
        for roundness in [-100.0, -33.0, 0.0, 33.0, 100.0] {
            for midpoint in [0.0, 40.0, 100.0] {
                for feather in [0.0, 60.0, 100.0] {
                    let params = VignetteParams {
                        amount: 0.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    for x in 0..width {
                        for y in 0..height {
                            let m = mask(x, y, width, height, &params);
                            let mirrored = mask(width - 1 - x, y, width, height, &params);
                            let flipped = mask(x, height - 1 - y, width, height, &params);
                            assert!(
                                (m - mirrored).abs() < 1e-12,
                                "{width}x{height} {params:?}: mask({x},{y})={m} != \
                                 mirror mask({},{})={mirrored}",
                                width - 1 - x,
                                y
                            );
                            assert!(
                                (m - flipped).abs() < 1e-12,
                                "{width}x{height} {params:?}: mask({x},{y})={m} != \
                                 flip mask({x},{})={flipped}",
                                height - 1 - y
                            );
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: mask is nondecreasing in the shape radius r, so it is
// nondecreasing along every ray outward from the centre (shape_radius is
// homogeneous of degree 1 along such a ray in both the s>=0 and s<0 branches,
// so "nondecreasing in r" implies "nondecreasing along a ray" directly).
// ---------------------------------------------------------------------------

#[test]
fn mask_is_nondecreasing_as_a_function_of_shape_radius() {
    // Sample many (u, v) directions and march outward in equal steps of t,
    // computing r and mask via the actual pixel grid at a large size (so the
    // pixel quantization is fine enough to approximate the continuous ray).
    let (width, height) = (2000u32, 1500u32);
    let mut rng = SplitMix64(2024);
    for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
        for midpoint in [0.0, 30.0, 70.0, 100.0] {
            for feather in [0.0, 40.0, 100.0] {
                let params = VignetteParams {
                    amount: 0.0,
                    midpoint,
                    roundness,
                    feather,
                };
                for _ in 0..20 {
                    // A random direction from the centre pixel, marched outward.
                    let angle = rng.next_range(0.0, std::f64::consts::TAU);
                    let (dx, dy) = (angle.cos(), angle.sin());
                    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
                    let mut previous_r: Option<f64> = None;
                    let mut previous_mask: Option<f64> = None;
                    for step in 0..40 {
                        let t = f64::from(step) / 39.0;
                        let radius_px = t * (width.min(height) as f64 / 2.0 - 1.0);
                        let x = (cx + dx * radius_px)
                            .round()
                            .clamp(0.0, f64::from(width - 1));
                        let y = (cy + dy * radius_px)
                            .round()
                            .clamp(0.0, f64::from(height - 1));
                        let (x, y) = (x as u32, y as u32);
                        let m = mask(x, y, width, height, &params);
                        // Recover r directly to order the samples by radius,
                        // independent of the (possibly non-monotonic in pixel
                        // index, due to rounding) marching path.
                        let u = (f64::from(x) + 0.5 - cx) / cx;
                        let v = (f64::from(y) + 0.5 - cy) / cy;
                        let r = if roundness >= 0.0 {
                            let hd2 = cx * cx + cy * cy;
                            let s = roundness / 100.0;
                            let a = (1.0 - s) / 2.0 + s * cx * cx / hd2;
                            let b = (1.0 - s) / 2.0 + s * cy * cy / hd2;
                            (a * u * u + b * v * v).sqrt()
                        } else {
                            let p = 2.0 + 6.0 * (-roundness / 100.0);
                            ((u.abs().powf(p) + v.abs().powf(p)) / 2.0).powf(1.0 / p)
                        };
                        if let (Some(pr), Some(pm)) = (previous_r, previous_mask)
                            && r >= pr
                        {
                            assert!(
                                m >= pm - 1e-9,
                                "{params:?}: mask decreased from r={pr} (mask={pm}) to \
                                 r={r} (mask={m})"
                            );
                        }
                        previous_r = Some(r);
                        previous_mask = Some(m);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: the mask depends only on (x, y, width, height, params) -- so
// evaluating it at a smaller stage (as a crop produces) recentres
// independently of the original frame. The frame's own centre-ish pixel
// always has mask 0 under any midpoint > 0 (or, at midpoint = 0, satisfies
// the precise "r <= r0" centre-invariance definition below).
// ---------------------------------------------------------------------------

#[test]
fn mask_recentres_on_a_smaller_stage_after_a_crop() {
    // A "photograph" cropped from 240x160 down to 24x16 (the same 3:2
    // aspect): the new stage's own centre-ish pixel must read mask 0 under
    // the default midpoint/feather, exactly as the pre-crop stage's did,
    // because the mask is recomputed from the new stage's own width/height,
    // not inherited from the original.
    let params = defaults();
    for &(large_w, large_h) in &[(240u32, 160u32), (160, 240), (200, 200)] {
        let (x, y) = near_centre(large_w, large_h);
        let large_mask = mask(x, y, large_w, large_h, &params);
        assert_eq!(
            large_mask, 0.0,
            "{large_w}x{large_h}: default centre mask must be 0"
        );
    }
    for &(small_w, small_h) in &ASPECTS {
        let (x, y) = near_centre(small_w, small_h);
        let small_mask = mask(x, y, small_w, small_h, &params);
        assert_eq!(
            small_mask, 0.0,
            "{small_w}x{small_h}: cropped stage's own centre mask must independently be 0"
        );
    }
}

// ---------------------------------------------------------------------------
// Property: the centre-invariance definition, stated precisely (see the study
// note): mask(u,v) = 0 for r <= r0. At midpoint = 0, feather = 0 (r0 = 0),
// only the exact geometric centre (r = 0) reads 0; every other pixel reads 1
// (a hard step at r0 = 0). No pixel centre sits exactly at (0, 0) for an even
// width/height, so the frame's own "centre" pixels do NOT read 0 there --
// this is recorded as the honest edge case, not treated as a bug.
// ---------------------------------------------------------------------------

#[test]
fn centre_invariance_is_r_less_or_equal_r0_not_an_exact_centre_pixel_guarantee() {
    let degenerate = VignetteParams {
        amount: 0.0,
        midpoint: 0.0,
        roundness: 0.0,
        feather: 0.0,
    };
    // Odd x odd: one pixel sits exactly at r = 0 and reads mask 0.
    let (w, h) = (21u32, 15u32);
    let (cx, cy) = (w / 2, h / 2);
    assert_eq!(mask(cx, cy, w, h, &degenerate), 0.0);
    // Even x even (e.g. every ASPECTS fixture): no pixel sits at r = 0, so
    // every pixel, including the near-centre pixel, reads mask 1 under this
    // degenerate hard-step-at-zero parameter combination.
    for &(width, height) in &ASPECTS {
        let (x, y) = near_centre(width, height);
        let m = mask(x, y, width, height, &degenerate);
        assert_eq!(
            m, 1.0,
            "{width}x{height}: even dimensions have no r=0 pixel, so midpoint=feather=0 \
             leaves even the near-centre pixel at mask 1, not 0"
        );
    }
    // The general property that always holds, independent of parity: mask is
    // 0 wherever r <= r0, and nowhere else when r0 == r1 (the hard-step case).
    for &(width, height) in &ASPECTS {
        for &(x, y) in &[
            (0u32, 0u32),
            (width / 2, height / 2),
            (width - 1, height - 1),
        ] {
            let half_w = f64::from(width) / 2.0;
            let half_h = f64::from(height) / 2.0;
            let u = (f64::from(x) + 0.5 - half_w) / half_w;
            let v = (f64::from(y) + 0.5 - half_h) / half_h;
            let r = (0.5 * u * u + 0.5 * v * v).sqrt();
            let m = mask(x, y, width, height, &degenerate);
            let expected = if r <= 0.0 { 0.0 } else { 1.0 };
            assert_eq!(m, expected, "{width}x{height} ({x},{y}): r={r}");
        }
    }
}

// ---------------------------------------------------------------------------
// Property: the corner pixel always carries the frame's maximum mask value
// (shape_radius is coordinatewise nondecreasing in |u| and |v| separately,
// and the corner pixel maximises both simultaneously), independent of
// whether that maximum reaches exactly 1.
// ---------------------------------------------------------------------------

#[test]
fn corner_mask_is_always_the_frame_maximum() {
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            for midpoint in [0.0, 50.0, 90.0, 99.0] {
                for feather in [0.0, 50.0, 90.0, 99.0] {
                    let params = VignetteParams {
                        amount: 0.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    let corner_mask_value = mask(0, 0, width, height, &params);
                    // Spot-check against a coarse grid rather than every
                    // pixel, to keep this test fast; the monotone-in-r proof
                    // above is the exhaustive argument this spot check backs.
                    for x in (0..width).step_by(3) {
                        for y in (0..height).step_by(3) {
                            let m = mask(x, y, width, height, &params);
                            assert!(
                                m <= corner_mask_value + 1e-12,
                                "{width}x{height} {params:?}: mask({x},{y})={m} exceeds \
                                 corner mask {corner_mask_value}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: corner mask == 1 (and, by extension, amount=-100 blackens the
// corner exactly) exactly when r1 <= corner_radius(width, height, roundness).
// See "Corner saturation condition" in the study note. This is the precise,
// honest replacement for the informal "corners are always 1" claim: it holds
// for the default/moderate parameter grid this study freezes fixtures at, and
// the boundary test right after this one demonstrates, rather than hides, the
// small-canvas case where it does not.
// ---------------------------------------------------------------------------

#[test]
fn corner_mask_reaches_one_exactly_when_the_derived_saturation_condition_holds() {
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            let r_corner = corner_radius(width, height, roundness);
            for midpoint in [0.0, 25.0, 50.0, 75.0, 90.0, 99.0] {
                for feather in [0.0, 25.0, 50.0, 75.0, 90.0, 99.0] {
                    let m = midpoint / 100.0;
                    let f = feather / 100.0;
                    let r1 = m + (1.0 - m) * f;
                    let params = VignetteParams {
                        amount: 0.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    let corner_mask_value = mask(0, 0, width, height, &params);
                    if r1 <= r_corner {
                        assert_eq!(
                            corner_mask_value, 1.0,
                            "{width}x{height} roundness={roundness} midpoint={midpoint} \
                             feather={feather}: r1={r1} <= r_corner={r_corner} must give \
                             corner mask exactly 1"
                        );
                    } else {
                        assert!(
                            corner_mask_value < 1.0,
                            "{width}x{height} roundness={roundness} midpoint={midpoint} \
                             feather={feather}: r1={r1} > r_corner={r_corner} must give \
                             corner mask strictly below 1"
                        );
                    }
                }
            }
        }
    }
}

/// The required "corners reach 1" property, restricted to the parameter
/// subset the study actually recommends production be checked against on
/// small canvases (midpoint and feather each at most 75, well inside the
/// derived saturation condition for every ASPECTS size and every roundness --
/// see the study note's table of `r_corner` values, all >= ~0.947, versus the
/// worst-case r1 = 0.9375 this grid reaches).
#[test]
fn corner_mask_is_exactly_one_for_the_frozen_default_and_moderate_extreme_grid() {
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            for midpoint in [0.0, 25.0, 50.0, 75.0] {
                for feather in [0.0, 25.0, 50.0, 75.0] {
                    let params = VignetteParams {
                        amount: 0.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    for (x, y) in corners(width, height) {
                        assert_eq!(
                            mask(x, y, width, height, &params),
                            1.0,
                            "{width}x{height} ({x},{y}) {params:?}: corner mask must be 1"
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: negative amount = -100 blackens every corner exactly, for every
// parameter combination where the corner mask itself is exactly 1 (see
// above); this is the direct consequence of `apply`'s gain formula.
// ---------------------------------------------------------------------------

#[test]
fn corners_go_black_at_amount_minus_100_whenever_corner_mask_is_one() {
    let rgb = [0.6, 0.35, 0.05];
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            for midpoint in [0.0, 25.0, 50.0, 75.0] {
                for feather in [0.0, 25.0, 50.0, 75.0] {
                    let params = VignetteParams {
                        amount: -100.0,
                        midpoint,
                        roundness,
                        feather,
                    };
                    for (x, y) in corners(width, height) {
                        let out = vignette_pixel(x, y, width, height, rgb, &params);
                        assert_eq!(
                            out,
                            [0.0, 0.0, 0.0],
                            "{width}x{height} ({x},{y}) {params:?}: amount=-100 must blacken \
                             a corner whose mask is 1"
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: positive amount never exceeds encoded 1 -- but only for a
// channel whose *input* is already below encoded white. This is the frozen
// equation's actual guarantee (see `apply`'s doc comment: "E' <= 1... for
// whenever E < 1"), not a blanket clamp: a channel that starts at or above
// encoded white (an out-of-gamut input already preserved from an earlier
// stage) is passed through completely unchanged by the "else" branch, so it
// stays exactly as far above 1 as it started, by design (see the mirror test
// below). The first version of this test asserted the blanket claim and
// failed on a randomly sampled channel at linear 1.587 (encoded ~1.224,
// already above white before `apply` ran); fixed here to sample only
// below-white channels, which is the domain the property actually covers.
// ---------------------------------------------------------------------------

fn encode_ext(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn positive_amount_never_exceeds_encoded_one_for_a_below_white_input() {
    let mut rng = SplitMix64(99);
    let mut sampled_any_below_white = false;
    for _ in 0..2000 {
        // Sample channels below linear 1.0 (encoded < 1.0, since encode_ext
        // is monotone increasing and encode_ext(1.0) == 1.0), including
        // negative (below-black) out-of-gamut values, which are still below
        // encoded white.
        let rgb = [
            rng.next_range(-0.5, 0.999_999),
            rng.next_range(-0.5, 0.999_999),
            rng.next_range(-0.5, 0.999_999),
        ];
        let m = rng.next_range(0.0, 1.0);
        let amount = rng.next_range(0.0, 100.0);
        let out = apply(rgb, m, amount);
        for channel in out {
            let encoded = encode_ext(channel);
            assert!(
                encoded <= 1.0 + 1e-9,
                "rgb={rgb:?} mask={m} amount={amount}: encoded output {encoded} exceeds 1"
            );
            sampled_any_below_white = true;
        }
    }
    assert!(sampled_any_below_white);
    // The exact boundary: amount=100, mask=1, encoded input already at
    // exactly white must map to exactly encoded 1 (decode_ext(1.0) = 1.0).
    assert_eq!(apply([1.0, 1.0, 1.0], 1.0, 100.0), [1.0, 1.0, 1.0]);
}

#[test]
fn a_channel_already_at_or_above_encoded_white_is_passed_through_unchanged() {
    // The design's "else" branch (E >= 1: E' = E, unchanged) means an
    // out-of-gamut input already above white is not brought down to 1 by a
    // positive amount -- it stays exactly as it was, for every mask and
    // amount. This is the honest complement to the property above: "never
    // exceeds 1" is a guarantee about not *pushing* a value past white, not
    // a general clamp.
    for input in [1.0, 1.2, 1.5868993113623606, 3.0] {
        assert!(
            encode_ext(input) >= 1.0 - 1e-12,
            "fixture input must already be at/above white"
        );
        for mask_value in [0.0, 0.25, 0.5, 1.0] {
            for amount in [1.0, 50.0, 100.0] {
                let out = apply([input, input, input], mask_value, amount);
                for channel in out {
                    assert!(
                        (channel - input).abs() < 1e-9,
                        "input={input} mask={mask_value} amount={amount}: expected unchanged \
                         {input}, got {channel}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property: finite parameters and finite input never produce a non-finite
// output (production refuses non-finite input before this stage; this
// reference proves it never manufactures non-finite output from finite
// input, the same claim `tone.rs`'s reference makes for Tone).
// ---------------------------------------------------------------------------

#[test]
fn finite_input_and_parameters_never_produce_non_finite_output() {
    let mut rng = SplitMix64(7);
    for _ in 0..500 {
        let width = 4 + rng.next_u32(60);
        let height = 4 + rng.next_u32(60);
        let x = rng.next_u32(width);
        let y = rng.next_u32(height);
        let params = VignetteParams {
            amount: rng.next_range(-100.0, 100.0),
            midpoint: rng.next_range(0.0, 100.0),
            roundness: rng.next_range(-100.0, 100.0),
            feather: rng.next_range(0.0, 100.0),
        };
        let rgb = [
            rng.next_range(-1.0, 3.0),
            rng.next_range(-1.0, 3.0),
            rng.next_range(-1.0, 3.0),
        ];
        let m = mask(x, y, width, height, &params);
        assert!(
            m.is_finite(),
            "{width}x{height} ({x},{y}) {params:?}: mask={m}"
        );
        let out = vignette_pixel(x, y, width, height, rgb, &params);
        assert!(
            out.iter().all(|c| c.is_finite()),
            "{width}x{height} ({x},{y}) {params:?} rgb={rgb:?}: out={out:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Hand-computed worked examples (also recorded, with the arithmetic shown
// step by step, in docs/design/vignette-study.md). Three or more cases, per
// the task brief.
// ---------------------------------------------------------------------------

#[test]
fn hand_computed_example_one_ellipse_corner_at_defaults_is_one() {
    // 24x16, roundness=0 (ellipse, a=b=0.5), midpoint=50, feather=50, corner
    // (0,0). u=-0.9583333..., v=-0.9375, r=sqrt(0.5*u^2+0.5*v^2)=0.947973...,
    // r0=0.25, r1=0.75, t=clamp((r-r0)/(r1-r0),0,1)=clamp(1.39594...,0,1)=1,
    // mask=smoothstep(1)=1.
    let params = defaults();
    let m = mask(0, 0, 24, 16, &params);
    assert!((m - 1.0).abs() < 1e-9, "m={m}");
}

#[test]
fn hand_computed_example_two_ellipse_near_centre_at_defaults_is_zero() {
    // 24x16, roundness=0, midpoint=50, feather=50, pixel (11,7) (adjacent to
    // the geometric centre of an even-sized frame). u=-0.041666..., v=-0.0625,
    // r=sqrt(0.5*u^2+0.5*v^2)=0.053114..., well below r0=0.25, so mask=0
    // (hard floor of the clamp, t=0).
    let params = defaults();
    let m = mask(11, 7, 24, 16, &params);
    assert_eq!(m, 0.0);
}

#[test]
fn hand_computed_example_three_boundary_case_corner_mask_below_one() {
    // 20x20 square, roundness=0, midpoint=90, feather=90: r0=0.09, r1=0.99.
    // The discrete corner's radius is only 0.95 (< r1=0.99), so t =
    // (0.95-0.09)/(0.99-0.09) = 0.86/0.9 = 0.955555..., and
    // mask=smoothstep(0.955555...) = 0.994249657064..., strictly below 1.
    // This is the honest boundary case: see "Corner saturation condition".
    let params = VignetteParams {
        amount: 0.0,
        midpoint: 90.0,
        roundness: 0.0,
        feather: 90.0,
    };
    let m = mask(0, 0, 20, 20, &params);
    assert!(
        (m - 0.994_249_657_064_471_7).abs() < 1e-12,
        "m={m}, expected ~0.9942496570644717"
    );
    assert!(m < 1.0);
}

#[test]
fn hand_computed_example_four_amount_application_at_mask_one_and_mask_half() {
    // rgb=[0.5,0.3,0.1], mask=1, amount=-100: gain=1-1*1=0 -> [0,0,0].
    assert_eq!(apply([0.5, 0.3, 0.1], 1.0, -100.0), [0.0, 0.0, 0.0]);
    // Same rgb, mask=1, amount=+100: every channel's encoded value lifts to
    // exactly 1 (a=1, mask=1, E'=E+1*(1-E)=1), decoding back to linear 1.0.
    assert_eq!(apply([0.5, 0.3, 0.1], 1.0, 100.0), [1.0, 1.0, 1.0]);
    // mask=0.5, amount=+50 (a=0.5): E'=E+0.25*(1-E). For c=0.5,
    // E=encode_ext(0.5)=0.735356983..., E'=0.801517737...,
    // decode_ext(E')=0.606403030961...
    let out = apply([0.5, 0.3, 0.1], 0.5, 50.0);
    let expected = [
        0.606_403_030_961_760_7,
        0.430_913_342_049_097_1,
        0.225_214_369_799_676_6,
    ];
    for (o, e) in out.iter().zip(expected.iter()) {
        assert!((o - e).abs() < 1e-12, "o={o} e={e}");
    }
}

// ---------------------------------------------------------------------------
// Oracle fixtures: fixtures/vignette/mask-cases.json, amount-cases.json
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct FixtureParams {
    amount: f64,
    midpoint: f64,
    roundness: f64,
    feather: f64,
}

impl From<FixtureParams> for VignetteParams {
    fn from(p: FixtureParams) -> Self {
        VignetteParams {
            amount: p.amount,
            midpoint: p.midpoint,
            roundness: p.roundness,
            feather: p.feather,
        }
    }
}

impl From<VignetteParams> for FixtureParams {
    fn from(p: VignetteParams) -> Self {
        FixtureParams {
            amount: p.amount,
            midpoint: p.midpoint,
            roundness: p.roundness,
            feather: p.feather,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct MaskCase {
    label: String,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    params: FixtureParams,
    expected_mask: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct AmountCase {
    label: String,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    input_linear_rgb: [f64; 3],
    params: FixtureParams,
    expected_mask: f64,
    expected_linear_rgb: [f64; 3],
}

fn mask_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vignette/mask-cases.json")
}

fn amount_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vignette/amount-cases.json")
}

/// A synthetic "gradient" linear-RGB input: varies smoothly with pixel
/// position so amount-cases.json exercises a non-flat source, per the task
/// brief's "flat fields and gradients" requirement.
fn gradient_input(x: u32, y: u32, width: u32, height: u32) -> [f64; 3] {
    let fx = f64::from(x) / f64::from(width.max(1) - 1).max(1.0);
    let fy = f64::from(y) / f64::from(height.max(1) - 1).max(1.0);
    [fx, fy, 0.5 * (fx + fy)]
}

fn build_mask_cases() -> Vec<MaskCase> {
    let mut cases = Vec::new();
    let mut push =
        |label: String, width: u32, height: u32, x: u32, y: u32, params: VignetteParams| {
            let expected = mask(x, y, width, height, &params);
            cases.push(MaskCase {
                label,
                width,
                height,
                x,
                y,
                params: params.into(),
                expected_mask: expected,
            });
        };

    // Defaults, every aspect, corners + near-centre.
    for &(width, height) in &ASPECTS {
        let params = defaults();
        for (name, (x, y)) in [
            ("corner-tl", (0, 0)),
            ("corner-tr", (width - 1, 0)),
            ("corner-bl", (0, height - 1)),
            ("corner-br", (width - 1, height - 1)),
            ("near-centre", near_centre(width, height)),
        ] {
            push(
                format!("defaults/{width}x{height}/{name}"),
                width,
                height,
                x,
                y,
                params,
            );
        }
    }

    // Roundness sweep at defaults' midpoint/feather, every aspect, corner and
    // near-centre (roundness never changes the near-centre-is-0 result, but
    // it is recorded anyway as a direct check).
    for &(width, height) in &ASPECTS {
        for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            let params = VignetteParams {
                amount: 0.0,
                midpoint: 50.0,
                roundness,
                feather: 50.0,
            };
            push(
                format!("roundness/{width}x{height}/{roundness}/corner-tl"),
                width,
                height,
                0,
                0,
                params,
            );
            let (cx, cy) = near_centre(width, height);
            push(
                format!("roundness/{width}x{height}/{roundness}/near-centre"),
                width,
                height,
                cx,
                cy,
                params,
            );
        }
    }

    // Midpoint/feather grid at roundness=0, every aspect, corner only (the
    // property this grid demonstrates: corner mask stays 1 until r1 exceeds
    // the discrete corner radius).
    for &(width, height) in &ASPECTS {
        for midpoint in [0.0, 25.0, 50.0, 75.0, 90.0, 99.0, 100.0] {
            for feather in [0.0, 25.0, 50.0, 75.0, 90.0, 99.0, 100.0] {
                let params = VignetteParams {
                    amount: 0.0,
                    midpoint,
                    roundness: 0.0,
                    feather,
                };
                push(
                    format!("midpoint-feather/{width}x{height}/{midpoint}/{feather}/corner-tl"),
                    width,
                    height,
                    0,
                    0,
                    params,
                );
            }
        }
    }

    // The explicit boundary example (hand-computed example three above).
    push(
        "boundary/20x20/midpoint90-feather90/corner-tl".to_string(),
        20,
        20,
        0,
        0,
        VignetteParams {
            amount: 0.0,
            midpoint: 90.0,
            roundness: 0.0,
            feather: 90.0,
        },
    );

    // Midpoint = 100 edge cases (falloff starts at the corners; f=0 makes it
    // a hard step whose r0=r1=1, so corner mask=0; f>0 with m=100 gives
    // r0=1, r1=1, still r0==r1 since (1-m)=0 forces r1=m=1 regardless of f).
    for &(width, height) in &ASPECTS {
        for feather in [0.0, 50.0, 100.0] {
            let params = VignetteParams {
                amount: 0.0,
                midpoint: 100.0,
                roundness: 0.0,
                feather,
            };
            push(
                format!("midpoint100/{width}x{height}/feather{feather}/corner-tl"),
                width,
                height,
                0,
                0,
                params,
            );
        }
    }

    cases
}

fn build_amount_cases() -> Vec<AmountCase> {
    let mut cases = Vec::new();
    let flat = [0.5, 0.3, 0.1];
    let mut push = |label: String,
                    width: u32,
                    height: u32,
                    x: u32,
                    y: u32,
                    input: [f64; 3],
                    params: VignetteParams| {
        let m = mask(x, y, width, height, &params);
        let expected = apply(input, m, params.amount);
        cases.push(AmountCase {
            label,
            width,
            height,
            x,
            y,
            input_linear_rgb: input,
            params: params.into(),
            expected_mask: m,
            expected_linear_rgb: expected,
        });
    };

    for &(width, height) in &ASPECTS {
        for amount in [-100.0, -50.0, 50.0, 100.0] {
            let params = VignetteParams {
                amount,
                midpoint: 50.0,
                roundness: 0.0,
                feather: 50.0,
            };
            for (name, (x, y)) in [
                ("corner-tl", (0, 0)),
                ("near-centre", near_centre(width, height)),
            ] {
                push(
                    format!("flat/{width}x{height}/{amount}/{name}"),
                    width,
                    height,
                    x,
                    y,
                    flat,
                    params,
                );
                push(
                    format!("gradient/{width}x{height}/{amount}/{name}"),
                    width,
                    height,
                    x,
                    y,
                    gradient_input(x, y, width, height),
                    params,
                );
            }
        }
    }

    // amount = -100 at every corner and every aspect: the "corners go black"
    // fixture, on the flat input.
    for &(width, height) in &ASPECTS {
        let params = VignetteParams {
            amount: -100.0,
            midpoint: 50.0,
            roundness: 0.0,
            feather: 50.0,
        };
        for (name, (x, y)) in [
            ("corner-tl", (0, 0)),
            ("corner-tr", (width - 1, 0)),
            ("corner-bl", (0, height - 1)),
            ("corner-br", (width - 1, height - 1)),
        ] {
            push(
                format!("black-corners/{width}x{height}/{name}"),
                width,
                height,
                x,
                y,
                flat,
                params,
            );
        }
    }

    cases
}

#[test]
fn committed_mask_case_fixture_matches_the_reference() {
    let path = mask_fixture_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let committed: Vec<MaskCase> = serde_json::from_str(&contents)
        .expect("fixtures/vignette/mask-cases.json must be valid JSON");
    let fresh = build_mask_cases();
    assert_eq!(
        committed.len(),
        fresh.len(),
        "the committed mask fixture has a different case count than the reference produces; \
         regenerate it with `cargo test -p lightwell-reference --test studies -- --ignored \
         regenerate_committed_vignette_fixtures`"
    );
    for (committed_case, fresh_case) in committed.iter().zip(fresh.iter()) {
        assert_eq!(
            committed_case.label, fresh_case.label,
            "case order or labels drifted"
        );
        assert_eq!(
            committed_case.params, fresh_case.params,
            "params drifted for {:?}",
            committed_case.label
        );
        assert_eq!(
            (
                committed_case.width,
                committed_case.height,
                committed_case.x,
                committed_case.y
            ),
            (
                fresh_case.width,
                fresh_case.height,
                fresh_case.x,
                fresh_case.y
            ),
            "geometry drifted for {:?}",
            committed_case.label
        );
        assert!(
            approximately_equal(committed_case.expected_mask, fresh_case.expected_mask),
            "fixtures/vignette/mask-cases.json is stale for {:?}: committed {}, reference {}",
            committed_case.label,
            committed_case.expected_mask,
            fresh_case.expected_mask
        );
    }
}

#[test]
fn committed_amount_case_fixture_matches_the_reference() {
    let path = amount_fixture_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let committed: Vec<AmountCase> = serde_json::from_str(&contents)
        .expect("fixtures/vignette/amount-cases.json must be valid JSON");
    let fresh = build_amount_cases();
    assert_eq!(
        committed.len(),
        fresh.len(),
        "the committed amount fixture has a different case count than the reference produces; \
         regenerate it with `cargo test -p lightwell-reference --test studies -- --ignored \
         regenerate_committed_vignette_fixtures`"
    );
    for (committed_case, fresh_case) in committed.iter().zip(fresh.iter()) {
        assert_eq!(
            committed_case.label, fresh_case.label,
            "case order or labels drifted"
        );
        assert_eq!(
            committed_case.params, fresh_case.params,
            "params drifted for {:?}",
            committed_case.label
        );
        assert!(
            approximately_equal(committed_case.expected_mask, fresh_case.expected_mask),
            "expected_mask stale for {:?}",
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
                "fixtures/vignette/amount-cases.json is stale for {:?}: committed {a}, reference {b}",
                committed_case.label
            );
        }
    }
}

#[test]
#[ignore = "regenerates the committed oracle fixtures; run explicitly after changing the frozen \
            equations in crates/lightwell-reference/src/vignette.rs, and re-freeze \
            docs/design/vignette-study.md to match"]
fn regenerate_committed_vignette_fixtures() {
    let mask_cases = build_mask_cases();
    let mask_json = serde_json::to_string_pretty(&mask_cases).expect("serializable");
    std::fs::write(mask_fixture_path(), mask_json)
        .expect("write fixtures/vignette/mask-cases.json");

    let amount_cases = build_amount_cases();
    let amount_json = serde_json::to_string_pretty(&amount_cases).expect("serializable");
    std::fs::write(amount_fixture_path(), amount_json)
        .expect("write fixtures/vignette/amount-cases.json");
}
