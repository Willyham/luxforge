//! TASK-001: independent proofs for the frozen mask coverage mathematics — mask
//! space, the component composition algebra and the linear and radial falloffs.
//!
//! This binary shares no code with `luxforge-core`'s production sources. The
//! frozen equations live in `crates/luxforge-reference/src/mask.rs`; the mathematics, the
//! rejected alternatives and every measured figure quoted below are written out
//! in full in `docs/design/mask-study.md`, which this file's test names track.
//!
//! The study's 24 MP figures are printed by the one ignored test at the end:
//!
//! ```sh
//! cargo test --release --locked --package luxforge-core --test mask \
//!     -- --ignored --nocapture mask_study_figures
//! ```

use super::*;
use luxforge_reference::linear_to_code;
use luxforge_reference::mask::{
    Algebra, Component, DISTANCE_MAX, DISTANCE_MIN, Easing, Kind, Linear, Mask, Mode, Radial,
    Stage, axis_is_legal, blend, combine, component_coverage, coverage, distance_is_legal, ease,
    linear_coverage, radial_coverage, radial_coverage_branch_form, smooth,
};

// ---------------------------------------------------------------------------
// The study's own inputs.
// ---------------------------------------------------------------------------

/// The frozen tolerance, `1e-6 + 1e-6·|reference|` in coverage units.
fn tolerance(reference: f64) -> f64 {
    1e-6 + 1e-6 * reference.abs()
}

/// A legal linear payload with a generous axis.
fn sample_linear(rng: &mut SplitMix64) -> Linear {
    loop {
        let linear = Linear {
            x0: rng.next_range(0.0, 1.0),
            y0: rng.next_range(0.0, 1.0),
            x1: rng.next_range(0.0, 1.0),
            y1: rng.next_range(0.0, 1.0),
        };
        if axis_is_legal(&linear, &LANDSCAPE) && axis_is_legal(&linear, &PORTRAIT) {
            return linear;
        }
    }
}

/// A legal radial payload spanning the feather range, including the hard edge.
fn sample_radial(rng: &mut SplitMix64) -> Radial {
    Radial {
        x: rng.next_range(0.0, 1.0),
        y: rng.next_range(0.0, 1.0),
        radius_x: rng.next_range(0.02, 0.60),
        radius_y: rng.next_range(0.02, 0.60),
        angle: rng.next_range(-180.0, 180.0),
        feather: if rng.next_usize(8) == 0 {
            0.0
        } else {
            rng.next_range(0.0, 100.0)
        },
    }
}

fn sample_component(rng: &mut SplitMix64, mode: Mode) -> Component {
    Component {
        mode,
        invert: rng.next_bool(),
        kind: if rng.next_bool() {
            Kind::Linear(sample_linear(rng))
        } else {
            Kind::Radial(sample_radial(rng))
        },
    }
}

fn sample_mode(rng: &mut SplitMix64) -> Mode {
    match rng.next_usize(3) {
        0 => Mode::Add,
        1 => Mode::Subtract,
        _ => Mode::Intersect,
    }
}

/// A randomized mask whose first component is `Add`, as the model requires.
fn sample_mask(rng: &mut SplitMix64, components: usize) -> Mask {
    let mut list = vec![sample_component(rng, Mode::Add)];
    for _ in 1..components {
        let mode = sample_mode(rng);
        list.push(sample_component(rng, mode));
    }
    Mask {
        amount: rng.next_range(0.0, 100.0),
        invert: rng.next_bool(),
        components: list,
    }
}

/// A coarse but dense sample of mask space over a stage: every `step`-th pixel
/// centre on each axis.
fn grid(stage: &Stage, step: u32) -> Vec<(f64, f64)> {
    let mut points = Vec::new();
    let mut py = 0;
    while py < stage.height {
        let mut px = 0;
        while px < stage.width {
            points.push(stage.pixel_uv(px, py));
            px += step;
        }
        py += step;
    }
    points
}

// ---------------------------------------------------------------------------
// Mask space.
// ---------------------------------------------------------------------------

/// The worked examples the study states in prose, checked as numbers.
#[test]
fn worked_examples_map_stored_positions_to_mask_space_as_documented() {
    // Landscape 6000x4000: W/H = 1.5, so mask space is (0, 1.5) x (0, 1).
    assert_eq!(LANDSCAPE.aspect(), 1.5);
    assert_eq!(LANDSCAPE.position_uv(0.0, 0.0), (0.0, 0.0));
    assert_eq!(LANDSCAPE.position_uv(1.0, 1.0), (1.5, 1.0));
    assert_eq!(LANDSCAPE.position_uv(0.5, 0.25), (0.75, 0.25));
    // The centre pixel's own mask-space coordinate: (3000 + 0.5)/4000 = 0.750125,
    // (2000 + 0.5)/4000 = 0.500125.
    assert_eq!(LANDSCAPE.pixel_uv(3000, 2000), (0.750_125, 0.500_125));
    // A stored radius of 0.25 is 0.25 * 4000 = 1000 px on both axes.
    assert_eq!(0.25 * f64::from(LANDSCAPE.height), 1000.0);

    // Portrait 4000x6000: W/H = 2/3, so mask space is (0, 0.666...) x (0, 1).
    assert_eq!(PORTRAIT.aspect(), 4000.0 / 6000.0);
    assert_eq!(PORTRAIT.position_uv(1.0, 1.0).1, 1.0);
    assert!((PORTRAIT.position_uv(1.0, 1.0).0 - 0.666_666_666_666_666_6).abs() < 1e-15);
    assert_eq!(
        PORTRAIT.pixel_uv(2000, 3000),
        (0.333_416_666_666_666_65, 0.500_083_333_333_333_3)
    );
    // The same stored radius is 0.25 * 6000 = 1500 px on both axes.
    assert_eq!(0.25 * f64::from(PORTRAIT.height), 1500.0);
}

/// The property the convention exists for: a component with equal radii covers
/// a pixel region as wide as it is tall, on a landscape and on a portrait stage,
/// measured in pixels rather than argued in prose.
#[test]
fn a_circle_is_a_circle_in_pixels_at_every_aspect_ratio() {
    for stage in [LANDSCAPE, PORTRAIT] {
        let radial = Radial {
            x: 0.5,
            y: 0.5,
            radius_x: 0.2,
            radius_y: 0.2,
            angle: 0.0,
            feather: 0.0,
        };
        let (cu, cv) = stage.position_uv(radial.x, radial.y);
        // Walk out from the centre on each axis and count covered pixels.
        let mut horizontal = 0u32;
        for px in 0..stage.width {
            let (u, _) = stage.pixel_uv(px, 0);
            if radial_coverage(&radial, &stage, u, cv) > 0.0 {
                horizontal += 1;
            }
        }
        let mut vertical = 0u32;
        for py in 0..stage.height {
            let (_, v) = stage.pixel_uv(0, py);
            if radial_coverage(&radial, &stage, cu, v) > 0.0 {
                vertical += 1;
            }
        }
        assert_eq!(
            horizontal, vertical,
            "{}x{}: covered span {horizontal} px across vs {vertical} px down",
            stage.width, stage.height
        );
        // And it is the diameter the stored radius names: 0.2 * H * 2 pixels.
        let expected = (0.4 * f64::from(stage.height)).round() as u32;
        assert!(
            horizontal.abs_diff(expected) <= 1,
            "{}x{}: covered {horizontal} px, expected about {expected} px",
            stage.width,
            stage.height
        );
        println!(
            "{}x{}: radius 0.2 covers {horizontal} px across and {vertical} px down (nominal {expected})",
            stage.width, stage.height
        );
    }
}

/// `pixel_uv` and `position_uv` are the same map written two ways. They agree
/// far inside the frozen tolerance but not bit for bit, which is why each
/// caller is frozen to one of them.
#[test]
fn the_pixel_and_position_spellings_agree_within_tolerance_but_not_bit_for_bit() {
    let mut worst = 0.0_f64;
    let mut differed = 0usize;
    let mut total = 0usize;
    for stage in [LANDSCAPE, PORTRAIT] {
        let mut py = 0;
        while py < stage.height {
            let mut px = 0;
            while px < stage.width {
                let (u_pixel, v_pixel) = stage.pixel_uv(px, py);
                let x = (f64::from(px) + 0.5) / f64::from(stage.width);
                let y = (f64::from(py) + 0.5) / f64::from(stage.height);
                let (u_position, v_position) = stage.position_uv(x, y);
                worst = worst
                    .max((u_pixel - u_position).abs())
                    .max((v_pixel - v_position).abs());
                if u_pixel != u_position || v_pixel != v_position {
                    differed += 1;
                }
                total += 1;
                px += 7;
            }
            py += 11;
        }
    }
    assert!(
        worst < tolerance(1.5),
        "spellings disagree by {worst}, above the frozen tolerance"
    );
    assert!(
        differed > 0,
        "the two spellings were bit-identical at all {total} sampled pixels, \
         which would make the frozen distinction unnecessary"
    );
    assert!(worst < 1e-15, "worst spelling deviation {worst}");
    println!(
        "spelling deviation max {worst:.3e}; {differed} of {total} sampled pixels differ in bits"
    );
}

// ---------------------------------------------------------------------------
// The linear gradient.
// ---------------------------------------------------------------------------

/// Coverage is exactly 0 at `p0` and exactly 1 at `p1`: at `p1` the numerator is
/// computed as `du*du + dv*dv` in the same order as `l2`, so the ratio is
/// exactly `1.0` and `smooth(1.0)` is exactly `1.0`.
#[test]
fn linear_coverage_is_exactly_zero_at_p0_and_exactly_one_at_p1() {
    let mut rng = SplitMix64(0x5EED_0001);
    for _ in 0..2000 {
        let linear = sample_linear(&mut rng);
        for stage in [LANDSCAPE, PORTRAIT] {
            let (u0, v0) = stage.position_uv(linear.x0, linear.y0);
            let (u1, v1) = stage.position_uv(linear.x1, linear.y1);
            assert_eq!(linear_coverage(&linear, &stage, u0, v0), 0.0);
            assert_eq!(linear_coverage(&linear, &stage, u1, v1), 1.0);
        }
    }
}

/// Coverage depends only on the projection onto the axis, so it is constant
/// along every line perpendicular to it.
#[test]
fn linear_coverage_is_constant_perpendicular_to_the_axis() {
    let mut rng = SplitMix64(0x5EED_0002);
    let mut worst = 0.0_f64;
    for _ in 0..500 {
        let linear = sample_linear(&mut rng);
        let (u0, v0) = LANDSCAPE.position_uv(linear.x0, linear.y0);
        let (u1, v1) = LANDSCAPE.position_uv(linear.x1, linear.y1);
        let (du, dv) = (u1 - u0, v1 - v0);
        let length = (du * du + dv * dv).sqrt();
        let (nu, nv) = (-dv / length, du / length);
        for step in 0..9 {
            let t = f64::from(step) / 8.0;
            let (bu, bv) = (u0 + t * du, v0 + t * dv);
            let base = linear_coverage(&linear, &LANDSCAPE, bu, bv);
            for offset in [-0.4, -0.1, 0.1, 0.4] {
                let moved =
                    linear_coverage(&linear, &LANDSCAPE, bu + offset * nu, bv + offset * nv);
                worst = worst.max((moved - base).abs());
            }
        }
    }
    assert!(
        worst < tolerance(1.0),
        "perpendicular deviation {worst} exceeds the frozen tolerance"
    );
}

/// Coverage is nondecreasing from `p0` toward `p1` and clamps flat outside the
/// axis, which is what makes the drag direction mean what the gesture says.
#[test]
fn linear_coverage_is_nondecreasing_along_the_axis_and_flat_outside_it() {
    let mut rng = SplitMix64(0x5EED_0003);
    for _ in 0..500 {
        let linear = sample_linear(&mut rng);
        let (u0, v0) = LANDSCAPE.position_uv(linear.x0, linear.y0);
        let (u1, v1) = LANDSCAPE.position_uv(linear.x1, linear.y1);
        let (du, dv) = (u1 - u0, v1 - v0);
        let mut previous = f64::NEG_INFINITY;
        for step in -20..=120 {
            let t = f64::from(step) / 100.0;
            let c = linear_coverage(&linear, &LANDSCAPE, u0 + t * du, v0 + t * dv);
            assert!((0.0..=1.0).contains(&c), "coverage {c} outside [0, 1]");
            assert!(
                c >= previous,
                "coverage fell from {previous} to {c} at t={t}"
            );
            previous = c;
        }
        assert_eq!(
            linear_coverage(&linear, &LANDSCAPE, u0 - 0.5 * du, v0 - 0.5 * dv),
            0.0
        );
        assert_eq!(
            linear_coverage(&linear, &LANDSCAPE, u1 + 0.5 * du, v1 + 0.5 * dv),
            1.0
        );
    }
}

/// The axis rule, stated as a bound rather than as an equality against zero: an
/// axis shorter than one `DISTANCE_MIN` in mask space is rejected, so the
/// divisor `l2` is never below `DISTANCE_MIN²`.
#[test]
fn a_degenerate_axis_is_rejected_by_the_frozen_rule() {
    let zero = Linear {
        x0: 0.4,
        y0: 0.4,
        x1: 0.4,
        y1: 0.4,
    };
    assert!(!axis_is_legal(&zero, &LANDSCAPE));
    // Half the floor, laid along the unit (vertical) axis.
    let short = Linear {
        x0: 0.4,
        y0: 0.4,
        x1: 0.4,
        y1: 0.4 + DISTANCE_MIN / 2.0,
    };
    assert!(!axis_is_legal(&short, &LANDSCAPE));
    // Twice the floor is legal. The floor itself is not a representable
    // boundary: `0.4 + DISTANCE_MIN - 0.4` rounds a few ulps either side of
    // `1e-4`, so the rule is stated as a bound and not as an exact threshold,
    // and nothing depends on which side a payload exactly at the floor lands.
    let exact = Linear {
        x0: 0.4,
        y0: 0.4,
        x1: 0.4,
        y1: 0.4 + 2.0 * DISTANCE_MIN,
    };
    assert!(axis_is_legal(&exact, &LANDSCAPE));
    assert!(distance_is_legal(DISTANCE_MIN));
    assert!(distance_is_legal(DISTANCE_MAX));
}

// ---------------------------------------------------------------------------
// The radial gradient.
// ---------------------------------------------------------------------------

/// The clamped one-branch spelling production transcribes is bit-identical to
/// the design's three-branch form, over a dense sweep that crosses `r0` and
/// `r = 1` on every side and includes the hard edge.
#[test]
fn the_clamped_radial_form_equals_the_branch_form_bit_for_bit() {
    let mut rng = SplitMix64(0x5EED_0004);
    let mut checked = 0usize;
    for _ in 0..400 {
        let radial = sample_radial(&mut rng);
        for stage in [LANDSCAPE, PORTRAIT] {
            for point in grid(&stage, 137) {
                let clamped = radial_coverage(&radial, &stage, point.0, point.1);
                let branch = radial_coverage_branch_form(&radial, &stage, point.0, point.1);
                assert_eq!(
                    clamped.to_bits(),
                    branch.to_bits(),
                    "{radial:?} at {point:?}: {clamped} vs {branch}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100_000, "only {checked} points compared");
}

/// `feather = 0` takes the explicit hard-edge branch, so no division by a
/// vanishing span is ever evaluated, and the result is exactly `1` inside and
/// exactly `0` outside. A feather so small that `1 - f/100` rounds to `1.0`
/// takes the same branch, by the same test.
#[test]
fn feather_zero_is_a_hard_edge_and_no_vanishing_span_is_divided_by() {
    let stage = LANDSCAPE;
    for feather in [0.0_f64, 1e-16, 1e-18] {
        let radial = Radial {
            x: 0.5,
            y: 0.5,
            radius_x: 0.3,
            radius_y: 0.2,
            angle: 17.0,
            feather,
        };
        let span = 1.0 - (1.0 - feather / 100.0);
        assert_eq!(span, 0.0, "feather {feather} did not collapse the span");
        let mut inside = 0usize;
        let mut outside = 0usize;
        for point in grid(&stage, 53) {
            let c = radial_coverage(&radial, &stage, point.0, point.1);
            assert!(c == 0.0 || c == 1.0, "hard edge produced {c}");
            if c == 1.0 {
                inside += 1;
            } else {
                outside += 1;
            }
        }
        assert!(
            inside > 0 && outside > 0,
            "{inside} inside, {outside} outside"
        );
    }
    // A small but representable feather is the smooth branch, not the hard one.
    let soft = Radial {
        x: 0.5,
        y: 0.5,
        radius_x: 0.3,
        radius_y: 0.2,
        angle: 17.0,
        feather: 0.01,
    };
    let mut intermediate = 0usize;
    for point in grid(&stage, 3) {
        let c = radial_coverage(&soft, &stage, point.0, point.1);
        if c > 0.0 && c < 1.0 {
            intermediate += 1;
        }
    }
    assert!(
        intermediate > 0,
        "feather 0.01 produced no intermediate coverage"
    );
}

/// Inside is selected: coverage is exactly `1` at the centre for every feather,
/// and exactly `0` at and beyond the ellipse's boundary.
#[test]
fn the_radial_selects_inside_exactly_at_the_centre_and_the_boundary() {
    let mut rng = SplitMix64(0x5EED_0005);
    for _ in 0..2000 {
        let radial = sample_radial(&mut rng);
        for stage in [LANDSCAPE, PORTRAIT] {
            let (cu, cv) = stage.position_uv(radial.x, radial.y);
            assert_eq!(
                radial_coverage(&radial, &stage, cu, cv),
                1.0,
                "{radial:?} centre coverage"
            );
            // Just outside the boundary along the rotated a-axis: the point with
            // `a = reach, b = 0` is `centre + reach·(cos θ, sin θ)`.
            let theta = radial.angle * std::f64::consts::PI / 180.0;
            let (ca, sa) = (theta.cos(), theta.sin());
            let reach = radial.radius_x * 1.000_001;
            let c = radial_coverage(&radial, &stage, cu + ca * reach, cv + sa * reach);
            assert_eq!(c, 0.0, "{radial:?} coverage just outside the boundary");
            // And along the b-axis, `centre + reach·(-sin θ, cos θ)`.
            let reach = radial.radius_y * 1.000_001;
            let c = radial_coverage(&radial, &stage, cu - sa * reach, cv + ca * reach);
            assert_eq!(c, 0.0, "{radial:?} coverage just outside the b boundary");
        }
    }
}

/// Coverage is nonincreasing outward from the centre along every ray, so the
/// component is one connected selection with no ring artefacts.
#[test]
fn radial_coverage_is_nonincreasing_outward_along_every_ray() {
    let mut rng = SplitMix64(0x5EED_0006);
    for _ in 0..300 {
        let radial = sample_radial(&mut rng);
        let (cu, cv) = LANDSCAPE.position_uv(radial.x, radial.y);
        for direction in 0..24 {
            let phi = f64::from(direction) * std::f64::consts::TAU / 24.0;
            let (dx, dy) = (phi.cos(), phi.sin());
            let mut previous = f64::INFINITY;
            for step in 0..=200 {
                let distance = f64::from(step) / 200.0 * 1.2;
                let c =
                    radial_coverage(&radial, &LANDSCAPE, cu + dx * distance, cv + dy * distance);
                assert!((0.0..=1.0).contains(&c), "coverage {c} outside [0, 1]");
                assert!(c <= previous, "coverage rose from {previous} to {c}");
                previous = c;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The composition algebra.
// ---------------------------------------------------------------------------

/// The frozen algebra's first claimed property: a component applied twice is a
/// component applied once, for every mode, bit for bit, over randomized
/// component lists.
#[test]
fn zadeh_composition_is_idempotent_bit_for_bit_in_every_mode() {
    let mut rng = SplitMix64(0x5EED_0007);
    let points = grid(&LANDSCAPE, 419);
    for _ in 0..300 {
        let count = 1 + rng.next_usize(6);
        let mask = sample_mask(&mut rng, count);
        for index in 0..mask.components.len() {
            let mut doubled = mask.clone();
            let repeated = doubled.components[index];
            doubled.components.insert(index + 1, repeated);
            for point in &points {
                let once = coverage(&mask, Algebra::Zadeh, &LANDSCAPE, point.0, point.1);
                let twice = coverage(&doubled, Algebra::Zadeh, &LANDSCAPE, point.0, point.1);
                assert_eq!(
                    once.to_bits(),
                    twice.to_bits(),
                    "component {index} repeated changed coverage from {once} to {twice}"
                );
            }
        }
    }
}

/// The frozen algebra's second claimed property: add components commute, bit for
/// bit, over randomized lists and randomized permutations.
#[test]
fn zadeh_add_components_are_order_independent_bit_for_bit() {
    let mut rng = SplitMix64(0x5EED_0008);
    let points = grid(&PORTRAIT, 401);
    for _ in 0..300 {
        let count = 2 + rng.next_usize(5);
        let mut mask = sample_mask(&mut rng, count);
        for component in &mut mask.components {
            component.mode = Mode::Add;
        }
        for _ in 0..4 {
            let mut shuffled = mask.clone();
            for i in (1..shuffled.components.len()).rev() {
                let j = rng.next_usize(i + 1);
                shuffled.components.swap(i, j);
            }
            for point in &points {
                let a = coverage(&mask, Algebra::Zadeh, &PORTRAIT, point.0, point.1);
                let b = coverage(&shuffled, Algebra::Zadeh, &PORTRAIT, point.0, point.1);
                assert_eq!(a.to_bits(), b.to_bits(), "reordering changed {a} to {b}");
            }
        }
    }
}

/// The product algebra has neither property, which is the evidence P2 is settled
/// on. Both failures are measured, not asserted qualitatively.
#[test]
fn the_product_algebra_is_neither_idempotent_nor_exactly_order_independent() {
    let mut rng = SplitMix64(0x5EED_0009);
    let points = grid(&LANDSCAPE, 379);

    let mut worst_idempotence = 0.0_f64;
    for _ in 0..200 {
        let count = 2 + rng.next_usize(4);
        let mask = sample_mask(&mut rng, count);
        for index in 0..mask.components.len() {
            let mut doubled = mask.clone();
            let repeated = doubled.components[index];
            doubled.components.insert(index + 1, repeated);
            for point in &points {
                let once = coverage(&mask, Algebra::Product, &LANDSCAPE, point.0, point.1);
                let twice = coverage(&doubled, Algebra::Product, &LANDSCAPE, point.0, point.1);
                worst_idempotence = worst_idempotence.max((once - twice).abs());
            }
        }
    }
    assert!(
        worst_idempotence > 0.1,
        "the product algebra's idempotence failure measured only {worst_idempotence}"
    );

    let mut worst_order = 0.0_f64;
    for _ in 0..200 {
        let count = 3 + rng.next_usize(4);
        let mut mask = sample_mask(&mut rng, count);
        for component in &mut mask.components {
            component.mode = Mode::Add;
        }
        for _ in 0..4 {
            let mut shuffled = mask.clone();
            for i in (1..shuffled.components.len()).rev() {
                let j = rng.next_usize(i + 1);
                shuffled.components.swap(i, j);
            }
            for point in &points {
                let a = coverage(&mask, Algebra::Product, &LANDSCAPE, point.0, point.1);
                let b = coverage(&shuffled, Algebra::Product, &LANDSCAPE, point.0, point.1);
                worst_order = worst_order.max((a - b).abs());
            }
        }
    }
    assert!(
        worst_order > 0.0,
        "the product algebra's reordering deviation measured exactly 0, \
         which would make its order dependence unmeasurable here"
    );
    assert!(
        worst_order < 1e-15,
        "the product algebra's reordering deviation {worst_order} is larger than expected"
    );
    // Both figures are quoted in docs/design/mask-study.md; run this test with
    // `-- --nocapture` to reprint them.
    println!("product idempotence failure  {worst_idempotence:.6}");
    println!("product reorder deviation    {worst_order:.3e}");
}

/// Composed coverage stays in `[0, 1]` for every randomized mask, at the design's
/// component limit, and `amount`/`invert` are applied in the frozen order.
#[test]
fn composed_coverage_stays_in_range_for_every_randomized_mask() {
    let mut rng = SplitMix64(0x5EED_000A);
    let points = grid(&LANDSCAPE, 311);
    for _ in 0..200 {
        let count = 1 + rng.next_usize(32);
        let mask = sample_mask(&mut rng, count);
        for point in &points {
            let m = coverage(&mask, Algebra::Zadeh, &LANDSCAPE, point.0, point.1);
            assert!(
                (0.0..=1.0).contains(&m),
                "coverage {m} outside [0, 1] for {} components",
                mask.components.len()
            );
            assert!(m.is_finite(), "coverage {m} not finite");
            assert!(
                m <= mask.amount / 100.0 + 1e-18,
                "coverage {m} exceeds amount"
            );
        }
    }
}

/// An empty component list composes to `m = 0`: an empty mask selects nothing.
#[test]
fn an_empty_component_list_selects_nothing() {
    let empty = Mask {
        amount: 100.0,
        invert: false,
        components: Vec::new(),
    };
    assert_eq!(coverage(&empty, Algebra::Zadeh, &LANDSCAPE, 0.4, 0.6), 0.0);
    let inverted = Mask {
        invert: true,
        ..empty
    };
    assert_eq!(
        coverage(&inverted, Algebra::Zadeh, &LANDSCAPE, 0.4, 0.6),
        1.0
    );
}

/// The algebra's own arithmetic error: `max` and `min` are exact, so the only
/// rounding a component list contributes is one `1 - c` per inversion or
/// subtraction. Measured against the design's 32-component limit and compared
/// with the frozen tolerance.
#[test]
fn the_algebras_own_rounding_is_orders_below_the_frozen_tolerance() {
    let mut rng = SplitMix64(0x5EED_000B);
    let mut worst = 0.0_f64;
    for _ in 0..1_000_000 {
        // Through `smooth`, so the sampled coverages are ordinary f64 values
        // rather than the generator's own multiples of 2^-53, which invert
        // exactly and would understate the step.
        let c = smooth(rng.next_range(0.0, 1.0));
        worst = worst.max((1.0 - (1.0 - c) - c).abs());
    }
    // 32 components, each contributing at most one such step.
    let bound = 32.0 * worst;
    assert!(
        bound < tolerance(1.0) / 1e6,
        "32 inversion steps could reach {bound}, not negligible against the tolerance"
    );
    println!("one 1 - c step at most {worst:.3e}; 32 of them at most {bound:.3e}");
    // max and min are exact: composing them introduces nothing at all.
    for _ in 0..100_000 {
        let m = rng.next_range(0.0, 1.0);
        let c = rng.next_range(0.0, 1.0);
        let added = combine(Algebra::Zadeh, m, Mode::Add, c);
        assert!(added == m || added == c);
        let intersected = combine(Algebra::Zadeh, m, Mode::Intersect, c);
        assert!(intersected == m || intersected == c);
    }
}

// ---------------------------------------------------------------------------
// The easing choice.
// ---------------------------------------------------------------------------

/// Every candidate easing hits both ends exactly; only the frozen one is used.
#[test]
fn every_candidate_easing_is_exact_at_both_ends() {
    for easing in [
        Easing::Smoothstep,
        Easing::Smootherstep,
        Easing::RaisedCosine,
    ] {
        assert_eq!(ease(easing, 0.0), 0.0, "{easing:?} at 0");
        assert_eq!(ease(easing, 1.0), 1.0, "{easing:?} at 1");
        assert!((ease(easing, 0.5) - 0.5).abs() < 1e-15, "{easing:?} at 0.5");
    }
    assert_eq!(smooth(0.0), 0.0);
    assert_eq!(smooth(1.0), 1.0);
}

/// The measured separation between the frozen easing and the two rejected ones,
/// and the maximum slope of each, which is the figure the study quotes.
#[test]
fn the_rejected_easings_differ_from_the_frozen_one_by_the_recorded_amounts() {
    let mut worst_cosine = 0.0_f64;
    let mut worst_smoother = 0.0_f64;
    let mut slope = [0.0_f64; 3];
    let steps = 1_000_000;
    let mut previous = [0.0_f64; 3];
    for step in 0..=steps {
        let s = step as f64 / steps as f64;
        let values = [
            ease(Easing::Smoothstep, s),
            ease(Easing::Smootherstep, s),
            ease(Easing::RaisedCosine, s),
        ];
        worst_smoother = worst_smoother.max((values[0] - values[1]).abs());
        worst_cosine = worst_cosine.max((values[0] - values[2]).abs());
        if step > 0 {
            for index in 0..3 {
                slope[index] =
                    slope[index].max((values[index] - previous[index]).abs() * steps as f64);
            }
        }
        previous = values;
    }
    // Recorded in docs/design/mask-study.md: the raised cosine departs from the
    // frozen easing by at most 0.010009 (at s = 0.2786) and smootherstep by at
    // most 0.053666 (at s = 0.7236).
    assert!(
        (worst_cosine - 0.010_008_5).abs() < 1e-6,
        "raised cosine separation {worst_cosine}"
    );
    assert!(
        (worst_smoother - 0.053_665_6).abs() < 1e-6,
        "smootherstep separation {worst_smoother}"
    );
    assert!(
        (slope[0] - 1.5).abs() < 1e-4,
        "smoothstep slope {}",
        slope[0]
    );
    assert!(
        (slope[1] - 1.875).abs() < 1e-4,
        "smootherstep slope {}",
        slope[1]
    );
    assert!(
        (slope[2] - std::f64::consts::FRAC_PI_2).abs() < 1e-4,
        "raised cosine slope {}",
        slope[2]
    );
}

// ---------------------------------------------------------------------------
// Finiteness and the double inversion.
// ---------------------------------------------------------------------------

/// Legal finite payloads never produce a non-finite coverage, at any point in
/// mask space, including well outside the frame.
#[test]
fn legal_payloads_never_produce_non_finite_coverage() {
    let mut rng = SplitMix64(0x5EED_000C);
    for _ in 0..20_000 {
        let count = 1 + rng.next_usize(8);
        let mask = sample_mask(&mut rng, count);
        for _ in 0..8 {
            let u = rng.next_range(-2.0, 4.0);
            let v = rng.next_range(-2.0, 4.0);
            let m = coverage(&mask, Algebra::Zadeh, &LANDSCAPE, u, v);
            assert!(m.is_finite(), "coverage {m} at ({u}, {v})");
            assert!((0.0..=1.0).contains(&m), "coverage {m} outside [0, 1]");
        }
    }
}

/// Inverting twice is not exactly the identity in `f64`, and the deviation is
/// ten orders of magnitude below the frozen tolerance. Stated because both the
/// component and the mask carry an invert flag.
#[test]
fn double_inversion_deviates_by_at_most_one_ulp() {
    let mut rng = SplitMix64(0x5EED_000D);
    let points = grid(&LANDSCAPE, 293);
    let mut worst = 0.0_f64;
    let mut exact = 0usize;
    let mut total = 0usize;
    for _ in 0..200 {
        // Real coverage values, not values on the generator's own 2^-53 grid:
        // a grid-aligned value inverts exactly, an arbitrary one need not.
        let component = sample_component(&mut rng, Mode::Add);
        for point in &points {
            let c = component_coverage(&component, &LANDSCAPE, point.0, point.1);
            let round_trip = 1.0 - (1.0 - c);
            if round_trip == c {
                exact += 1;
            }
            worst = worst.max((round_trip - c).abs());
            total += 1;
        }
    }
    assert!(worst <= f64::EPSILON, "double inversion deviation {worst}");
    assert!(worst < tolerance(1.0) / 1e9, "deviation {worst}");
    assert!(
        exact > 0 && exact < total,
        "{exact} of {total} inverted exactly, so the rounding is unmeasured here"
    );
    println!(
        "double inversion: max deviation {worst:.3e}, exact for {exact} of {total} coverage values"
    );
}

/// A component's own coverage and its inversion are complementary by
/// construction, so P3's default costs nothing either way: the other reading is
/// one exact subtraction.
#[test]
fn a_components_inversion_is_the_exact_complement_of_its_coverage() {
    let mut rng = SplitMix64(0x5EED_000E);
    let points = grid(&LANDSCAPE, 257);
    let mut worst = 0.0_f64;
    for _ in 0..500 {
        let mut component = sample_component(&mut rng, Mode::Add);
        component.invert = false;
        let mut inverted = component;
        inverted.invert = true;
        for point in &points {
            let straight = component_coverage(&component, &LANDSCAPE, point.0, point.1);
            let other = component_coverage(&inverted, &LANDSCAPE, point.0, point.1);
            worst = worst.max((straight + other - 1.0).abs());
        }
    }
    assert_eq!(worst, 0.0, "complement deviation {worst}");
}

// ---------------------------------------------------------------------------
// The study's measured figures, at 24 MP.
// ---------------------------------------------------------------------------

/// The three-component mask the study's 24 MP comparison uses: a vertical sky
/// gradient, an added soft radial low left and a subtracted soft radial high
/// right, which is the shape of an ordinary local edit rather than a contrived
/// worst case.
fn study_components() -> [Component; 3] {
    [
        Component {
            mode: Mode::Add,
            invert: false,
            kind: Kind::Linear(Linear {
                x0: 0.50,
                y0: 0.56,
                x1: 0.50,
                y1: 0.14,
            }),
        },
        Component {
            mode: Mode::Add,
            invert: false,
            kind: Kind::Radial(Radial {
                x: 0.28,
                y: 0.66,
                radius_x: 0.30,
                radius_y: 0.22,
                angle: 0.0,
                feather: 55.0,
            }),
        },
        Component {
            mode: Mode::Subtract,
            invert: false,
            kind: Kind::Radial(Radial {
                x: 0.70,
                y: 0.34,
                radius_x: 0.24,
                radius_y: 0.24,
                angle: 20.0,
                feather: 70.0,
            }),
        },
    ]
}

/// `+1 EV` through the coverage on an 18% grey input, quantized: the mapping
/// from a coverage difference to a difference a person could see.
fn code_at(m: f64) -> u8 {
    let input = 0.18;
    linear_to_code(blend(input, input * 2.0, m))
}

#[test]
#[ignore = "24 MP sweep: run with --release to print the study's figures"]
fn mask_study_figures() {
    let stage = LANDSCAPE;
    let components = study_components();
    let mask = Mask {
        amount: 100.0,
        invert: false,
        components: components.to_vec(),
    };
    let product = mask.clone();

    let mut worst_algebra = 0.0_f64;
    let mut sum_algebra = 0.0_f64;
    let mut overlay_differed = 0u64;
    let mut code_differed = 0u64;
    let mut worst_code = 0i32;
    let mut worst_step_zadeh = 0.0_f64;
    let mut worst_step_product = 0.0_f64;
    let mut worst_slope_change_zadeh = 0.0_f64;
    let mut worst_slope_change_product = 0.0_f64;
    let mut worst_code_step_zadeh = 0i32;
    let pixels = u64::from(stage.width) * u64::from(stage.height);

    for py in 0..stage.height {
        let mut previous_zadeh = [0.0_f64; 2];
        let mut previous_product = [0.0_f64; 2];
        for px in 0..stage.width {
            let (u, v) = stage.pixel_uv(px, py);
            let z = coverage(&mask, Algebra::Zadeh, &stage, u, v);
            let p = coverage(&product, Algebra::Product, &stage, u, v);
            let difference = (z - p).abs();
            worst_algebra = worst_algebra.max(difference);
            sum_algebra += difference;
            if (255.0 * z + 0.5).floor() != (255.0 * p + 0.5).floor() {
                overlay_differed += 1;
            }
            let (cz, cp) = (code_at(z), code_at(p));
            if cz != cp {
                code_differed += 1;
                worst_code = worst_code.max(i32::from(cz) - i32::from(cp));
            }
            if px > 0 {
                let step_z = (z - previous_zadeh[0]).abs();
                let step_p = (p - previous_product[0]).abs();
                worst_step_zadeh = worst_step_zadeh.max(step_z);
                worst_step_product = worst_step_product.max(step_p);
                worst_code_step_zadeh = worst_code_step_zadeh
                    .max((i32::from(cz) - i32::from(code_at(previous_zadeh[0]))).abs());
                if px > 1 {
                    worst_slope_change_zadeh = worst_slope_change_zadeh
                        .max((z - 2.0 * previous_zadeh[0] + previous_zadeh[1]).abs());
                    worst_slope_change_product = worst_slope_change_product
                        .max((p - 2.0 * previous_product[0] + previous_product[1]).abs());
                }
            }
            previous_zadeh = [z, previous_zadeh[0]];
            previous_product = [p, previous_product[0]];
        }
    }

    println!(
        "--- Algebra comparison, {}x{} ({pixels} px), three components ---",
        stage.width, stage.height
    );
    println!("max |M_zadeh - M_product|          {worst_algebra:.6}");
    println!(
        "mean |M_zadeh - M_product|         {:.6}",
        sum_algebra / pixels as f64
    );
    println!(
        "overlay byte differs               {overlay_differed} px ({:.2}%)",
        100.0 * overlay_differed as f64 / pixels as f64
    );
    println!(
        "output code differs at +1 EV       {code_differed} px ({:.2}%), max {worst_code} codes",
        100.0 * code_differed as f64 / pixels as f64
    );
    println!("--- Smoothness of the frozen algebra at a component crossing ---");
    println!("max adjacent-pixel |dM| zadeh      {worst_step_zadeh:.3e}");
    println!("max adjacent-pixel |dM| product    {worst_step_product:.3e}");
    println!("max second difference zadeh        {worst_slope_change_zadeh:.3e}");
    println!("max second difference product      {worst_slope_change_product:.3e}");
    println!("max adjacent-pixel |dcode| zadeh   {worst_code_step_zadeh}");

    // Easing comparison down a 4000-row vertical gradient spanning 0.14..0.56.
    let linear = Linear {
        x0: 0.50,
        y0: 0.56,
        x1: 0.50,
        y1: 0.14,
    };
    let (u0, v0) = stage.position_uv(linear.x0, linear.y0);
    let (u1, v1) = stage.position_uv(linear.x1, linear.y1);
    let (du, dv) = (u1 - u0, v1 - v0);
    let l2 = du * du + dv * dv;
    let mut worst_cosine = 0.0_f64;
    let mut worst_smoother = 0.0_f64;
    let mut worst_code_cosine = 0i32;
    let mut worst_code_smoother = 0i32;
    let mut rows_cosine = 0u32;
    let mut rows_smoother = 0u32;
    for py in 0..stage.height {
        let (u, v) = stage.pixel_uv(stage.width / 2, py);
        let t = (((u - u0) * du + (v - v0) * dv) / l2).clamp(0.0, 1.0);
        let frozen = ease(Easing::Smoothstep, t);
        let cosine = ease(Easing::RaisedCosine, t);
        let smoother = ease(Easing::Smootherstep, t);
        worst_cosine = worst_cosine.max((frozen - cosine).abs());
        worst_smoother = worst_smoother.max((frozen - smoother).abs());
        let base = i32::from(code_at(frozen));
        let dc = (base - i32::from(code_at(cosine))).abs();
        let ds = (base - i32::from(code_at(smoother))).abs();
        worst_code_cosine = worst_code_cosine.max(dc);
        worst_code_smoother = worst_code_smoother.max(ds);
        if dc > 0 {
            rows_cosine += 1;
        }
        if ds > 0 {
            rows_smoother += 1;
        }
    }
    // The two structural properties, measured on this same mask rather than only
    // on randomized lists: duplicating the subtract component, and swapping the
    // two add components.
    let mut duplicated = mask.clone();
    duplicated.components.insert(3, components[2]);
    let mut swapped = mask.clone();
    swapped.components.swap(0, 1);
    let mut worst_duplicate = [0.0_f64; 2];
    let mut worst_swap = [0.0_f64; 2];
    for py in (0..stage.height).step_by(7) {
        for px in (0..stage.width).step_by(7) {
            let (u, v) = stage.pixel_uv(px, py);
            for (index, algebra) in [Algebra::Zadeh, Algebra::Product].into_iter().enumerate() {
                let base = coverage(&mask, algebra, &stage, u, v);
                worst_duplicate[index] = worst_duplicate[index]
                    .max((coverage(&duplicated, algebra, &stage, u, v) - base).abs());
                worst_swap[index] =
                    worst_swap[index].max((coverage(&swapped, algebra, &stage, u, v) - base).abs());
            }
        }
    }
    println!("--- The two structural properties on the same mask ---");
    println!(
        "duplicate the subtract component   zadeh {:.3e}   product {:.6}",
        worst_duplicate[0], worst_duplicate[1]
    );
    println!(
        "swap the two add components        zadeh {:.3e}   product {:.3e}",
        worst_swap[0], worst_swap[1]
    );

    // One worked pixel, for the study's worked example.
    for (px, py) in [(3800u32, 1800u32), (1680, 2200)] {
        let (u, v) = stage.pixel_uv(px, py);
        let c: Vec<f64> = components
            .iter()
            .map(|component| component_coverage(component, &stage, u, v))
            .collect();
        println!(
            "pixel ({px}, {py}) u={u:.6} v={v:.6}  c = [{:.9}, {:.9}, {:.9}]  \
             M_zadeh {:.9}  M_product {:.9}",
            c[0],
            c[1],
            c[2],
            coverage(&mask, Algebra::Zadeh, &stage, u, v),
            coverage(&product, Algebra::Product, &stage, u, v)
        );
    }

    println!(
        "--- Easing comparison, one gradient down {} rows ---",
        stage.height
    );
    println!(
        "max |smoothstep - raised cosine|   {worst_cosine:.6}  ({worst_code_cosine} codes, {rows_cosine} of {} rows differ)",
        stage.height
    );
    println!(
        "max |smoothstep - smootherstep|    {worst_smoother:.6}  ({worst_code_smoother} codes, {rows_smoother} of {} rows differ)",
        stage.height
    );
}
