//! TASK-023: the production luminance-range and colour-range components against the frozen `f64`
//! reference, and the contract change that made a value-based component possible at all.
//!
//! `tests/reference/range.rs` shares no code with `luxforge-core`'s sources, and
//! `docs/design/range-study.md` freezes the mathematics both write. The bar is **bit-identity**
//! rather than the study's tolerance, and it is reachable because production widens the incoming
//! `[f32; 3]` once and then evaluates the reference's own `f64` expressions in the reference's own
//! order: the study derived a coverage tolerance from the shoulder's Lipschitz constant precisely to
//! cover an `f32` transcription, and this one removes that gap instead of living inside it. A
//! failure here is a rewritten expression, not float noise.
//!
//! What else is proved here, each named by the thing it protects:
//!
//! * **P12's condition.** `CompiledMask::evaluate` takes the pixel now. A geometric component must
//!   ignore it *exactly*, not approximately, because the frozen mask study compares those
//!   components bit for bit; `a_geometric_component_ignores_the_pixel_it_is_handed` sweeps that
//!   over every kind and every pixel of a stage.
//! * **The sample equals the render.** A range selection reads the input of the operation it
//!   modulates, on the byte path and the RAW linear path, and `render.sample` passes the same input
//!   pixel the rasterizing pass does — proved across a feathered boundary where a wrong pixel would
//!   show as a wrong byte.
//! * **The composition.** A range component combined with a gradient and a subtract brush renders
//!   exactly as the frozen algebra composes them, which is what makes a sky selection practical.
//! * **P13's answer.** The conservative rectangle is the whole stage, the smallest feature is
//!   infinite, and a range mask is therefore never supersampled and never reported approximate.

mod reference;

use luxforge_core::{
    AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EditorService, Layer, LayerId,
    Mask, ModuleRegistry, Mutation, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, Stage,
    mask::{
        CompiledMask,
        commands::{self, MaskTarget},
    },
    path::{Stroke, StrokeTable},
    render, render_linear, sample, sample_linear,
};
use luxforge_core::{LinearImage, LinearSettings};
use reference::mask::{
    Brush as RefBrush, BrushStroke, Linear, Stage as RefStage, brush_coverage, linear_coverage,
};
use reference::range::{
    ColourRange as RefColourRange, LuminanceRange as RefLuminanceRange, colour_coverage,
    compile_colour_range, compile_luminance_range, luminance_coverage,
};
use reference::{code_threshold, linear_to_srgb_code, srgb_to_linear};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// One component's reference coverage at one linear pixel, boxed so a sweep can hold whichever of
/// the two kinds it drew this round.
type Oracle = Box<dyn Fn([f64; 3]) -> f64>;

const WIDTH: u32 = 24;
const HEIGHT: u32 = 16;

/// The exposure the masked layer applies, in EV. Large enough that a coverage difference of one part
/// in a thousand is visible in the output codes rather than lost in the quantizer.
const MASKED_EV: f64 = 2.0;

/// SplitMix64, the dependency-free generator every study's figures use, so each sweep below is
/// reproducible on any machine from its stated seed.
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

    fn next_usize(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

fn no_strokes() -> StrokeTable {
    StrokeTable::default()
}

/// One mask holding a single `add` component of `kind` over `payload`.
fn one_component(kind: &str, payload: Value) -> Mask {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name(kind);
    mask.components
        .push(Component::new(name, ComponentMode::Add, kind, payload));
    mask
}

fn luminance_payload(band: &RefLuminanceRange) -> Value {
    json!({
        "low": band.low,
        "low_feather": band.low_feather,
        "high": band.high,
        "high_feather": band.high_feather,
    })
}

fn colour_payload(range: &RefColourRange) -> Value {
    json!({ "samples": range.samples, "refine": range.refine })
}

/// A randomized band inside every legality rule the study states: ordered edges, and a shoulder that
/// is either exactly zero or at least the floor.
fn sample_band(rng: &mut SplitMix64) -> RefLuminanceRange {
    let a = rng.next_range(0.0, 100.0);
    let b = rng.next_range(0.0, 100.0);
    let shoulder = |rng: &mut SplitMix64| {
        if rng.next_usize(5) == 0 {
            0.0
        } else {
            rng.next_range(1.0, 100.0)
        }
    };
    RefLuminanceRange {
        low: a.min(b),
        low_feather: shoulder(rng),
        high: a.max(b),
        high_feather: shoulder(rng),
    }
}

/// A randomized colour range: up to the declared five samples, drawn from a generous linear range so
/// out-of-gamut and negative components — which an earlier unit can legitimately produce — are part
/// of the sweep rather than excluded from it.
fn sample_colours(rng: &mut SplitMix64) -> RefColourRange {
    let count = rng.next_usize(6);
    RefColourRange {
        samples: (0..count)
            .map(|_| {
                [
                    rng.next_range(-0.2, 1.4),
                    rng.next_range(-0.2, 1.4),
                    rng.next_range(-0.2, 1.4),
                ]
            })
            .collect(),
        refine: rng.next_range(0.0, 100.0),
    }
}

/// A randomized linear pixel, including values outside `[0, 1]`: the axis is deliberately unclamped
/// and the Oklab conversion deliberately signed, so the sweep has to reach both.
fn sample_pixel(rng: &mut SplitMix64) -> [f64; 3] {
    [
        rng.next_range(-0.5, 2.5),
        rng.next_range(-0.5, 2.5),
        rng.next_range(-0.5, 2.5),
    ]
}

// ---------------------------------------------------------------------------
// Bit-identity with the frozen references
// ---------------------------------------------------------------------------

/// The production luminance band is bit-identical to the reference over randomized payloads and
/// randomized pixels, including pixels outside the gamut and shoulders at the feather floor.
///
/// The comparison is on the mask's own `coverage`, which is the whole `f64` field the render and a
/// point sample both narrow from, so it covers the component, its inversion and the composition's
/// final multiply in one.
#[test]
fn the_luminance_band_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x0023_1A7E);
    let compiled_stage = stage(8, 8);
    let mut compared = 0usize;
    let mut partial = 0usize;
    for _ in 0..600 {
        let band = sample_band(&mut rng);
        let mask = one_component("luminance-range", luminance_payload(&band));
        let compiled = CompiledMask::new(&mask, compiled_stage, &no_strokes())
            .unwrap_or_else(|error| panic!("{band:?} is legal: {error}"));
        let oracle = compile_luminance_range(&band);
        for _ in 0..40 {
            let rgb = sample_pixel(&mut rng);
            let expected = luminance_coverage(&oracle, rgb);
            // Position is irrelevant to this component, so it is varied on purpose: a production
            // unit that let the position leak into the value would fail here and nowhere else.
            for (x, y) in [(0u32, 0u32), (3, 5), (7, 7)] {
                assert_eq!(
                    compiled.coverage(x, y, rgb).to_bits(),
                    expected.to_bits(),
                    "band {band:?} at pixel {rgb:?}, position ({x}, {y})"
                );
            }
            if expected > 0.0 && expected < 1.0 {
                partial += 1;
            }
            compared += 1;
        }
    }
    assert!(
        partial > 2000,
        "only {partial} partially covered samples: the sweep is all endpoints"
    );
    println!("{compared} luminance-band pixels, {partial} of them on a shoulder, bit for bit");
}

/// The production colour range is bit-identical to the reference over randomized sample lists,
/// randomized refines and randomized pixels, with the unsampled case and the duplicate-sample case
/// both in the sweep.
#[test]
fn the_colour_range_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x00C0_10E5);
    let compiled_stage = stage(8, 8);
    let mut compared = 0usize;
    let mut partial = 0usize;
    let mut unsampled = 0usize;
    for _ in 0..600 {
        let range = sample_colours(&mut rng);
        if range.samples.is_empty() {
            unsampled += 1;
        }
        let mask = one_component("colour-range", colour_payload(&range));
        let compiled = CompiledMask::new(&mask, compiled_stage, &no_strokes())
            .unwrap_or_else(|error| panic!("{range:?} is legal: {error}"));
        let oracle = compile_colour_range(&range);
        for _ in 0..40 {
            let rgb = sample_pixel(&mut rng);
            let expected = colour_coverage(&oracle, rgb);
            for (x, y) in [(0u32, 0u32), (3, 5), (7, 7)] {
                assert_eq!(
                    compiled.coverage(x, y, rgb).to_bits(),
                    expected.to_bits(),
                    "range {range:?} at pixel {rgb:?}, position ({x}, {y})"
                );
            }
            if expected > 0.0 && expected < 1.0 {
                partial += 1;
            }
            compared += 1;
        }
    }
    assert!(unsampled > 50, "the sweep must reach the unsampled case");
    assert!(
        partial > 200,
        "only {partial} partially covered samples: the sweep is all endpoints"
    );
    println!(
        "{compared} colour-range pixels, {partial} of them on the falloff, {unsampled} unsampled \
         components, bit for bit"
    );
}

// ---------------------------------------------------------------------------
// P12: what the change costs the geometric components
// ---------------------------------------------------------------------------

/// **The condition on proposal P12.** A geometric component ignores the pixel it is handed, exactly:
/// coverage is the same `f64` bits for every pixel value, over every kind whose geometry is a
/// position, at every pixel of a stage.
///
/// This is the whole of what the signature change costs them, and it is asserted rather than
/// assumed, because the mask and vignette studies compare those components to their own references
/// bit for bit and a value leaking into a position-only falloff would break that silently.
#[test]
fn a_geometric_component_ignores_the_pixel_it_is_handed() {
    let size = stage(37, 29);
    let stroke = Stroke::capture(
        &[[0.2, 0.3], [0.6, 0.7], [0.8, 0.4]],
        0.12,
        40.0,
        80.0,
        false,
    )
    .expect("a legal stroke");
    let mut table = StrokeTable::new("the range tests");
    let address = table.insert(stroke).to_string();
    let masks = [
        (
            "linear",
            json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
            StrokeTable::default(),
        ),
        (
            "radial",
            json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.2, "angle": 30.0, "feather": 45.0}),
            StrokeTable::default(),
        ),
        ("brush", json!({"strokes": [address]}), table),
    ];
    let pixels = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.2, 0.7, 0.35],
        [-0.4, 2.3, 0.9],
        [f64::MIN_POSITIVE, 0.5, 1.7],
    ];
    let mut compared = 0usize;
    for (kind, payload, strokes) in masks {
        let mut mask = one_component(kind, payload);
        // Both inversions on, so the `1 - c` the composition performs and the whole-mask amount are
        // inside the comparison rather than beside it.
        mask.components[0].invert = true;
        mask.invert = true;
        mask.amount = 62.5;
        let compiled = CompiledMask::new(&mask, size, &strokes).expect("a legal component");
        assert!(
            !compiled.reads_pixels(),
            "{kind} is a position-only component"
        );
        for y in 0..size.height {
            for x in 0..size.width {
                let first = compiled.coverage(x, y, pixels[0]).to_bits();
                for rgb in &pixels[1..] {
                    assert_eq!(
                        compiled.coverage(x, y, *rgb).to_bits(),
                        first,
                        "{kind} at ({x}, {y}) moved when handed {rgb:?}"
                    );
                    compared += 1;
                }
                // The narrowed `f32` answers the same way, which is the call a render makes.
                let narrow = compiled.evaluate(x, y, [0.0, 0.0, 0.0]);
                assert_eq!(
                    narrow.to_bits(),
                    compiled.evaluate(x, y, [1.0, 0.25, 0.75]).to_bits()
                );
            }
        }
    }
    println!("{compared} geometric evaluations unchanged by the pixel they were handed");
}

// ---------------------------------------------------------------------------
// P13: what a value-based component answers about the frame
// ---------------------------------------------------------------------------

/// The kind table's `value_based` column and a compiled component's own `reads_pixels` are the same
/// fact asked two ways, and the two are checked against each other over **every registered kind**
/// rather than trusted to stay in step.
///
/// A client needs the question answered before anything is drawn — a panel says what a range
/// selection cannot do while its component is still empty — and only a compiled mask can answer
/// `reads_pixels`. So the kind carries the answer too, and this is what stops the two from drifting
/// when a kind is added: a new row whose column disagrees with its own compiled behaviour fails here.
///
/// The brush is the one kind where they are deliberately different, and the difference is stated
/// rather than excepted: the kind is position-based, and a *stroke* held to a colour reads the pixel,
/// which is a property of that stroke. Both halves are asserted.
#[test]
fn a_value_based_kind_is_exactly_one_that_reads_the_pixel() {
    let size = stage(WIDTH, HEIGHT);
    let payloads: Vec<(&str, Value)> = vec![
        ("linear", json!({"x0":0.1,"y0":0.2,"x1":0.8,"y1":0.9})),
        (
            "radial",
            json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.2,"angle":12.0,"feather":40.0}),
        ),
        (
            "luminance-range",
            luminance_payload(&RefLuminanceRange {
                low: 20.0,
                low_feather: 5.0,
                high: 70.0,
                high_feather: 5.0,
            }),
        ),
        (
            "colour-range",
            colour_payload(&RefColourRange {
                samples: vec![[0.12, 0.2, 0.42]],
                refine: 50.0,
            }),
        ),
    ];
    let mut checked = 0usize;
    for (kind, payload) in &payloads {
        let mask = one_component(kind, payload.clone());
        let compiled = CompiledMask::new(&mask, size, &no_strokes()).expect("a legal payload");
        assert_eq!(
            luxforge_core::mask::component_kind_is_value_based(kind),
            compiled.reads_pixels(),
            "{kind}: the kind table and the compiled component disagree about reading the pixel"
        );
        checked += 1;
    }
    // Every registered kind is covered, so a kind added without a payload here fails rather than
    // being silently unchecked. The brush is the one kind with no typed payload and is checked below.
    let registered: Vec<&str> = luxforge_core::mask::component_kinds().collect();
    for kind in &registered {
        assert!(
            *kind == luxforge_core::mask::BRUSH || payloads.iter().any(|(named, _)| named == kind),
            "{kind} is registered and this test has no payload for it"
        );
    }
    // The brush: the kind is position-based, and one stroke held to a colour is what reads the pixel.
    assert!(
        !luxforge_core::mask::component_kind_is_value_based(luxforge_core::mask::BRUSH),
        "a brush's geometry is a path, so the kind is position-based"
    );
    let mut table = StrokeTable::default();
    let plain = table.insert(
        Stroke::capture(&[[0.2, 0.3], [0.6, 0.3]], 0.1, 20.0, 100.0, false)
            .expect("a legal stroke"),
    );
    let held = table.insert(
        Stroke::capture(&[[0.2, 0.7], [0.6, 0.7]], 0.1, 20.0, 100.0, false)
            .expect("a legal stroke")
            .with_colour_limit(
                luxforge_core::path::ColourLimit::sampled(
                    [120, 140, 180],
                    luxforge_core::mask::REFINE_DEFAULT,
                )
                .expect("a legal refine"),
            ),
    );
    for (addresses, reads) in [(vec![plain.clone()], false), (vec![plain, held], true)] {
        let mask = one_component(
            luxforge_core::mask::BRUSH,
            json!({ luxforge_core::path::STROKES_FIELD: addresses }),
        );
        let compiled = CompiledMask::new(&mask, size, &table).expect("a legal brush");
        assert_eq!(compiled.reads_pixels(), reads);
        checked += 1;
    }
    println!("{checked} kinds agree with the table about reading the pixel");
}

/// The conservative rectangle of a value-based component is the **whole stage**, its smallest
/// feature is infinite, and it says so about itself.
///
/// Both answers are stated rather than guessed. The rectangle is the whole stage because a colour
/// the component selects can appear at any pixel, so no span or tile may be skipped for it; the
/// feature is infinite because a value test draws nothing a pixel grid can miss, which is also why
/// the thin-feature rule must not fire for one — supersampling cannot help when the
/// full-resolution pixels are not there to read.
#[test]
fn a_value_based_component_bounds_the_whole_stage_and_draws_no_feature() {
    let band = RefLuminanceRange {
        low: 20.0,
        low_feather: 8.0,
        high: 60.0,
        high_feather: 12.0,
    };
    let colours = RefColourRange {
        samples: vec![[0.12, 0.2, 0.42]],
        refine: 50.0,
    };
    for (kind, payload) in [
        ("luminance-range", luminance_payload(&band)),
        ("colour-range", colour_payload(&colours)),
    ] {
        for (width, height) in [(37u32, 29u32), (640, 480), (6000, 4000)] {
            let size = stage(width, height);
            let mask = one_component(kind, payload.clone());
            let compiled = CompiledMask::new(&mask, size, &no_strokes()).expect("a legal payload");
            assert!(compiled.reads_pixels(), "{kind} reads the pixel");
            let bounds = compiled.bounds();
            assert_eq!(
                (bounds.x0, bounds.y0, bounds.width, bounds.height),
                (0, 0, width, height),
                "{kind} on a {width}x{height} stage"
            );
            assert_eq!(compiled.min_feature_px(size), f32::INFINITY, "{kind}");
            // And at proxy size, which is the stage the thin-feature rule is asked about.
            assert_eq!(
                compiled.min_feature_px(stage(width.div_ceil(4).max(1), height.div_ceil(4).max(1))),
                f32::INFINITY,
                "{kind} at proxy size"
            );
        }
    }
}

/// A whole-mask inversion or an amount of exactly zero still decide the rectangle, because those are
/// properties of the composition and not of any component: zero is empty whatever the components
/// read, and an inversion is the whole stage, which a range component's rectangle already was.
#[test]
fn the_whole_mask_amount_still_decides_the_rectangle() {
    let colours = RefColourRange {
        samples: vec![[0.3, 0.4, 0.5]],
        refine: 70.0,
    };
    let mut mask = one_component("colour-range", colour_payload(&colours));
    mask.amount = 0.0;
    let compiled = CompiledMask::new(&mask, stage(64, 48), &no_strokes()).unwrap();
    assert!(compiled.bounds().is_empty());
    assert_eq!(compiled.coverage(10, 10, [0.3, 0.4, 0.5]), 0.0);
}

// ---------------------------------------------------------------------------
// Payload legality
// ---------------------------------------------------------------------------

/// Every refusal names the component and the field, and the shoulder floor's gap is refused rather
/// than rounded into the hard branch.
#[test]
fn an_illegal_range_payload_is_refused_by_name() {
    let cases: [(&str, Value, &str); 6] = [
        (
            "luminance-range",
            json!({"low": 70.0, "low_feather": 0.0, "high": 30.0, "high_feather": 0.0}),
            "validation: component Luminance range 1 luminance-range low must not be above high, \
             and 30 is",
        ),
        (
            "luminance-range",
            json!({"low": 10.0, "low_feather": 0.5, "high": 30.0, "high_feather": 0.0}),
            "validation: component Luminance range 1 luminance-range low_feather must be exactly 0 \
             for a hard edge or a number within 1..=100",
        ),
        (
            "luminance-range",
            json!({"low": -1.0, "low_feather": 0.0, "high": 30.0, "high_feather": 0.0}),
            "validation: component Luminance range 1 luminance-range low must be a number within \
             0..=100",
        ),
        (
            "luminance-range",
            json!({"low": 10.0, "high": 30.0, "high_feather": 0.0}),
            "validation: component Luminance range 1 has an invalid luminance-range payload: \
             missing field `low_feather`",
        ),
        (
            "colour-range",
            json!({"samples": [], "refine": 120.0}),
            "validation: component Colour range 1 colour-range refine must be a number within \
             0..=100",
        ),
        (
            "colour-range",
            json!({"samples": [[0.0, 0.0, 99.0]], "refine": 50.0}),
            "validation: component Colour range 1 colour-range samples must each be three \
             linear-sRGB numbers within -16..=16",
        ),
    ];
    for (kind, payload, message) in cases {
        let mask = one_component(kind, payload);
        let error = CompiledMask::new(&mask, stage(64, 48), &no_strokes())
            .expect_err("an illegal payload is refused");
        assert_eq!(error.to_string(), message);
    }
}

/// A colour range past its declared sample count is refused by the limit's own name, and the stored
/// payload is left exactly as it was.
#[test]
fn too_many_samples_are_refused_by_the_declared_limit() {
    let range = RefColourRange {
        samples: vec![[0.1, 0.2, 0.3]; 6],
        refine: 50.0,
    };
    let mask = one_component("colour-range", colour_payload(&range));
    let error = CompiledMask::new(&mask, stage(64, 48), &no_strokes()).expect_err("past the limit");
    assert_eq!(error.kind, luxforge_core::ErrorKind::ResourceLimit);
    assert!(error.detail.contains("the limit is 5 samples"), "{error}");
    assert_eq!(
        mask.components[0].payload["samples"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
}

// ---------------------------------------------------------------------------
// The render, and the sample that must equal it
// ---------------------------------------------------------------------------

/// A source whose bytes vary on both axes and in all three channels, so a wrong row, a dropped
/// channel or a mask read at the wrong pixel is visible in the comparison.
fn byte_source() -> SourceImage {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgba.extend([
                (x * 9 + 3) as u8,
                (y * 13 + 40) as u8,
                (x * 5 + y * 7 + 90) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:mask-range-fixture".into(),
        orientation: 1,
    }
}

/// The colour-study tolerance: an exact code everywhere except within `1e-6 + 1e-6·|threshold|` of a
/// code boundary, where one code of difference is allowed because production decodes, multiplies and
/// blends in `f32`.
fn assert_code(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual} against reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(crossed >= 1, "{case}: code 0 has no lower threshold");
    let threshold = code_threshold(crossed);
    let tolerance = 1e-6 + 1e-6 * threshold.abs();
    assert!(
        (linear - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected} is not within {tolerance} of the threshold \
         {threshold} (reference linear {linear})"
    );
}

/// A one-layer stack whose masked Basic layer lifts exposure by [`MASKED_EV`].
fn masked_stack(mask: Mask) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": MASKED_EV}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        }],
        masks: vec![mask],
        ..Recipe::default()
    }
}

/// **Coverage reads the input pixel of the operation it modulates.** A masked exposure layer over a
/// luminance band and over a colour range renders exactly as the reference composes it, with the
/// coverage evaluated on the layer's *input* — the undisturbed source pixel — and not on what the
/// exposure produced from it.
///
/// This is the whole of the contract in one comparison: a production unit that evaluated the mask on
/// the operation's output would still produce a plausible picture and would disagree with the
/// reference at every partially covered pixel.
#[test]
fn a_range_selection_reads_the_operations_input_and_renders_as_the_reference_composes_it() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let mut rng = SplitMix64(0x00BE_11A5);
    let mut checked = 0usize;
    let mut partial = 0usize;
    for round in 0..14 {
        let (kind, payload, evaluate): (&str, Value, Oracle) = if rng.next_bool() {
            let band = sample_band(&mut rng);
            let oracle = compile_luminance_range(&band);
            (
                "luminance-range",
                luminance_payload(&band),
                Box::new(move |rgb| luminance_coverage(&oracle, rgb)),
            )
        } else {
            // A sample drawn from the fixture itself, so the selection is a real part of the
            // picture rather than an empty one.
            let offset = rng.next_usize((WIDTH * HEIGHT) as usize) * 4;
            let picked = [
                srgb_to_linear(source.rgba[offset]),
                srgb_to_linear(source.rgba[offset + 1]),
                srgb_to_linear(source.rgba[offset + 2]),
            ];
            let range = RefColourRange {
                samples: vec![picked],
                refine: rng.next_range(20.0, 80.0),
            };
            let oracle = compile_colour_range(&range);
            (
                "colour-range",
                colour_payload(&range),
                Box::new(move |rgb| colour_coverage(&oracle, rgb)),
            )
        };
        let mask = one_component(kind, payload);
        let stack = masked_stack(mask);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = ((y * WIDTH + x) * 4) as usize;
                let input = [
                    srgb_to_linear(source.rgba[offset]),
                    srgb_to_linear(source.rgba[offset + 1]),
                    srgb_to_linear(source.rgba[offset + 2]),
                ];
                let m = evaluate(input);
                if m > 0.0 && m < 1.0 {
                    partial += 1;
                }
                let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
                for (channel, &code) in pixel.iter().enumerate().take(3) {
                    let effect = input[channel] * 2.0_f64.powf(MASKED_EV);
                    let blended = (1.0 - m) * input[channel] + m * effect;
                    assert_code(
                        code,
                        linear_to_srgb_code(blended),
                        blended,
                        &format!("round {round} ({kind}), ({x}, {y}) channel {channel}"),
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(
        partial > 200,
        "only {partial} partially covered pixels: the sweep is all endpoints"
    );
    println!("{checked} rendered channels over range selections, {partial} of them on a falloff");
}

// ---------------------------------------------------------------------------
// The sample equals the rendered byte
// ---------------------------------------------------------------------------

/// **A sampled byte equals the rendered byte across a feathered range boundary.**
///
/// `render.sample` and the rasterizing pass reach the mask through one call in one shared function,
/// and that call now carries the pixel; the value a point query hands it is the same input the
/// rasterizing pass snapshots. A point query that read the wrong pixel — the operation's output, or
/// a neighbour — would disagree exactly on the shoulder, which is where this sweep concentrates.
///
/// The band is placed on the fixture's own tones so most of the frame sits on a shoulder rather than
/// at an endpoint, and the test asserts that it does. It is the delivered point-query path: no
/// rectangle is rasterized for it, and a value-based component adds nothing to its cost but the
/// pixel the run already computed.
#[test]
fn a_sampled_byte_equals_the_rendered_byte_across_a_range_boundary() {
    let source = byte_source();
    let registry = ModuleRegistry::builtin();
    // A narrow band with wide shoulders, so nearly every pixel of the fixture sits on a ramp
    // rather than at an endpoint: that is where a point query reading the wrong pixel would show.
    let band = RefLuminanceRange {
        low: 50.0,
        low_feather: 50.0,
        high: 53.0,
        high_feather: 50.0,
    };
    let mask = one_component("luminance-range", luminance_payload(&band));
    let stack = masked_stack(mask);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a frame");
    let oracle = compile_luminance_range(&band);
    let mut partial = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let sampled = sample(&registry, &source, &stack, x, y)
                .expect("a sample of the stage")
                .rgba
                .expect("inside the stage");
            let byte = rendered.pixel(x, y).expect("a pixel of the stage");
            assert_eq!(
                [sampled[0], sampled[1], sampled[2]],
                [byte[0], byte[1], byte[2]],
                "a sampled byte differs from the rendered byte at ({x}, {y})"
            );
            let offset = ((y * WIDTH + x) * 4) as usize;
            let m = luminance_coverage(
                &oracle,
                [
                    srgb_to_linear(source.rgba[offset]),
                    srgb_to_linear(source.rgba[offset + 1]),
                    srgb_to_linear(source.rgba[offset + 2]),
                ],
            );
            if m > 0.0 && m < 1.0 {
                partial += 1;
            }
        }
    }
    assert!(
        partial > (WIDTH * HEIGHT) as usize / 2,
        "only {partial} of {} pixels are on a shoulder",
        WIDTH * HEIGHT
    );
    println!(
        "{partial} pixels on the band's shoulder, every sampled byte equal to the rendered one"
    );
}

// ---------------------------------------------------------------------------
// The RAW linear path
// ---------------------------------------------------------------------------

/// A RAW-shaped linear source whose planes carry values a byte path cannot: below black, above
/// white, and a wide spread across the three channels. The luminance axis is unclamped and the Oklab
/// conversion signed precisely so these pixels are answerable, and this is where that is exercised.
fn linear_source() -> LinearImage {
    let count = (WIDTH * HEIGHT) as usize;
    let mut planes = vec![0.0f32; count * 3];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = (y * WIDTH + x) as usize;
            let t = index as f32 / count as f32;
            planes[index] = -0.05 + 1.5 * t;
            planes[count + index] = 0.02 + 0.9 * (1.0 - t);
            planes[2 * count + index] = 0.3 + 0.6 * ((x % 7) as f32 / 7.0);
        }
    }
    LinearImage::new(WIDTH, HEIGHT, planes).expect("a linear source")
}

/// **The same two components on the RAW linear path**, where the operation's input is a float
/// straight out of the sensor pipeline rather than a decoded byte.
///
/// The linear path reaches the mask through the very same `apply_units` the byte path does, so this
/// is not a second transcription being checked — it is the proof that the pixel a value-based
/// component reads there is the operation's own input and not something the linear path substituted.
/// The comparison is on the rendered byte, against the reference composed on the source planes, with
/// the linear path's own exposure setting applied before the stack so the two are not accidentally
/// the same number.
#[test]
fn a_range_selection_reads_the_operations_input_on_the_raw_linear_path() {
    let registry = ModuleRegistry::builtin();
    let source = linear_source();
    let settings = LinearSettings {
        exposure_ev: 0.0,
        white_balance: None,
    };
    let count = (WIDTH * HEIGHT) as usize;
    let planes = {
        // The same values `linear_source` wrote, recomputed here rather than read back, so the
        // expectation does not depend on the image type's accessors.
        let mut planes = vec![0.0f64; count * 3];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let index = (y * WIDTH + x) as usize;
                let t = index as f32 / count as f32;
                planes[index] = f64::from(-0.05f32 + 1.5 * t);
                planes[count + index] = f64::from(0.02f32 + 0.9 * (1.0 - t));
                planes[2 * count + index] = f64::from(0.3f32 + 0.6 * ((x % 7) as f32 / 7.0));
            }
        }
        planes
    };

    let band = RefLuminanceRange {
        low: 55.0,
        low_feather: 50.0,
        high: 60.0,
        high_feather: 50.0,
    };
    let colours = RefColourRange {
        samples: vec![[0.4, 0.5, 0.6]],
        refine: 12.0,
    };
    let mut partial = 0usize;
    for (kind, payload) in [
        ("luminance-range", luminance_payload(&band)),
        ("colour-range", colour_payload(&colours)),
    ] {
        let oracle_band = compile_luminance_range(&band);
        let oracle_colour = compile_colour_range(&colours);
        let mask = one_component(kind, payload);
        let stack = masked_stack(mask);
        let rendered = render_linear(&registry, &source, SnapshotId::new(), &stack, settings)
            .unwrap_or_else(|error| panic!("{kind}: {error}"));
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let index = (y * WIDTH + x) as usize;
                let input = [
                    planes[index],
                    planes[count + index],
                    planes[2 * count + index],
                ];
                let m = if kind == "luminance-range" {
                    luminance_coverage(&oracle_band, input)
                } else {
                    colour_coverage(&oracle_colour, input)
                };
                if m > 0.0 && m < 1.0 {
                    partial += 1;
                }
                // The sampled byte equals the rendered byte on this path too, through the linear
                // path's own point query.
                let sampled = sample_linear(&registry, &source, &stack, settings, x, y)
                    .expect("a linear sample")
                    .rgba
                    .expect("inside the stage");
                let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
                assert_eq!(
                    [sampled[0], sampled[1], sampled[2]],
                    [pixel[0], pixel[1], pixel[2]],
                    "{kind}: a sampled byte differs from the rendered byte at ({x}, {y})"
                );
                for (channel, &code) in pixel.iter().enumerate().take(3) {
                    let effect = input[channel] * 2.0_f64.powf(MASKED_EV);
                    let blended = (1.0 - m) * input[channel] + m * effect;
                    assert_code(
                        code,
                        linear_to_srgb_code(blended),
                        blended,
                        &format!("{kind} ({x}, {y}) channel {channel}"),
                    );
                }
            }
        }
    }
    assert!(
        partial > 100,
        "only {partial} partially covered pixels on the linear path"
    );
    println!(
        "{partial} partially covered RAW linear pixels, every one as the reference composes it"
    );
}

// ---------------------------------------------------------------------------
// Composition with a gradient and a subtract brush
// ---------------------------------------------------------------------------

/// **A range component combined with a gradient and a subtract brush renders exactly as the frozen
/// algebra composes them.**
///
/// The three components are of three different kinds, two position-based and one value-based, and
/// the expected value is the study's own Zadeh fold transcribed here over three *independent*
/// references — `reference::mask`'s linear and brush coverage and `reference::range`'s band — so
/// nothing in the expectation comes from the code under test.
#[test]
fn a_range_a_gradient_and_a_subtract_brush_compose_as_the_algebra_says() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let reference_stage = RefStage::new(WIDTH, HEIGHT);

    let gradient = Linear {
        x0: 0.1,
        y0: 0.1,
        x1: 0.9,
        y1: 0.8,
    };
    let stroke = Stroke::capture(&[[0.25, 0.3], [0.7, 0.65]], 0.18, 55.0, 90.0, false)
        .expect("a legal stroke");
    let band = RefLuminanceRange {
        low: 35.0,
        low_feather: 25.0,
        high: 80.0,
        high_feather: 20.0,
    };
    let oracle_band = compile_luminance_range(&band);
    let oracle_brush = RefBrush {
        strokes: vec![BrushStroke {
            colour: None,
            points: stroke.points().collect(),
            size: stroke.size(),
            feather: stroke.feather(),
            flow: stroke.flow(),
            erase: stroke.erase(),
        }],
    };

    let mut table = StrokeTable::new("the range composition test");
    let address = table.insert(stroke).to_string();
    let mut mask = Mask::new("Mask 1");
    for (kind, mode, payload) in [
        (
            "linear",
            ComponentMode::Add,
            json!({"x0": gradient.x0, "y0": gradient.y0, "x1": gradient.x1, "y1": gradient.y1}),
        ),
        (
            "luminance-range",
            ComponentMode::Intersect,
            luminance_payload(&band),
        ),
        (
            "brush",
            ComponentMode::Subtract,
            json!({"strokes": [address]}),
        ),
    ] {
        let name = mask.next_component_name(kind);
        mask.components
            .push(Component::new(name, mode, kind, payload));
    }
    mask.amount = 80.0;
    let mask_id = mask.id.clone();
    let stack = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": MASKED_EV}),
            mask: Some(mask_id),
            artifacts: Vec::new(),
        }],
        masks: vec![mask],
        strokes: table,
        artifacts: Default::default(),
    };
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a frame");

    let mut partial = 0usize;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (u, v) = reference_stage.pixel_uv(x, y);
            let offset = ((y * WIDTH + x) * 4) as usize;
            let input = [
                srgb_to_linear(source.rgba[offset]),
                srgb_to_linear(source.rgba[offset + 1]),
                srgb_to_linear(source.rgba[offset + 2]),
            ];
            // The frozen Zadeh fold, transcribed from `docs/design/mask-study.md#composition`: add
            // takes the maximum, intersect and subtract the minimum, and the whole-mask amount is
            // the final multiply.
            let mut m: f64 = 0.0;
            m = m.max(linear_coverage(&gradient, &reference_stage, u, v));
            m = m.min(luminance_coverage(&oracle_band, input));
            m = m.min(1.0 - brush_coverage(&oracle_brush, &reference_stage, u, v, input));
            let m = 0.8 * m;
            if m > 0.0 && m < 0.8 {
                partial += 1;
            }
            let pixel = rendered.pixel(x, y).expect("a pixel of the stage");
            for (channel, &code) in pixel.iter().enumerate().take(3) {
                let effect = input[channel] * 2.0_f64.powf(MASKED_EV);
                let blended = (1.0 - m) * input[channel] + m * effect;
                assert_code(
                    code,
                    linear_to_srgb_code(blended),
                    blended,
                    &format!("({x}, {y}) channel {channel}"),
                );
            }
        }
    }
    assert!(
        partial > 50,
        "only {partial} partially covered pixels: the three components do not overlap enough to \
         prove the fold"
    );
    println!(
        "{partial} pixels where all three components composed partially, all as the algebra says"
    );
}

// ---------------------------------------------------------------------------
// The API: creating, patching and sampling a range component
// ---------------------------------------------------------------------------

static CASE: AtomicU64 = AtomicU64::new(0);

fn temp(name: &str) -> PathBuf {
    let case = CASE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "luxforge-mask-range-{name}-{}-{case}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a catalog directory");
    dir
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

/// The generated methods carry both kinds end to end: created, patched on a field, sampled by the
/// numbers a canvas pick reads, and a swatch removed on its own — each as one history entry, through
/// the same `mask.*` family every other kind reaches.
///
/// This is the API half of the acceptance: a range component that could be evaluated and not created
/// would be a component no client can reach, exactly as the radial was before its generated methods
/// existed.
#[test]
fn the_generated_methods_create_patch_sample_and_unsample_a_range() {
    let dir = temp("api");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).expect("the fixture copies");
    let mut service = EditorService::open(&dir.join("catalog.sqlite")).expect("a catalog");
    let asset: AssetId = service
        .import(&source)
        .expect("the fixture imports")
        .asset
        .id;
    let mut request = 0u64;
    let mut run =
        |service: &mut EditorService, method: &str, target: MaskTarget, parameters: Value| {
            request += 1;
            let command = commands::find(method).expect("a declared command");
            let mutation = Mutation {
                expected_revision: service.state(&asset).unwrap().revision,
                request_id: format!("range-{request}"),
                actor: "mask-range".to_owned(),
            };
            service
                .apply_mask_command(&asset, mutation, command, parameters, target)
                .unwrap_or_else(|error| panic!("{method}: {error}"))
        };
    let listing = |service: &EditorService| {
        let entry = service.state(&asset).unwrap().current_entry.id;
        service.mask_listing(&asset, &entry).expect("a listing")
    };

    let created = run(
        &mut service,
        "mask.create-colour-range",
        MaskTarget::default(),
        json!({"refine": 50.0}),
    );
    let mask = created.mask.clone().expect("a created mask");
    let component = created.component.clone().expect("its first component");
    assert_eq!(created.label.as_deref(), Some("Add colour range"));
    let of_component = MaskTarget {
        mask: Some(mask.clone()),
        component: Some(component),
        name: None,
        stroke: None,
    };

    let sampled = run(
        &mut service,
        "mask.add-colour-range-sample",
        of_component.clone(),
        json!({"r": 0.2, "g": 0.35, "b": 0.5}),
    );
    assert_eq!(sampled.label.as_deref(), Some("Sample Colour range 1"));

    // Sampling the same colour again changes nothing: the fold is `min`, so a duplicate is a no-op
    // and spending one of five swatches on it would be a silent loss.
    let again = run(
        &mut service,
        "mask.add-colour-range-sample",
        of_component.clone(),
        json!({"r": 0.2, "g": 0.35, "b": 0.5}),
    );
    assert_eq!(again.label, None, "a duplicate sample writes no entry");

    run(
        &mut service,
        "mask.add-colour-range-sample",
        of_component.clone(),
        json!({"r": 0.9, "g": 0.1, "b": 0.1}),
    );
    run(
        &mut service,
        "mask.set-colour-range",
        of_component.clone(),
        json!({"refine": 72.0}),
    );

    let stored = listing(&service).masks[0].components[0].clone();
    assert_eq!(stored.kind, "colour-range");
    assert!(stored.available);
    assert_eq!(stored.payload["refine"], json!(72.0));
    assert_eq!(
        stored.payload["samples"],
        json!([[0.2, 0.35, 0.5], [0.9, 0.1, 0.1]])
    );

    let removed = run(
        &mut service,
        "mask.delete-colour-range-sample",
        of_component,
        json!({"index": 0}),
    );
    assert_eq!(
        removed.label.as_deref(),
        Some("Remove a sample from Colour range 1")
    );
    assert_eq!(
        listing(&service).masks[0].components[0].payload["samples"],
        json!([[0.9, 0.1, 0.1]])
    );

    // A second component, of the other range kind, added to the same mask and patched on one field.
    let added = run(
        &mut service,
        "mask.add-luminance-range",
        MaskTarget {
            mask: Some(mask.clone()),
            component: None,
            name: None,
            stroke: None,
        },
        json!({"mode": "intersect", "low": 20.0, "low_feather": 5.0, "high": 80.0, "high_feather": 5.0}),
    );
    let band = added.component.clone().expect("the appended component");
    run(
        &mut service,
        "mask.set-luminance-range",
        MaskTarget {
            mask: Some(mask),
            component: Some(band),
            name: None,
            stroke: None,
        },
        json!({"high_feather": 12.5}),
    );
    let stored = listing(&service).masks[0].components[1].clone();
    assert_eq!(stored.kind, "luminance-range");
    assert_eq!(stored.payload["high_feather"], json!(12.5));
    assert_eq!(stored.payload["low"], json!(20.0));

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The cost of a whole-stage rectangle
// ---------------------------------------------------------------------------

/// What a whole-stage rectangle costs, measured rather than argued: the coverage field of one
/// component of each kind over every pixel of a 24 MP stage, beside a geometric component that can
/// skip most of the frame.
///
/// Ignored by default because it is a measurement and not a pass/fail property. Run it with
/// `cargo test --release --locked --package luxforge-core --test mask_range --
/// --ignored --nocapture the_cost_of_a_whole_stage_rectangle`, and record the host's one-minute
/// load average beside the figure.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn the_cost_of_a_whole_stage_rectangle() {
    let size = stage(6000, 4000);
    let band = RefLuminanceRange {
        low: 25.0,
        low_feather: 15.0,
        high: 75.0,
        high_feather: 15.0,
    };
    let colours = RefColourRange {
        samples: vec![[0.12, 0.2, 0.42], [0.5, 0.3, 0.2]],
        refine: 50.0,
    };
    let cases: [(&str, Value); 3] = [
        // A gradient placed low in the frame, so its half-plane rectangle excludes most of the
        // stage: that is the comparison the two range kinds are measured against.
        (
            "linear",
            json!({"x0": 0.5, "y0": 0.6, "x1": 0.5, "y1": 0.9}),
        ),
        ("luminance-range", luminance_payload(&band)),
        ("colour-range", colour_payload(&colours)),
    ];
    for (kind, payload) in cases {
        let mask = one_component(kind, payload);
        let compiled = CompiledMask::new(&mask, size, &no_strokes()).expect("a legal payload");
        let bounds = compiled.bounds();
        let covered = u64::from(bounds.width) * u64::from(bounds.height);
        let total = u64::from(size.width) * u64::from(size.height);
        let mut rgb = [0.2f32, 0.4, 0.6];
        let started = std::time::Instant::now();
        let mut sum = 0.0f64;
        for y in bounds.y0..bounds.y1() {
            for x in bounds.x0..bounds.x1() {
                // A varying pixel, so the value-based branch is not folded to a constant.
                rgb[0] = (x % 251) as f32 / 251.0;
                rgb[1] = (y % 241) as f32 / 241.0;
                sum += f64::from(compiled.evaluate(x, y, rgb));
            }
        }
        std::hint::black_box(sum);
        let elapsed = started.elapsed();
        println!(
            "{kind}: rectangle {}x{} ({:.1}% of the stage), {:.1} ms over it, {:.2} ns/pixel",
            bounds.width,
            bounds.height,
            100.0 * covered as f64 / total as f64,
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1e9 / covered as f64
        );
    }
}
