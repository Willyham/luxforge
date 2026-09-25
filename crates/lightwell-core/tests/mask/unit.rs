//! TASK-004: the production compiled mask against the frozen `f64` reference.
//!
//! `crates/lightwell-reference/src/mask.rs` shares no code with `lightwell-core`'s sources, and
//! `docs/design/mask-study.md` freezes the mathematics both write. The bar here is **bit-identity**
//! rather than a tolerance: the production unit transcribes the reference's expressions in the
//! reference's order, so the same payloads, stage and pixel must produce the same `f64` bits. A
//! failure of these tests is a rewritten expression, not float noise — the study's Transcription
//! section lists what a rewrite costs, and each item in it is within tolerance and not bit-identical.
//!
//! The `bounds` rectangle and `min_feature_px` are not frozen by the study, so they are verified
//! here by exhaustive evaluation on small stages rather than against an oracle.

use super::*;
use lightwell_core::{
    Component, ComponentMode, Mask,
    mask::{CompiledMask, LinearGradient},
};
use lightwell_reference::mask::{
    Algebra, Component as RefComponent, Kind, Linear, Mask as RefMask, Mode, axis_is_legal,
    coverage,
};
use serde_json::json;

const ANY_PIXEL_F32: [f32; 3] = [0.25, 0.5, 0.75];

/// A stored position anywhere in the legal `[-1, 2]` range, so the widened range is swept rather
/// than assumed.
fn sample_gradient(rng: &mut SplitMix64, stages: &[(u32, u32)]) -> LinearGradient {
    loop {
        let gradient = LinearGradient {
            x0: rng.next_range(-1.0, 2.0),
            y0: rng.next_range(-1.0, 2.0),
            x1: rng.next_range(-1.0, 2.0),
            y1: rng.next_range(-1.0, 2.0),
        };
        if stages.iter().all(|&(width, height)| {
            axis_is_legal(&as_reference(gradient), &ref_stage(width, height))
        }) {
            return gradient;
        }
    }
}

fn as_reference(gradient: LinearGradient) -> Linear {
    Linear {
        x0: gradient.x0,
        y0: gradient.y0,
        x1: gradient.x1,
        y1: gradient.y1,
    }
}

fn payload(gradient: LinearGradient) -> serde_json::Value {
    json!({"x0": gradient.x0, "y0": gradient.y0, "x1": gradient.x1, "y1": gradient.y1})
}

/// One randomized mask in both spellings: the stored model the host compiles, and the reference's
/// own structure. The first component is always `add`, which the model validates structurally.
fn sample_pair(rng: &mut SplitMix64, components: usize, stages: &[(u32, u32)]) -> (Mask, RefMask) {
    let mut mask = Mask::new("Mask 1");
    let mut reference = RefMask {
        amount: 100.0,
        invert: false,
        components: Vec::new(),
    };
    mask.amount = rng.next_range(0.0, 100.0);
    reference.amount = mask.amount;
    mask.invert = rng.next_bool();
    reference.invert = mask.invert;
    for index in 0..components {
        let (mode, ref_mode) = if index == 0 {
            (ComponentMode::Add, Mode::Add)
        } else {
            mode_of(rng.next_usize(3))
        };
        let gradient = sample_gradient(rng, stages);
        let invert = rng.next_bool();
        let name = mask.next_component_name("linear");
        let mut component = Component::new(name, mode, "linear", payload(gradient));
        component.invert = invert;
        mask.components.push(component);
        reference.components.push(RefComponent {
            mode: ref_mode,
            invert,
            kind: Kind::Linear(as_reference(gradient)),
        });
    }
    (mask, reference)
}

/// The production field against the frozen reference, bit for bit, over randomized masks, stages and
/// pixels. `to_bits` rather than `==` so a `-0.0` or a NaN could not pass as equal, and the assertion
/// prints both patterns when it fails.
#[test]
fn the_compiled_mask_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x4A5C_0004);
    let mut checked = 0usize;
    for components in 1..=6 {
        for round in 0..40 {
            let (mask, reference) = sample_pair(&mut rng, components, &STAGES);
            mask.validate()
                .expect("the sampled mask is structurally valid");
            for (width, height) in STAGES {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &lightwell_core::path::StrokeTable::default(),
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
    println!("{checked} coverages bit-identical to the reference");
}

/// The same comparison walked densely down one column and across one row of a small stage, so the
/// clamp's two ends and the exactly-zero and exactly-one plateaus are swept rather than sampled.
#[test]
fn bit_identity_holds_across_whole_rows_and_columns() {
    let mut rng = SplitMix64(0x4A5C_0005);
    let stages = [(31u32, 17u32), (17, 31), (64, 64)];
    for components in 1..=4 {
        for _ in 0..20 {
            let (mask, reference) = sample_pair(&mut rng, components, &stages);
            for (width, height) in stages {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &lightwell_core::path::StrokeTable::default(),
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
                    }
                }
            }
        }
    }
}

/// `evaluate` is the `f64` field narrowed once at the end, not an `f32` composition: it equals
/// `coverage(...) as f32` at every pixel, and it stays inside `[0, 1]`.
#[test]
fn evaluate_is_the_narrowed_field_and_stays_in_range() {
    let mut rng = SplitMix64(0x4A5C_0006);
    let stages = [(41u32, 29u32)];
    for components in 1..=5 {
        for _ in 0..20 {
            let (mask, _) = sample_pair(&mut rng, components, &stages);
            let compiled = CompiledMask::new(
                &mask,
                stage(41, 29),
                &lightwell_core::path::StrokeTable::default(),
            )
            .unwrap();
            for y in 0..29 {
                for x in 0..41 {
                    let coverage = compiled.coverage(x, y, ANY_PIXEL);
                    assert_eq!(
                        compiled.evaluate(x, y, ANY_PIXEL_F32).to_bits(),
                        (coverage as f32).to_bits()
                    );
                    assert!((0.0..=1.0).contains(&coverage), "{coverage} at ({x}, {y})");
                }
            }
        }
    }
}

/// `bounds` is conservative: on stages small enough to evaluate exhaustively, no pixel outside the
/// rectangle has non-zero coverage, over randomized component lists in every mode with inversions at
/// both levels.
///
/// The test also records how often the rectangle is smaller than the stage, so a future change that
/// quietly returns the whole frame — which would pass the property and lose the point of it — is
/// visible as a failure rather than as a pass.
#[test]
fn bounds_never_excludes_a_non_zero_pixel() {
    let mut rng = SplitMix64(0x4A5C_0007);
    let stages = [(23u32, 19u32), (19, 23), (32, 32), (48, 12)];
    let mut narrower = 0usize;
    let mut cases = 0usize;
    for components in 1..=5 {
        for _ in 0..60 {
            let (mask, _) = sample_pair(&mut rng, components, &stages);
            for (width, height) in stages {
                let compiled = CompiledMask::new(
                    &mask,
                    stage(width, height),
                    &lightwell_core::path::StrokeTable::default(),
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
    // The seed is fixed, so this floor is a recorded figure rather than a guess: 147 of 1200 at the
    // seed above. Half the sampled masks are inverted, and a stored axis anywhere in `[-1, 2]` often
    // puts the whole frame in front of `p0`, so most cases legitimately bound to the whole stage. A
    // change that returned the whole stage unconditionally would pass the property above and lose the
    // point of it; it fails here instead. A tighter rectangle can only raise the count.
    assert!(
        narrower >= 120,
        "only {narrower} of {cases} rectangles were narrower than the whole stage, against a \
         recorded 147; the property above would be holding vacuously"
    );
}

/// A mask whose components all lie behind the frame still answers, and its rectangle is empty rather
/// than the whole stage: a gradient dragged entirely off the canvas selects nothing here.
#[test]
fn a_gradient_entirely_off_the_frame_bounds_to_nothing() {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        // p0 and p1 both above the frame, pointing further up: every pixel is behind p0.
        json!({"x0": 0.5, "y0": -0.4, "x1": 0.5, "y1": -0.9}),
    ));
    let compiled = CompiledMask::new(
        &mask,
        stage(40, 30),
        &lightwell_core::path::StrokeTable::default(),
    )
    .unwrap();
    assert!(compiled.bounds().is_empty());
    for y in 0..30 {
        for x in 0..40 {
            assert_eq!(compiled.coverage(x, y, ANY_PIXEL), 0.0, "at ({x}, {y})");
        }
    }
}

/// `min_feature_px` scales with the stage, because a ramp stored as a fraction of the frame is that
/// many fewer pixels at proxy size. It is the question the proxy path asks before it compiles.
#[test]
fn min_feature_px_scales_with_the_stage_it_is_asked_about() {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.5, "y0": 0.4, "x1": 0.5, "y1": 0.45}),
    ));
    let full = stage(6000, 4000);
    let compiled =
        CompiledMask::new(&mask, full, &lightwell_core::path::StrokeTable::default()).unwrap();
    // 0.05 mask-space units is 200 px at 4000 px of height and 12 px at 240.
    assert_eq!(compiled.min_feature_px(full), 200.0);
    assert_eq!(compiled.min_feature_px(stage(360, 240)), 12.0);
    // Below two pixels is what the proxy path treats as aliasing; at 24 px of height it is 1.2.
    assert!(compiled.min_feature_px(stage(36, 24)) < 2.0);
}

/// A component of a kind this build does not know is refused by name, and the stored mask reads back
/// with its bytes intact. Which evaluation paths that refusal reaches is asserted beside the rule
/// itself, in the module registry's own tests.
#[test]
fn an_unknown_kind_is_refused_by_name_and_still_reads_back() {
    let mut mask = Mask::new("Mask 1");
    // Every kind the masking design names is delivered, so the kind this build does not claim is one
    // no design names — which is exactly the case retention exists for.
    let name = mask.next_component_name("depth-range");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "depth-range",
        json!({"low": 0.2, "low_feather": 10.0, "high": 0.8, "high_feather": 10.0}),
    ));
    let error = CompiledMask::new(
        &mask,
        stage(64, 48),
        &lightwell_core::path::StrokeTable::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "incompatible: unknown mask component depth-range"
    );
    // The stored mask is structurally valid — the model never asks what a kind means — and survives
    // a round trip byte for byte, which is what retention means.
    mask.validate().unwrap();
    let encoded = serde_json::to_vec(&mask).unwrap();
    let reopened: Mask = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(reopened, mask);
    assert_eq!(
        serde_json::to_string(&reopened.components[0].payload).unwrap(),
        serde_json::to_string(&mask.components[0].payload).unwrap()
    );
}
