//! TASK-012: the production `radial` component kind against the frozen `f64` reference.
//!
//! `crates/luxforge-reference/src/mask.rs` shares no code with `luxforge-core`'s sources, and
//! `docs/design/mask-study.md` freezes the mathematics both write. The bar here is the same as the
//! linear kind's in `mask/unit.rs`: **bit-identity** rather than a tolerance, because the production
//! unit transcribes the reference's expressions in the reference's order. A failure of these tests
//! is a rewritten expression, not float noise — the study's Transcription section lists what a
//! rewrite costs, and every item in it is within tolerance and not bit-identical.
//!
//! What the study does not freeze is verified here by exhaustive evaluation on small stages instead:
//! the conservative `bounds` rectangle of a rotated ellipse, and `min_feature_px`.

use super::*;
use luxforge_core::{
    Component, ComponentMode, Mask,
    mask::{CompiledMask, RadialGradient},
};
use luxforge_reference::mask::{
    Algebra, Component as RefComponent, Kind, Mask as RefMask, Mode, Radial, coverage,
    radial_coverage,
};
use serde_json::json;

/// A legal radial payload: a centre anywhere in the widened `[-1, 2]` range, both radii inside the
/// study's legal distance range, any angle, and a feather that lands on each of the three cases the
/// study names — the explicit `0`, the `100` that starts the ramp at the centre, and a feather so
/// small that `1 - feather / 100` rounds to exactly `1.0`, which is the hard edge reached by
/// rounding rather than by an equality against zero.
fn sample_radial(rng: &mut SplitMix64) -> RadialGradient {
    RadialGradient {
        x: rng.next_range(-1.0, 2.0),
        y: rng.next_range(-1.0, 2.0),
        radius_x: match rng.next_usize(8) {
            0 => rng.next_range(1e-4, 1e-2),
            1 => rng.next_range(2.0, 12.0),
            _ => rng.next_range(0.02, 0.9),
        },
        radius_y: match rng.next_usize(8) {
            0 => rng.next_range(1e-4, 1e-2),
            1 => rng.next_range(2.0, 12.0),
            _ => rng.next_range(0.02, 0.9),
        },
        angle: rng.next_range(-180.0, 180.0),
        feather: match rng.next_usize(10) {
            0 => 0.0,
            1 => 100.0,
            2 => 1e-16,
            _ => rng.next_range(0.0, 100.0),
        },
    }
}

fn as_reference(radial: RadialGradient) -> Radial {
    Radial {
        x: radial.x,
        y: radial.y,
        radius_x: radial.radius_x,
        radius_y: radial.radius_y,
        angle: radial.angle,
        feather: radial.feather,
    }
}

fn payload(radial: RadialGradient) -> serde_json::Value {
    json!({
        "x": radial.x,
        "y": radial.y,
        "radius_x": radial.radius_x,
        "radius_y": radial.radius_y,
        "angle": radial.angle,
        "feather": radial.feather,
    })
}

/// One randomized radial mask in both spellings: the stored model the host compiles, and the
/// reference's own structure. The first component is always `add`, which the model validates
/// structurally.
fn sample_pair(rng: &mut SplitMix64, components: usize) -> (Mask, RefMask) {
    let mut mask = Mask::new("Mask 1");
    let mut reference = RefMask {
        amount: rng.next_range(0.0, 100.0),
        invert: rng.next_bool(),
        components: Vec::new(),
    };
    mask.amount = reference.amount;
    mask.invert = reference.invert;
    for index in 0..components {
        let (mode, ref_mode) = if index == 0 {
            (ComponentMode::Add, Mode::Add)
        } else {
            mode_of(rng.next_usize(3))
        };
        let radial = sample_radial(rng);
        let invert = rng.next_bool();
        let name = mask.next_component_name("radial");
        let mut component = Component::new(name, mode, "radial", payload(radial));
        component.invert = invert;
        mask.components.push(component);
        reference.components.push(RefComponent {
            mode: ref_mode,
            invert,
            kind: Kind::Radial(as_reference(radial)),
        });
    }
    (mask, reference)
}

/// One stored mask holding a single radial component as drawn, at full amount.
fn single(radial: RadialGradient) -> Mask {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("radial");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "radial",
        payload(radial),
    ));
    mask
}

fn compile(radial: RadialGradient, width: u32, height: u32) -> CompiledMask {
    CompiledMask::new(
        &single(radial),
        stage(width, height),
        &luxforge_core::path::StrokeTable::default(),
    )
    .expect("a legal radial compiles")
}

// ---------------------------------------------------------------------------
// Bit-identity with the frozen reference.
// ---------------------------------------------------------------------------

/// The production radial field against the frozen reference, bit for bit, over randomized centres,
/// radii, angles, feathers, stages and pixels. `to_bits` rather than `==` so a `-0.0` or a NaN could
/// not pass as equal, and the assertion prints both patterns when it fails.
#[test]
fn the_compiled_radial_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x4A5C_0012);
    let mut checked = 0usize;
    for components in 1..=6 {
        for round in 0..40 {
            let (mask, reference) = sample_pair(&mut rng, components);
            mask.validate()
                .expect("the sampled mask is structurally valid");
            for (width, height) in STAGES {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &luxforge_core::path::StrokeTable::default(),
                )
                .expect("a legal payload compiles");
                let reference_stage = ref_stage(width, height);
                for _ in 0..24 {
                    let x = rng.next_usize(width as usize) as u32;
                    let y = rng.next_usize(height as usize) as u32;
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    let expected = coverage(&reference, Algebra::Zadeh, &reference_stage, u, v);
                    let actual = compiled.coverage(x, y, ANY_PIXEL);
                    assert_eq!(
                        actual.to_bits(),
                        expected.to_bits(),
                        "{components} components, round {round}, {width}x{height} at ({x}, {y}): \
                         {actual:?} ({:#018x}) against the reference {expected:?} ({:#018x})",
                        actual.to_bits(),
                        expected.to_bits()
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 6 * 40 * 4 * 24, "the sweep's size is stated");
    println!("{checked} composed radial coverages bit-identical to the reference");
}

/// The same comparison walked densely over whole small stages, so the clamp's two ends, the exactly
/// one plateau inside `r0` and the exactly zero plateau beyond the boundary are swept rather than
/// sampled — and so the two hard-edge routes, which a sparse sample would rarely land on either
/// side of, are crossed on every row.
#[test]
fn radial_bit_identity_holds_over_whole_small_stages() {
    let mut rng = SplitMix64(0x4A5C_0013);
    let stages = [(31u32, 17u32), (17, 31), (64, 64), (48, 12)];
    let mut checked = 0usize;
    for components in 1..=4 {
        for _ in 0..20 {
            let (mask, reference) = sample_pair(&mut rng, components);
            for (width, height) in stages {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &luxforge_core::path::StrokeTable::default(),
                )
                .unwrap();
                let reference_stage = ref_stage(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let (u, v) = reference_stage.pixel_uv(x, y);
                        assert_eq!(
                            compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                            coverage(&reference, Algebra::Zadeh, &reference_stage, u, v).to_bits(),
                            "{width}x{height} at ({x}, {y})"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    println!("{checked} coverages bit-identical over whole stages");
}

/// A radial and a linear component in one mask, composed together, still agree bit for bit: the two
/// kinds meet only through the fold, and the fold is the frozen one.
#[test]
fn a_mixed_radial_and_linear_mask_is_bit_identical() {
    let mut rng = SplitMix64(0x4A5C_0014);
    for _ in 0..60 {
        let radial = sample_radial(&mut rng);
        let linear = luxforge_reference::mask::Linear {
            x0: rng.next_range(0.0, 1.0),
            y0: rng.next_range(0.0, 1.0),
            x1: rng.next_range(0.0, 1.0),
            y1: rng.next_range(0.0, 1.0),
        };
        if !luxforge_reference::mask::axis_is_legal(&linear, &ref_stage(64, 48)) {
            continue;
        }
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("radial");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "radial",
            payload(radial),
        ));
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Subtract,
            "linear",
            json!({"x0": linear.x0, "y0": linear.y0, "x1": linear.x1, "y1": linear.y1}),
        ));
        let reference_mask = RefMask {
            amount: 100.0,
            invert: false,
            components: vec![
                RefComponent {
                    mode: Mode::Add,
                    invert: false,
                    kind: Kind::Radial(as_reference(radial)),
                },
                RefComponent {
                    mode: Mode::Subtract,
                    invert: false,
                    kind: Kind::Linear(linear),
                },
            ],
        };
        let compiled = CompiledMask::new(
            &mask,
            stage(64, 48),
            &luxforge_core::path::StrokeTable::default(),
        )
        .unwrap();
        let reference_stage = ref_stage(64, 48);
        for y in 0..48 {
            for x in 0..64 {
                let (u, v) = reference_stage.pixel_uv(x, y);
                assert_eq!(
                    compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                    coverage(&reference_mask, Algebra::Zadeh, &reference_stage, u, v).to_bits(),
                    "at ({x}, {y})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The bounds rectangle of a rotated ellipse.
// ---------------------------------------------------------------------------

/// `bounds` is conservative: on stages small enough to evaluate exhaustively, no pixel outside the
/// rectangle has non-zero coverage, over randomized radial component lists in every mode with
/// inversions at both levels.
///
/// The test also records how often the rectangle is strictly smaller than the stage, so a change
/// that quietly returned the whole frame — which would pass the property and lose the point of it —
/// is visible as a failure rather than as a pass.
#[test]
fn radial_bounds_never_excludes_a_non_zero_pixel() {
    let mut rng = SplitMix64(0x4A5C_0015);
    let stages = [(23u32, 19u32), (19, 23), (32, 32), (48, 12)];
    let mut narrower = 0usize;
    let mut cases = 0usize;
    for components in 1..=5 {
        for _ in 0..60 {
            let (mask, _) = sample_pair(&mut rng, components);
            for (width, height) in stages {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &luxforge_core::path::StrokeTable::default(),
                )
                .unwrap();
                let bounds = compiled.bounds();
                cases += 1;
                if bounds.pixels() < u64::from(width) * u64::from(height) {
                    narrower += 1;
                }
                for y in 0..height {
                    for x in 0..width {
                        let coverage = compiled.coverage(x, y, ANY_PIXEL);
                        if coverage != 0.0 {
                            assert!(
                                bounds.contains(x, y),
                                "{width}x{height}: coverage {coverage} at ({x}, {y}) lies outside \
                                 {bounds:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    println!("{narrower} of {cases} rectangles were smaller than their stage");
    // The seed is fixed, so this floor is a recorded figure rather than a guess: 238 of 1200 at the
    // seed above. Most of the rest legitimately bound to the whole stage — half the sampled masks
    // are whole-mask inverted, half of every component list's components are inverted, and one
    // sampled radius in eight is between 2 and 12 mask-space units, which covers any of these small
    // stages outright. A change that returned the whole stage unconditionally would pass the
    // property above and lose the point of it; it fails here instead, and the angle sweep below
    // holds every one of its rectangles to being strictly smaller. A tighter rectangle can only
    // raise the count.
    assert!(
        narrower >= 200,
        "only {narrower} of {cases} rectangles were narrower than the whole stage, against a \
         recorded 238; the property above would be holding vacuously"
    );
}

/// The same property swept over the rotation alone, at every degree of a full turn, on a single
/// add component with no inversion — so the rectangle is the ellipse's own box and the angle is the
/// only thing varying. Every rectangle here is strictly smaller than its stage, which is the whole
/// point of the inside-selected default.
#[test]
fn the_rotated_ellipse_box_is_conservative_at_every_angle() {
    let stages = [(40u32, 30u32), (30, 40), (36, 36)];
    let mut narrower = 0usize;
    let mut cases = 0usize;
    for degrees in -180..=180 {
        for feather in [0.0, 1e-16, 12.5, 50.0, 100.0] {
            let radial = RadialGradient {
                x: 0.5,
                y: 0.5,
                radius_x: 0.34,
                radius_y: 0.13,
                angle: f64::from(degrees),
                feather,
            };
            for (width, height) in stages {
                let compiled = compile(radial, width, height);
                let bounds = compiled.bounds();
                cases += 1;
                if bounds.pixels() < u64::from(width) * u64::from(height) {
                    narrower += 1;
                }
                for y in 0..height {
                    for x in 0..width {
                        let coverage = compiled.coverage(x, y, ANY_PIXEL);
                        if coverage != 0.0 {
                            assert!(
                                bounds.contains(x, y),
                                "{degrees} deg, feather {feather}, {width}x{height}: coverage \
                                 {coverage} at ({x}, {y}) lies outside {bounds:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    println!("{narrower} of {cases} rotated-ellipse rectangles were smaller than their stage");
    assert_eq!(
        narrower, cases,
        "an inside-selected radial that fits inside the frame must never bound to the whole stage"
    );
}

/// An inverted radial is non-zero over the whole of the stage outside its inner ellipse, so its
/// rectangle is the whole stage. The complement of an ellipse is not a rectangle, and the test says
/// so by finding a non-zero pixel in every corner.
#[test]
fn an_inverted_radial_bounds_to_the_whole_stage() {
    let mut mask = single(RadialGradient {
        x: 0.5,
        y: 0.5,
        radius_x: 0.2,
        radius_y: 0.2,
        angle: 0.0,
        feather: 50.0,
    });
    mask.components[0].invert = true;
    let compiled = CompiledMask::new(
        &mask,
        stage(40, 30),
        &luxforge_core::path::StrokeTable::default(),
    )
    .unwrap();
    assert_eq!(compiled.bounds().x0, 0);
    assert_eq!(compiled.bounds().y0, 0);
    assert_eq!(compiled.bounds().width, 40);
    assert_eq!(compiled.bounds().height, 30);
    for (x, y) in [(0u32, 0u32), (39, 0), (0, 29), (39, 29)] {
        assert_eq!(compiled.coverage(x, y, ANY_PIXEL), 1.0, "at ({x}, {y})");
    }
    // The inner ellipse is where the inversion is exactly zero.
    assert_eq!(compiled.coverage(20, 15, ANY_PIXEL), 0.0);
}

/// A radial centred entirely off the frame bounds to nothing, and every pixel of the stage is
/// exactly zero: an ellipse dragged off the canvas selects nothing here.
#[test]
fn a_radial_entirely_off_the_frame_bounds_to_nothing() {
    let compiled = compile(
        RadialGradient {
            x: -0.9,
            y: -0.9,
            radius_x: 0.1,
            radius_y: 0.1,
            angle: 30.0,
            feather: 40.0,
        },
        40,
        30,
    );
    assert!(compiled.bounds().is_empty());
    for y in 0..30 {
        for x in 0..40 {
            assert_eq!(compiled.coverage(x, y, ANY_PIXEL), 0.0, "at ({x}, {y})");
        }
    }
}

// ---------------------------------------------------------------------------
// The cases the study names.
// ---------------------------------------------------------------------------

/// `feather = 0` is the explicit hard edge: every coverage is exactly `0.0` or exactly `1.0`, and
/// the same holds for the second route to that branch, a feather so small that `1 - feather / 100`
/// rounds to exactly `1.0`. A representable small feather is the smooth branch, checked here too so
/// the boundary is not asserted from one side only.
#[test]
fn feather_zero_is_a_hard_edge_by_both_routes() {
    for feather in [0.0, 1e-16, 1e-18] {
        let compiled = compile(
            RadialGradient {
                x: 0.5,
                y: 0.5,
                radius_x: 0.3,
                radius_y: 0.2,
                angle: 20.0,
                feather,
            },
            120,
            90,
        );
        let mut ones = 0usize;
        for y in 0..90 {
            for x in 0..120 {
                let coverage = compiled.coverage(x, y, ANY_PIXEL);
                assert!(
                    coverage == 0.0 || coverage == 1.0,
                    "feather {feather} at ({x}, {y}) gave {coverage}, which is neither end"
                );
                if coverage == 1.0 {
                    ones += 1;
                }
            }
        }
        assert!(ones > 0, "feather {feather} selected nothing at all");
        // A hard edge has no ramp for a pixel grid to resolve, and says so.
        assert_eq!(compiled.min_feature_px(stage(120, 90)), 0.0);
    }
    // A representable small feather is the smooth branch, not the hard one. `0.01` leaves a ramp
    // `0.0001 · 0.2 = 2e-5` mask-space units wide, which is 0.018 px on this stage — too narrow for
    // a pixel centre to land in, so the branch is read off the feature width it reports rather than
    // off a sampled partial value, and the width is non-zero precisely because the span is.
    let narrow = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.3,
            radius_y: 0.2,
            angle: 20.0,
            feather: 0.01,
        },
        1200,
        900,
    );
    assert!(narrow.min_feature_px(stage(1200, 900)) > 0.0);
    // A feather whose ramp is wide enough to sample shows the smooth branch directly: pixels land
    // strictly between the two ends.
    let compiled = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.3,
            radius_y: 0.3,
            angle: 20.0,
            feather: 2.0,
        },
        1200,
        900,
    );
    let partial = (0..1200)
        .map(|x| compiled.coverage(x, 450, ANY_PIXEL))
        .filter(|c| *c > 0.0 && *c < 1.0)
        .count();
    assert!(partial > 0, "a feather of 2 produced no ramp at all");
}

/// `feather = 100` starts the ramp at the centre: coverage is exactly `1.0` at the centre pixel and
/// falls monotonically to exactly `0.0` at the boundary, with no plateau between them.
#[test]
fn feather_one_hundred_ramps_from_the_centre() {
    // An odd-sided stage so one pixel centre is the ellipse centre exactly.
    let compiled = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.25,
            radius_y: 0.25,
            angle: 0.0,
            feather: 100.0,
        },
        401,
        401,
    );
    assert_eq!(compiled.coverage(200, 200, ANY_PIXEL), 1.0);
    // Nonincreasing outward along the row, and nothing but the centre is at full coverage.
    let mut previous = 1.0;
    let mut plateau = 0usize;
    for x in 200..401 {
        let coverage = compiled.coverage(x, 200, ANY_PIXEL);
        assert!(coverage <= previous, "coverage rose outward at x = {x}");
        if coverage == 1.0 {
            plateau += 1;
        }
        previous = coverage;
    }
    assert_eq!(plateau, 1, "feather 100 left a plateau of {plateau} pixels");
    // Past the boundary, 0.25 units is 100.25 px, coverage is exactly zero.
    assert_eq!(compiled.coverage(320, 200, ANY_PIXEL), 0.0);
}

/// Inside is selected, and the component's own `invert` is the exact complement of it: the two sum
/// to exactly `1.0` at every pixel, and the inverted value is bit-for-bit `1 - c`.
#[test]
fn invert_is_the_exact_complement_and_inside_is_selected() {
    let mut rng = SplitMix64(0x4A5C_0016);
    for _ in 0..40 {
        let radial = sample_radial(&mut rng);
        let drawn = compile(radial, 61, 47);
        let mut inverted_mask = single(radial);
        inverted_mask.components[0].invert = true;
        let inverted = CompiledMask::new(
            &inverted_mask,
            stage(61, 47),
            &luxforge_core::path::StrokeTable::default(),
        )
        .unwrap();
        for y in 0..47 {
            for x in 0..61 {
                let c = drawn.coverage(x, y, ANY_PIXEL);
                let i = inverted.coverage(x, y, ANY_PIXEL);
                assert_eq!(i.to_bits(), (1.0 - c).to_bits(), "at ({x}, {y})");
                assert_eq!(c + i, 1.0, "at ({x}, {y}): {c} + {i}");
            }
        }
    }
    // Inside is selected: full coverage at the centre and none well outside, which is the opposite
    // of Lightroom's default and is proposal P3 as the study confirmed it.
    let compiled = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.15,
            radius_y: 0.15,
            angle: 0.0,
            feather: 50.0,
        },
        101,
        101,
    );
    assert_eq!(compiled.coverage(50, 50, ANY_PIXEL), 1.0);
    assert_eq!(compiled.coverage(0, 0, ANY_PIXEL), 0.0);
    assert_eq!(compiled.coverage(100, 100, ANY_PIXEL), 0.0);
}

/// The ellipse is defined in mask space, so a circle is a circle in pixels at any aspect ratio: the
/// same stored payload on a frame and its transpose covers the same number of pixels across as
/// down, counted rather than argued.
#[test]
fn a_circle_is_a_circle_at_every_aspect_ratio() {
    let radial = RadialGradient {
        x: 0.5,
        y: 0.5,
        radius_x: 0.2,
        radius_y: 0.2,
        angle: 0.0,
        feather: 0.0,
    };
    for (width, height) in [(601u32, 401u32), (401, 601), (401, 401)] {
        let compiled = compile(radial, width, height);
        let cx = width / 2;
        let cy = height / 2;
        assert_eq!(compiled.coverage(cx, cy, ANY_PIXEL), 1.0);
        let across = (cx..width)
            .take_while(|x| compiled.coverage(*x, cy, ANY_PIXEL) > 0.0)
            .count();
        let down = (cy..height)
            .take_while(|y| compiled.coverage(cx, *y, ANY_PIXEL) > 0.0)
            .count();
        // 0.2 mask-space units is 0.2 · H pixels on both axes, whatever the width is.
        let nominal = 0.2 * f64::from(height);
        assert_eq!(
            across, down,
            "{width}x{height}: {across} across, {down} down"
        );
        assert!(
            (across as f64 - nominal).abs() <= 1.0,
            "{width}x{height}: {across} px against a nominal {nominal}"
        );
    }
}

/// `min_feature_px` is the ramp's width in the stage's pixels — the narrower of the two axes — and
/// it scales with the stage it is asked about, because a radius stored as a fraction of the frame's
/// height is that many fewer pixels at proxy size.
#[test]
fn min_feature_px_is_the_measured_ramp_width() {
    let full = stage(6000, 4000);
    let compiled = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.4,
            radius_y: 0.2,
            angle: 0.0,
            feather: 50.0,
        },
        6000,
        4000,
    );
    // span = 0.5, the narrower radius is 0.2, so the ramp is 0.1 units — 400 px at 4000 of height.
    assert_eq!(compiled.min_feature_px(full), 400.0);
    assert_eq!(compiled.min_feature_px(stage(360, 240)), 24.0);
    // Below two pixels is what the proxy path treats as aliasing; at 16 px of height it is 1.6.
    assert!(compiled.min_feature_px(stage(24, 16)) < 2.0);
    // Counted down the narrow axis of a square stage: the ramp occupies the stated number of rows.
    let counted = compile(
        RadialGradient {
            x: 0.5,
            y: 0.5,
            radius_x: 0.4,
            radius_y: 0.2,
            angle: 0.0,
            feather: 50.0,
        },
        1000,
        1000,
    );
    let rows = (0..1000)
        .filter(|y| {
            let c = counted.coverage(500, *y, ANY_PIXEL);
            c > 0.0 && c < 1.0
        })
        .count();
    // 0.1 units is 100 px, on each side of the centre.
    assert!(
        (198..=202).contains(&rows),
        "the ramp occupied {rows} rows against a stated 2 x 100"
    );
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn refusal(payload: serde_json::Value) -> (luxforge_core::ErrorKind, String) {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        payload,
    ));
    let error = CompiledMask::new(
        &mask,
        stage(400, 400),
        &luxforge_core::path::StrokeTable::default(),
    )
    .unwrap_err();
    (error.kind, error.to_string())
}

fn legal() -> serde_json::Value {
    json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0, "feather": 50.0})
}

fn with(field: &str, value: serde_json::Value) -> serde_json::Value {
    let mut payload = legal();
    payload[field] = value;
    payload
}

/// Every message a malformed or illegal radial payload produces, in full.
#[test]
fn radial_validation_errors_name_the_field() {
    let cases = [
        (
            with("x", json!(-1.5)),
            "validation: component Radial 1 radial x must be a number within -1..=2",
        ),
        (
            with("y", json!(2.5)),
            "validation: component Radial 1 radial y must be a number within -1..=2",
        ),
        (
            with("radius_x", json!(0.0)),
            "validation: component Radial 1 radial radius_x must be a number within 1e-4..=64 \
             mask-space units",
        ),
        (
            with("radius_y", json!(5e-5)),
            "validation: component Radial 1 radial radius_y must be a number within 1e-4..=64 \
             mask-space units",
        ),
        (
            with("radius_x", json!(64.5)),
            "validation: component Radial 1 radial radius_x must be a number within 1e-4..=64 \
             mask-space units",
        ),
        (
            with("radius_y", json!(-0.3)),
            "validation: component Radial 1 radial radius_y must be a number within 1e-4..=64 \
             mask-space units",
        ),
        (
            with("angle", json!(181.0)),
            "validation: component Radial 1 radial angle must be a number within -180..=180 degrees",
        ),
        (
            with("angle", json!(-180.5)),
            "validation: component Radial 1 radial angle must be a number within -180..=180 degrees",
        ),
        (
            with("feather", json!(100.5)),
            "validation: component Radial 1 radial feather must be a number within 0..=100",
        ),
        (
            with("feather", json!(-1.0)),
            "validation: component Radial 1 radial feather must be a number within 0..=100",
        ),
        (
            json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0}),
            "validation: component Radial 1 has an invalid radial payload: missing field `feather`",
        ),
        (
            json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
            "validation: component Radial 1 has an invalid radial payload: unknown field `x0`, \
             expected one of `x`, `y`, `radius_x`, `radius_y`, `angle`, `feather`",
        ),
    ];
    for (payload, message) in cases {
        let (kind, actual) = refusal(payload);
        assert_eq!(kind, luxforge_core::ErrorKind::Validation);
        assert_eq!(actual, message);
    }
    // Both ends of the distance range are legal, and so are both ends of the angle and the feather.
    for payload in [
        with("radius_x", json!(1e-4)),
        with("radius_y", json!(64.0)),
        with("angle", json!(-180.0)),
        with("angle", json!(180.0)),
        with("feather", json!(0.0)),
        with("feather", json!(100.0)),
        with("x", json!(-1.0)),
        with("y", json!(2.0)),
    ] {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(Component::new(
            "Radial 1",
            ComponentMode::Add,
            "radial",
            payload.clone(),
        ));
        CompiledMask::new(
            &mask,
            stage(400, 400),
            &luxforge_core::path::StrokeTable::default(),
        )
        .unwrap_or_else(|error| panic!("{payload} was refused: {error}"));
    }
}

/// A non-finite field cannot be written as JSON at all — `serde_json` stores an infinity or a NaN as
/// `null` — so it is refused by the payload parse naming the field, and the `is_finite` guard behind
/// that is the second door rather than the first.
#[test]
fn a_non_finite_field_is_refused_by_name() {
    for field in ["x", "radius_x", "angle", "feather"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let (kind, message) = refusal(with(field, json!(value)));
            assert_eq!(kind, luxforge_core::ErrorKind::Validation);
            assert_eq!(
                message,
                format!(
                    "validation: component Radial 1 has an invalid radial payload: invalid type: \
                     null, expected f64"
                ),
                "{field} = {value}"
            );
        }
    }
    // The other route a non-finite field could take is a JSON literal too large for an `f64`. That
    // spelling never reaches the payload at all: the document itself fails to parse, so a stored
    // component could not have carried it.
    let text = r#"{"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0,
                   "feather": 1e400}"#;
    assert!(
        serde_json::from_str::<serde_json::Value>(text).is_err(),
        "a JSON number outside f64 was accepted, so the parse is not the first door after all"
    );
    // And the `is_finite` guard behind both doors is live rather than decorative: it is what refuses
    // a value the payload parse would have accepted, which is every finite number out of range.
    let (kind, message) = refusal(with("radius_x", json!(1e300)));
    assert_eq!(kind, luxforge_core::ErrorKind::Validation);
    assert!(
        message.contains("radius_x must be a number within"),
        "{message}"
    );
}

/// A radial whose payload is legal but whose coverage is asked for far outside the frame still
/// answers a finite number in `[0, 1]`: the field is total on finite inputs, which is what lets the
/// stored position range be a validation rule and nothing more.
#[test]
fn legal_payloads_never_produce_non_finite_coverage() {
    let mut rng = SplitMix64(0x4A5C_0017);
    for _ in 0..200 {
        let radial = sample_radial(&mut rng);
        let compiled = compile(radial, 6000, 4000);
        for _ in 0..40 {
            let x = rng.next_usize(6000) as u32;
            let y = rng.next_usize(4000) as u32;
            let coverage = compiled.coverage(x, y, ANY_PIXEL);
            assert!(
                coverage.is_finite() && (0.0..=1.0).contains(&coverage),
                "{radial:?} at ({x}, {y}) gave {coverage}"
            );
        }
    }
}

/// The kind table claims `radial`, and a stored radial round-trips byte for byte, which is what the
/// retention rule means for a kind this build *does* know.
#[test]
fn the_kind_table_claims_radial() {
    assert!(luxforge_core::mask::knows_component_kind("radial"));
    let mask = single(RadialGradient {
        x: 0.25,
        y: 0.75,
        radius_x: 0.3,
        radius_y: 0.2,
        angle: -12.5,
        feather: 33.0,
    });
    mask.validate().unwrap();
    assert_eq!(mask.components[0].name, "Radial 1");
    let encoded = serde_json::to_vec(&mask).unwrap();
    let reopened: Mask = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(reopened, mask);
    luxforge_core::mask::validate_component_kinds(&mask).unwrap();
}

/// The production falloff against the reference's own `radial_coverage`, one component at a time
/// and with no composition in the way, so a transcription failure is attributed to the falloff
/// rather than to the fold.
#[test]
fn the_falloff_alone_is_bit_identical_to_the_reference() {
    let mut rng = SplitMix64(0x4A5C_0018);
    for _ in 0..200 {
        let radial = sample_radial(&mut rng);
        for (width, height) in STAGES {
            let compiled = compile(radial, width, height);
            let reference_stage = ref_stage(width, height);
            for _ in 0..40 {
                let x = rng.next_usize(width as usize) as u32;
                let y = rng.next_usize(height as usize) as u32;
                let (u, v) = reference_stage.pixel_uv(x, y);
                let expected = radial_coverage(&as_reference(radial), &reference_stage, u, v);
                assert_eq!(
                    compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                    expected.to_bits(),
                    "{radial:?} on {width}x{height} at ({x}, {y})"
                );
            }
        }
    }
}
