//! What is a range selection's alone, beside the checklist every kind passes (`mask::kinds`, where
//! both range kinds are held to `crates/luxforge-reference/src/range.rs` bit for bit, P12's and P13's
//! answers are asserted for every kind, and a sampled byte is held to the rendered one):
//!
//! * **Coverage reads the input of the operation it modulates**, on the byte path and the RAW linear
//!   path, which a production unit that read the operation's output would get plausibly wrong at
//!   every partially covered pixel.
//! * **The composition.** A range component combined with a gradient and a subtract brush renders
//!   exactly as the frozen algebra composes them, which is what makes a sky selection practical.
//! * **The declared sample limit** and the generated methods that create, patch and sample a range.
//! * **What a whole-stage rectangle costs**, measured.

use super::kinds::{
    colour_range::colour_payload,
    luminance_range::{luminance_payload, sample_band},
};
use super::*;
use luxforge_core::{
    AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EditorService, Layer, LayerId,
    Mask, ModuleRegistry, Mutation, RECIPE_FORMAT, Recipe, SnapshotId,
    mask::{
        CompiledMask,
        commands::{self, MaskTarget},
    },
    path::{Stroke, StrokeTable},
};
use luxforge_core::{LinearImage, LinearSettings};
use luxforge_reference::mask::{
    Brush as RefBrush, BrushStroke, Linear, Stage as RefStage, brush_coverage, linear_coverage,
};
use luxforge_reference::range::{
    ColourRange as RefColourRange, LuminanceRange as RefLuminanceRange, colour_coverage,
    compile_colour_range, compile_luminance_range, luminance_coverage,
};
use luxforge_reference::srgb;
use luxforge_testkit::fixtures::{render, render_linear, sample_linear};
use serde_json::{Value, json};

/// One component's reference coverage at one linear pixel, boxed so a sweep can hold whichever of
/// the two kinds it drew this round.
type Oracle = Box<dyn Fn([f64; 3]) -> f64>;

fn no_strokes() -> StrokeTable {
    StrokeTable::default()
}

// ---------------------------------------------------------------------------
// Payload legality
// ---------------------------------------------------------------------------

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
                srgb::decode(source.rgba[offset]),
                srgb::decode(source.rgba[offset + 1]),
                srgb::decode(source.rgba[offset + 2]),
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
                    srgb::decode(source.rgba[offset]),
                    srgb::decode(source.rgba[offset + 1]),
                    srgb::decode(source.rgba[offset + 2]),
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
                        srgb::code(blended),
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
                        srgb::code(blended),
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
/// references — `luxforge_reference::mask`'s linear and brush coverage and `luxforge_reference::range`'s band — so
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
                srgb::decode(source.rgba[offset]),
                srgb::decode(source.rgba[offset + 1]),
                srgb::decode(source.rgba[offset + 2]),
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
                    srgb::code(blended),
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
    std::fs::copy(luxforge_testkit::fixtures::jpeg(), &source).expect("the fixture copies");
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
                .run_action(&asset, mutation, command.method, target.request(parameters))
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
/// `cargo test --release --locked --package luxforge-core --test mask --
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
