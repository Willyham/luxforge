//! The Basic module's Exposure parameter end to end: the compiled colour unit against the
//! independent f64 corpus, mixed geometry/replacement order, the editing journey through the
//! host's one action path and the draft lifecycle through the JSON methods.
//!
//! Numerical rule, from the design's numerical contract and `fixtures/basic/README.md`: a rendered
//! code must equal the f64 reference's code exactly, except where the reference's linear value sits
//! within `1e-6 + 1e-6 · |threshold|` of the exact linear threshold between two codes, where one
//! code of difference is permitted because production decodes and multiplies in f32. Identity
//! stacks, byte sharing and history behaviour are exact with no tolerance at all.

mod reference;

use lightwell_core::{
    ApiRequest, Availability, BASIC_EFFECT, CROP_EFFECT, EFFECT_FORMAT, EditorService, Error,
    ErrorKind, Layer, LayerId, ModuleDescriptor, ModuleRegistry, Mutation, MutationOutcome,
    ORIENTATION_EFFECT, Orientation, OwnerHandle, PIXEL_EFFECT, RECIPE_FORMAT, Recipe, SnapshotId,
    SourceImage, ToolModule, Transform, render,
};
use reference::{RefOp, code_threshold, evaluate_pixel, exposure, srgb_to_linear};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

/// A synthetic opaque source: exact 8-bit codes, so a rendered byte can be compared with the f64
/// reference without a decoder's own rounding in the way.
fn source_of(width: u32, height: u32, pixels: &[[u8; 3]]) -> SourceImage {
    assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:basic-exposure-fixture".into(),
        orientation: 1,
    }
}

fn basic_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        artifacts: Vec::new(),
    }
}

fn exposure_layer(ev: f64) -> Layer {
    basic_layer(json!({ "exposure": ev }))
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
    }
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "basic-exposure-test".into(),
    }
}

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-basic-{name}-{}.sqlite",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    path
}

/// The declared tolerance: an exact code, unless the reference's linear value sits within
/// `1e-6 + 1e-6 · |threshold|` of the threshold between the two codes, where one code of
/// difference is permitted.
fn assert_code(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual}, reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(
        crossed >= 1,
        "{case}: code 0 has no lower threshold to sit on"
    );
    let threshold = code_threshold(crossed);
    let tolerance = 1e-6 + 1e-6 * threshold.abs();
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {tolerance} of the code threshold {threshold}"
    );
}

// ---------------------------------------------------------------------------------------------
// The compiled unit against the independent f64 corpus
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExposureCase {
    input: [u8; 3],
    ev: f64,
    expected: [u8; 3],
    #[serde(default)]
    #[allow(dead_code)]
    note: String,
}

/// Every case of `fixtures/basic/exposure-cases.json`, rendered through a real Basic layer at the
/// exact input codes the corpus names. The corpus covers 0, ±0.5, ±1, ±2 and ±5 EV, the identity
/// codes and the explicit clip and floor boundaries.
#[test]
fn every_corpus_case_renders_through_a_real_basic_layer() {
    let raw = fs::read_to_string(fixture("basic/exposure-cases.json")).expect("the corpus");
    let cases: Vec<ExposureCase> = serde_json::from_str(&raw).expect("a corpus of cases");
    assert!(cases.len() >= 132, "the committed corpus has 132 cases");
    // One render per distinct EV, with every input code of that EV as a pixel of one row, so the
    // rendered frame is compared against the corpus pixel for pixel.
    let mut by_ev: BTreeMap<u64, Vec<&ExposureCase>> = BTreeMap::new();
    for case in &cases {
        by_ev.entry(case.ev.to_bits()).or_default().push(case);
    }
    let registry = ModuleRegistry::builtin();
    let mut covered: Vec<f64> = Vec::new();
    for group in by_ev.values() {
        let ev = group[0].ev;
        covered.push(ev);
        let inputs: Vec<[u8; 3]> = group.iter().map(|case| case.input).collect();
        let source = source_of(inputs.len() as u32, 1, &inputs);
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![exposure_layer(ev)]),
        )
        .expect("a rendered exposure");
        assert_eq!((rendered.width, rendered.height), (inputs.len() as u32, 1));
        for (index, case) in group.iter().enumerate() {
            let pixel = rendered.pixel(index as u32, 0).expect("a rendered pixel");
            assert_eq!(pixel[3], 255, "alpha is never touched");
            // The Rust reference agrees with the corpus (proved in `basic_reference.rs`); this
            // asserts against the committed file itself.
            assert_eq!(
                evaluate_pixel(case.input, &[RefOp::Exposure(ev)]),
                case.expected,
                "the reference disagrees with the corpus at {:?} {ev} EV",
                case.input
            );
            for (channel, input) in case.input.iter().enumerate() {
                let linear = exposure(srgb_to_linear(*input), ev);
                assert_code(
                    pixel[channel],
                    case.expected[channel],
                    linear,
                    &format!("input {:?} channel {channel} at {ev} EV", case.input),
                );
            }
        }
    }
    for required in [-5.0, -2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 5.0] {
        assert!(
            covered.contains(&required),
            "the corpus covers {required} EV"
        );
    }
}

/// A sample and a rendered byte are the same evaluation, over every code and several exposures.
#[test]
fn a_sample_equals_the_rendered_byte_for_every_code() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, code, code]).collect();
    let source = source_of(256, 1, &codes);
    for ev in [-5.0, -1.0, -0.25, 0.5, 1.0, 5.0] {
        let stack = recipe(vec![exposure_layer(ev)]);
        let rendered =
            render(&registry, &source, SnapshotId::new(), &stack).expect("a rendered frame");
        for code in 0u32..256 {
            let sampled = lightwell_core::sample(&registry, &source, &stack, code, 0)
                .expect("a sample")
                .rgba
                .expect("an opaque pixel");
            assert_eq!(
                sampled,
                rendered.pixel(code, 0).expect("a rendered pixel"),
                "code {code} at {ev} EV"
            );
        }
    }
}

/// A neutral Basic layer is a legal layer that compiles to no processing at all: the rendered bytes
/// are the source's own allocation, not a copy of it.
#[test]
fn a_neutral_basic_layer_renders_the_source_buffer_itself() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(16, 16, &codes);
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    for payload in [
        json!({}),
        json!({"exposure": 0.0}),
        json!({"exposure": -0.0}),
    ] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![basic_layer(payload.clone())]),
        )
        .expect("a neutral render");
        assert_eq!(rendered.rgba, identity.rgba, "{payload} changed a byte");
        assert!(
            Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    // A non-neutral layer does materialize a frame, which is what makes the sharing above a real
    // property rather than a render that never happened.
    let exposed = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![exposure_layer(1.0)]),
    )
    .expect("an exposed render");
    assert!(!Arc::ptr_eq(&exposed.rgba, &source.rgba));
    assert_ne!(exposed.rgba, identity.rgba);
}

// ---------------------------------------------------------------------------------------------
// Mixed order with replacements and exact geometry
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct MixedOrder {
    width: u32,
    height: u32,
    base_rgba: Vec<u8>,
    ev: f64,
    replace: Replace,
    cases: BTreeMap<String, MixedCase>,
}

#[derive(Deserialize)]
struct Replace {
    x: u32,
    y: u32,
    rgb: [u8; 3],
}

#[derive(Deserialize)]
struct MixedCase {
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
    order: Vec<String>,
    expected_rgba: Vec<u8>,
}

/// `fixtures/basic/mixed-order.json` through real layers: a replacement before the Basic layer is
/// processed by it, one after it is not, and an exact mirror commutes with the colour operation.
#[test]
fn mixed_order_cases_reproduce_exactly_through_real_layers() {
    let raw = fs::read_to_string(fixture("basic/mixed-order.json")).expect("the mixed-order file");
    let file: MixedOrder = serde_json::from_str(&raw).expect("mixed-order cases");
    let source = SourceImage {
        width: file.width,
        height: file.height,
        rgba: file.base_rgba.clone().into(),
        fingerprint: "sha256:mixed-order-fixture".into(),
        orientation: 1,
    };
    let registry = ModuleRegistry::builtin();
    assert_eq!(file.cases.len(), 4, "every committed case is exercised");
    for (name, case) in &file.cases {
        let layers: Vec<Layer> = case
            .order
            .iter()
            .map(|step| match step.as_str() {
                "point_replace" => Layer::pixel(file.replace.x, file.replace.y, file.replace.rgb),
                "exposure" => exposure_layer(file.ev),
                "mirror" => Layer::orientation(Orientation::of(Transform::MirrorHorizontal)),
                other => panic!("{name}: unknown step {other}"),
            })
            .collect();
        let rendered = render(&registry, &source, SnapshotId::new(), &recipe(layers))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            rendered.rgba.as_ref(),
            case.expected_rgba.as_slice(),
            "{name} did not reproduce its expected buffer"
        );
    }
    assert_eq!(
        file.cases["mirror_then_expose"].expected_rgba,
        file.cases["expose_then_mirror"].expected_rgba,
        "exact geometry commutes with pointwise colour"
    );
}

// ---------------------------------------------------------------------------------------------
// The editing journey through the host's one action path
// ---------------------------------------------------------------------------------------------

/// The Basic layer joins a stack that already holds a pixel replacement and a geometry tail, at the
/// colour insertion index before that tail, and every later set updates it in place.
#[test]
fn the_first_set_places_one_layer_before_the_geometry_tail_and_later_sets_update_it() {
    let path = catalog("place");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;

    service
        .apply_action(
            &asset,
            mutation(0, "pixel"),
            "set-pixel",
            json!({"x": 2, "y": 3, "rgb": [9, 9, 9]}),
        )
        .expect("a pixel edit");
    service
        .apply_transform(&asset, mutation(1, "rotate"), Transform::RotateRight)
        .expect("a rotation");
    service
        .apply_action(
            &asset,
            mutation(2, "crop"),
            "crop",
            json!({"x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}),
        )
        .expect("a crop");
    let before: Vec<String> = layers(&service, &asset)
        .iter()
        .map(|layer| layer.effect_id.clone())
        .collect();
    assert_eq!(
        before,
        [PIXEL_EFFECT, ORIENTATION_EFFECT, CROP_EFFECT],
        "the stack under test holds a replacement and a geometry tail"
    );

    let first = service
        .apply_action(
            &asset,
            mutation(3, "expose"),
            "set-basic",
            json!({"exposure": 0.5}),
        )
        .expect("the first exposure");
    assert_eq!(first.outcome, MutationOutcome::Applied);
    let stack = layers(&service, &asset);
    assert_eq!(
        stack
            .iter()
            .map(|layer| layer.effect_id.as_str())
            .collect::<Vec<_>>(),
        [PIXEL_EFFECT, BASIC_EFFECT, ORIENTATION_EFFECT, CROP_EFFECT],
        "a colour layer joins the stack before the geometry tail, after the replacement"
    );
    let basic = stack[1].clone();
    assert_eq!(basic.payload, json!({"exposure": 0.5}));

    // A second set updates that same layer at the same index; nothing else moves.
    service
        .apply_action(
            &asset,
            mutation(4, "expose-again"),
            "set-basic",
            json!({"exposure": -1.25}),
        )
        .expect("a second exposure");
    let stack = layers(&service, &asset);
    assert_eq!(stack.len(), 4, "no second Basic layer was added");
    assert_eq!(stack[1].id, basic.id, "the layer keeps its identity");
    assert_eq!(stack[1].payload, json!({"exposure": -1.25}));
    assert_eq!(stack[0].id, before_ids(&before, &stack)[0]);

    // The same value again is a no-op with no history row.
    let entries = service
        .history(&asset, None, 50)
        .expect("history")
        .entries
        .len();
    let repeat = service
        .apply_action(
            &asset,
            mutation(5, "expose-same"),
            "set-basic",
            json!({"exposure": -1.25}),
        )
        .expect("an unchanged set");
    assert_eq!(repeat.outcome, MutationOutcome::NoOp);
    assert_eq!(repeat.created_entry_id, None);
    assert_eq!(
        service
            .history(&asset, None, 50)
            .expect("history")
            .entries
            .len(),
        entries,
        "a set that changes nothing writes no history row"
    );

    // A reset keeps the layer and its identity; resetting again is a no-op.
    let reset = service
        .apply_action(&asset, mutation(5, "reset"), "reset-basic", json!({}))
        .expect("a reset");
    assert_eq!(reset.outcome, MutationOutcome::Applied);
    let stack = layers(&service, &asset);
    assert_eq!(stack[1].id, basic.id, "a reset keeps the layer's identity");
    assert_eq!(stack[1].payload, json!({}), "the canonical neutral payload");
    let again = service
        .apply_action(&asset, mutation(6, "reset-again"), "reset-basic", json!({}))
        .expect("a second reset");
    assert_eq!(again.outcome, MutationOutcome::NoOp);
    assert_eq!(again.created_entry_id, None);

    // A set of neutral on a neutral layer is a no-op too, both spellings of the same state.
    let neutral_set = service
        .apply_action(
            &asset,
            mutation(6, "expose-zero"),
            "set-basic",
            json!({"exposure": 0.0}),
        )
        .expect("a neutral set");
    assert_eq!(neutral_set.outcome, MutationOutcome::NoOp);

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// Every group reset the Basic descriptor declares is one legal `set-basic` patch: sent against a
/// layer that holds those fields it commits exactly one entry and keeps the layer's identity and
/// position, and sent again against the group it has already cleared it is a no-op with no entry.
/// The presets come from the descriptor, so a group that gains a field is covered the day it does.
#[test]
fn every_declared_group_reset_commits_once_and_then_reports_a_no_op() {
    let path = catalog("group-resets");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    let basic = ModuleRegistry::builtin()
        .descriptors()
        .into_iter()
        .find(|module| module.id == "lightwell.basic")
        .expect("the Basic module is registered")
        .clone();
    let groups: Vec<(String, Map<String, Value>)> = basic
        .controls
        .iter()
        .filter_map(|control| match control {
            lightwell_core::Control::Group {
                label,
                reset: Some(reset),
                ..
            } => Some((label.clone(), reset.preset.clone())),
            _ => None,
        })
        .collect();
    assert!(groups.len() >= 3, "the Basic module declares its groups");

    let mut revision = 0u64;
    let mut identity: Option<LayerId> = None;
    for (label, preset) in &groups {
        // Put every field of this group somewhere other than its default, one patch per group.
        let mut set = Map::new();
        for name in preset.keys() {
            let declared = basic
                .action("set-basic")
                .and_then(|action| action.parameter(name))
                .expect("a declared parameter");
            let lightwell_core::ParameterKind::Number { max, .. } = declared.kind else {
                panic!("{name} is not a number");
            };
            set.insert(name.clone(), json!(max / 2.0));
        }
        let applied = service
            .apply_action(
                &asset,
                mutation(revision, &format!("{label}-set")),
                "set-basic",
                Value::Object(set),
            )
            .unwrap_or_else(|error| panic!("{label} could not be set: {error}"));
        assert_eq!(applied.outcome, MutationOutcome::Applied, "{label}");
        revision = applied.revision;
        let stack = layers(&service, &asset);
        let layer = stack
            .iter()
            .find(|layer| layer.effect_id == BASIC_EFFECT)
            .expect("the Basic layer");
        match &identity {
            Some(id) => assert_eq!(&layer.id, id, "{label} replaced the Basic layer"),
            None => identity = Some(layer.id.clone()),
        }

        // The group's own reset: one entry, labelled by the module, layer kept in place.
        let before = service
            .history(&asset, None, 50)
            .expect("history")
            .entries
            .len();
        let reset = service
            .apply_action(
                &asset,
                mutation(revision, &format!("{label}-reset")),
                "set-basic",
                Value::Object(preset.clone()),
            )
            .unwrap_or_else(|error| panic!("{label} could not be reset: {error}"));
        assert_eq!(reset.outcome, MutationOutcome::Applied, "{label}");
        revision = reset.revision;
        assert_eq!(
            service
                .history(&asset, None, 50)
                .expect("history")
                .entries
                .len(),
            before + 1,
            "{label}'s reset wrote more than one entry"
        );
        assert_eq!(
            service
                .entry(&asset, &reset.created_entry_id.clone().expect("an entry"))
                .expect("the entry")
                .label,
            format!("Reset {label}"),
            "{label}'s reset is not labelled by the module"
        );
        let stack = layers(&service, &asset);
        let layer = stack
            .iter()
            .find(|layer| layer.effect_id == BASIC_EFFECT)
            .expect("the Basic layer survives its own reset");
        assert_eq!(
            Some(&layer.id),
            identity.as_ref(),
            "{label}'s reset replaced the layer instead of updating it"
        );
        for name in preset.keys() {
            assert!(
                layer.payload.get(name).is_none(),
                "{label} left {name} in the payload"
            );
        }

        // The same reset again changes nothing at all.
        let again = service
            .apply_action(
                &asset,
                mutation(revision, &format!("{label}-reset-again")),
                "set-basic",
                Value::Object(preset.clone()),
            )
            .unwrap_or_else(|error| panic!("{label} could not be reset twice: {error}"));
        assert_eq!(again.outcome, MutationOutcome::NoOp, "{label}");
        assert_eq!(again.created_entry_id, None, "{label} wrote a no-op entry");
    }

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

fn layers(service: &EditorService, asset: &lightwell_core::AssetId) -> Vec<Layer> {
    service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
        .layers
}

fn before_ids(before: &[String], stack: &[Layer]) -> Vec<LayerId> {
    assert_eq!(before[0], stack[0].effect_id);
    vec![stack[0].id.clone()]
}

/// Without a layer, a neutral first set adds nothing at all.
#[test]
fn a_neutral_first_set_adds_no_layer() {
    let path = catalog("neutral-first");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    let result = service
        .apply_action(
            &asset,
            mutation(0, "zero"),
            "set-basic",
            json!({"exposure": 0.0}),
        )
        .expect("a neutral set");
    assert_eq!(result.outcome, MutationOutcome::NoOp);
    assert!(layers(&service, &asset).is_empty());
    let empty = service
        .apply_action(&asset, mutation(0, "empty"), "set-basic", json!({}))
        .expect("an empty patch");
    assert_eq!(empty.outcome, MutationOutcome::NoOp);
    assert!(layers(&service, &asset).is_empty());
    let reset = service
        .apply_action(&asset, mutation(0, "reset"), "reset-basic", json!({}))
        .expect("a reset without a layer");
    assert_eq!(reset.outcome, MutationOutcome::NoOp);
    assert!(layers(&service, &asset).is_empty());
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// The history row stores the patch as sent and the label the module gives it; `recipe.describe`
/// reports the layer's summary and its effective values; a retry of the same request returns the
/// original result without a second row.
#[test]
fn history_stores_the_patch_as_sent_with_its_label_and_describes_the_layer() {
    let path = catalog("history");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;

    let applied = service
        .apply_action(
            &asset,
            mutation(0, "expose"),
            "set-basic",
            json!({"exposure": 0.5}),
        )
        .expect("an exposure");
    let entry_id = applied.created_entry_id.clone().expect("a created entry");
    let entry = service.entry(&asset, &entry_id).expect("the entry");
    assert_eq!(entry.action_id, "set-basic");
    assert_eq!(
        entry.parameters,
        json!({"exposure": 0.5}),
        "the stored parameters are the patch as sent, not the merged payload"
    );
    assert_eq!(entry.label, "Exposure +0.50 EV");

    // A retry of the same request id returns the original result and adds no row.
    let retry = service
        .apply_action(
            &asset,
            mutation(0, "expose"),
            "set-basic",
            json!({"exposure": 0.5}),
        )
        .expect("a retry");
    assert!(retry.deduplicated, "a retry is deduplicated");
    assert_eq!(retry.created_entry_id, Some(entry_id.clone()));
    assert_eq!(
        service
            .history(&asset, None, 50)
            .expect("history")
            .entries
            .len(),
        2,
        "the import and the one exposure"
    );

    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert_eq!(row.module.as_deref(), Some("lightwell.basic"));
    assert_eq!(row.title.as_deref(), Some("Basic"));
    assert_eq!(row.summary, "Exposure +0.50 EV");
    assert!(row.available);
    assert_eq!(
        row.values,
        json!({
            "temperature": 0.0,
            "tint": 0.0,
            "exposure": 0.5,
            "contrast": 0.0,
            "highlights": 0.0,
            "shadows": 0.0,
            "whites": 0.0,
            "blacks": 0.0,
            "vibrance": 0.0,
            "saturation": 0.0,
        })
        .as_object()
        .cloned()
        .unwrap(),
        "the row reports every implemented field, neutral ones included"
    );

    // The group reset and the module reset each carry their own label. The Tone group reset names
    // every one of its six fields at neutral, matching the descriptor's own group-reset preset.
    let tone = service
        .apply_action(
            &asset,
            mutation(1, "tone-reset"),
            "set-basic",
            json!({
                "exposure": 0.0,
                "contrast": 0.0,
                "highlights": 0.0,
                "shadows": 0.0,
                "whites": 0.0,
                "blacks": 0.0,
            }),
        )
        .expect("the Tone group reset");
    assert_eq!(
        service
            .entry(&asset, &tone.created_entry_id.expect("an entry"))
            .expect("the entry")
            .label,
        "Reset Tone"
    );
    service
        .apply_action(
            &asset,
            mutation(2, "expose-2"),
            "set-basic",
            json!({"exposure": 2.0}),
        )
        .expect("a second exposure");
    let module_reset = service
        .apply_action(
            &asset,
            mutation(3, "module-reset"),
            "reset-basic",
            json!({}),
        )
        .expect("the module reset");
    let reset_entry = service
        .entry(&asset, &module_reset.created_entry_id.expect("an entry"))
        .expect("the entry");
    assert_eq!(reset_entry.label, "Reset Basic");
    assert_eq!(reset_entry.parameters, json!({}));
    // The neutral layer is still described, with the neutral value of every implemented field.
    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert_eq!(row.summary, "Neutral");
    assert_eq!(
        row.values,
        json!({
            "temperature": 0.0,
            "tint": 0.0,
            "exposure": 0.0,
            "contrast": 0.0,
            "highlights": 0.0,
            "shadows": 0.0,
            "whites": 0.0,
            "blacks": 0.0,
            "vibrance": 0.0,
            "saturation": 0.0,
        })
        .as_object()
        .cloned()
        .unwrap(),
        "every implemented field reports its neutral value"
    );
    assert_eq!(row.values.get("exposure"), Some(&json!(0.0)));

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// Undo, redo, restore and reopening the catalog evaluate the same bytes, so an exposure is
/// reproduced from the original pixels and the stored payload alone.
#[test]
fn undo_redo_restore_and_reopen_evaluate_identically() {
    let path = catalog("journey");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    let original_bytes = fs::read(jpeg()).expect("the fixture");

    let original = service.render_current(&asset).expect("the original render");
    let first = service
        .apply_action(
            &asset,
            mutation(0, "expose-1"),
            "set-basic",
            json!({"exposure": 1.0}),
        )
        .expect("an exposure")
        .created_entry_id
        .expect("an entry");
    let exposed = service.render_current(&asset).expect("an exposed render");
    assert_ne!(exposed.rgba, original.rgba);
    let second = service
        .apply_action(
            &asset,
            mutation(1, "expose-2"),
            "set-basic",
            json!({"exposure": -2.0}),
        )
        .expect("a second exposure")
        .created_entry_id
        .expect("an entry");
    let darkened = service.render_current(&asset).expect("a darkened render");

    service.undo(&asset, mutation(2, "undo")).expect("an undo");
    assert_eq!(
        service.render_current(&asset).expect("after undo").rgba,
        exposed.rgba
    );
    service.redo(&asset, mutation(3, "redo")).expect("a redo");
    assert_eq!(
        service.render_current(&asset).expect("after redo").rgba,
        darkened.rgba
    );
    service
        .restore(&asset, mutation(4, "restore"), &first)
        .expect("a restore");
    assert_eq!(
        service.render_current(&asset).expect("after restore").rgba,
        exposed.rgba
    );
    // A historical entry renders its own stack whatever the current pointer is.
    assert_eq!(
        service
            .render_entry(&asset, &second)
            .expect("the entry render")
            .rgba,
        darkened.rgba
    );

    drop(service);
    let reopened = EditorService::open(&path).expect("the reopened catalog");
    assert_eq!(
        reopened.render_current(&asset).expect("after reopen").rgba,
        exposed.rgba,
        "a reopened catalog evaluates the same bytes"
    );
    assert_eq!(
        reopened
            .render_entry(&asset, &second)
            .expect("the entry render")
            .rgba,
        darkened.rgba
    );
    assert_eq!(
        fs::read(jpeg()).expect("the fixture"),
        original_bytes,
        "the original is never modified"
    );
    drop(reopened);
    fs::remove_file(path).expect("the catalog is removed");
}

/// An unsupported payload format and an unknown field are refused explicitly, and the stored recipe
/// is neither rewritten nor rendered without the effect.
#[test]
fn an_unsupported_format_or_unknown_field_is_refused_without_touching_the_stored_recipe() {
    let path = catalog("format");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "expose"),
            "set-basic",
            json!({"exposure": 1.0}),
        )
        .expect("an exposure");
    let stored = layers(&service, &asset);
    let rendered = service.render_current(&asset).expect("a render");

    // A request naming a field no parameter declares is refused by the generic check.
    let unknown = service
        .apply_action(
            &asset,
            mutation(1, "unknown"),
            "set-basic",
            json!({"gamma": 10.0}),
        )
        .expect_err("an unknown field");
    assert_eq!(unknown.kind, ErrorKind::Validation);
    assert!(unknown.detail.contains("gamma"), "{}", unknown.detail);
    // So is a value outside the declared range.
    for out_of_range in [json!({"exposure": 5.001}), json!({"exposure": -6.0})] {
        let error = service
            .apply_action(&asset, mutation(1, "range"), "set-basic", out_of_range)
            .expect_err("an out-of-range value");
        assert_eq!(error.kind, ErrorKind::Validation);
    }
    assert_eq!(layers(&service, &asset), stored, "nothing was written");
    assert_eq!(
        service.render_current(&asset).expect("a render").rgba,
        rendered.rgba
    );

    // A stored payload of an unsupported format is refused by the provider, not rewritten.
    let registry = ModuleRegistry::builtin();
    let future = Layer {
        effect_format: 2,
        ..basic_layer(json!({"exposure": 1.0}))
    };
    let error = registry
        .validate_layer(&future)
        .expect_err("an unsupported format");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        error.detail.contains("unsupported effect format 2"),
        "{}",
        error.detail
    );
    let source = source_of(2, 1, &[[10, 20, 30], [40, 50, 60]]);
    let render_error = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![future.clone()]),
    )
    .expect_err("an unsupported format never renders");
    assert_eq!(render_error.kind, ErrorKind::Incompatible);
    assert_eq!(future.payload, json!({"exposure": 1.0}), "unchanged");

    // An unknown stored field is a validation error naming the field.
    let unknown_field = registry
        .validate_layer(&basic_layer(json!({"gamma": 1.0})))
        .expect_err("an unknown stored field");
    assert_eq!(unknown_field.kind, ErrorKind::Validation);
    assert!(
        unknown_field.detail.contains("unknown basic field gamma"),
        "{}",
        unknown_field.detail
    );

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// A stack holding two Basic layers is ambiguous: the module refuses to plan against it and the
/// host refuses to compile it. Nothing is rewritten and the stack stays readable.
#[test]
fn two_basic_layers_are_refused_by_planning_and_by_rendering() {
    let registry = ModuleRegistry::builtin();
    let source = source_of(2, 1, &[[10, 20, 30], [40, 50, 60]]);
    let ambiguous = recipe(vec![exposure_layer(1.0), exposure_layer(-1.0)]);
    let error = render(&registry, &source, SnapshotId::new(), &ambiguous)
        .expect_err("an ambiguous stack never renders");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(error.detail, "ambiguous Basic layers");
    let sampled = lightwell_core::sample(&registry, &source, &ambiguous, 0, 0)
        .expect_err("an ambiguous stack never samples");
    assert_eq!(sampled.detail, "ambiguous Basic layers");
    // The layers are still readable: each row describes itself.
    let (module, _) = registry.effect(BASIC_EFFECT).expect("the Basic provider");
    for layer in &ambiguous.layers {
        assert!(
            module
                .describe_layer(&layer.effect_id, layer.effect_format, &layer.payload)
                .is_ok()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// An unavailable provider
// ---------------------------------------------------------------------------------------------

/// A registered provider wrapped as unavailable, exactly as the desktop's `--disable-module` does.
struct Disabled {
    inner: Arc<dyn ToolModule>,
    descriptor: ModuleDescriptor,
}

impl Disabled {
    fn wrap(inner: Arc<dyn ToolModule>) -> Arc<dyn ToolModule> {
        let descriptor = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..inner.descriptor().clone()
        };
        Arc::new(Self { inner, descriptor })
    }
}

impl ToolModule for Disabled {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<lightwell_core::ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(
        &self,
        input: &lightwell_core::ActionInput,
        stage: &lightwell_core::StageContext<'_>,
    ) -> Result<lightwell_core::ActionPlan, Error> {
        self.inner.plan(input, stage)
    }
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error> {
        self.inner.describe_layer(effect_id, format, payload)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: lightwell_core::Stage,
    ) -> Result<lightwell_core::Processing, Error> {
        self.inner.compile(effect_id, format, payload, stage)
    }
}

/// With the Basic provider disabled, a stack holding a Basic layer reports the unavailable effect
/// instead of rendering without it, and stays completely readable.
#[test]
fn a_disabled_basic_provider_reports_its_layers_instead_of_rendering_without_them() {
    let path = catalog("unavailable");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "expose"),
            "set-basic",
            json!({"exposure": 1.0}),
        )
        .expect("an exposure");
    let stored = layers(&service, &asset);
    drop(service);

    let mut registry = ModuleRegistry::new();
    registry
        .register(Arc::new(lightwell_core::PixelModule::new()))
        .expect("the pixel module");
    registry
        .register(Disabled::wrap(Arc::new(lightwell_core::BasicModule::new())))
        .expect("the disabled Basic module");
    registry
        .register(Arc::new(lightwell_core::TransformModule::new()))
        .expect("the transform module");
    registry
        .register(Arc::new(lightwell_core::CropModule::new()))
        .expect("the crop module");
    let service = EditorService::open_with(&path, Arc::new(registry)).expect("the same catalog");

    let error = service
        .render_current(&asset)
        .expect_err("an unavailable effect never renders");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        error
            .detail
            .starts_with(&format!("unavailable effect {BASIC_EFFECT}")),
        "{}",
        error.detail
    );

    // The stack is unchanged and still readable, with the row reporting why.
    assert_eq!(layers(&service, &asset), stored);
    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert!(!row.available);
    assert_eq!(row.summary, "unavailable: disabled by --disable-module");
    assert!(service.history(&asset, None, 50).is_ok());

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

// ---------------------------------------------------------------------------------------------
// Discovery and the draft lifecycle through the JSON methods
// ---------------------------------------------------------------------------------------------

fn call(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    method: &str,
    params: Value,
) -> Value {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered");
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

/// Import an original and adopt it: the owner acknowledges with a bounded source job, the worker
/// prepares the file, and `job.adopt` hands the asset state back to this client.
fn import_asset(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    path: &std::path::Path,
) -> Value {
    let queued = call(owner, client, "catalog.import", json!({"path": path}));
    let job_id = queued["job_id"]
        .as_str()
        .expect("an import job id")
        .to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = call(owner, client, "job.status", json!({"job_id": job_id}));
        match status["state"].as_str() {
            Some("ready") => break,
            Some("queued" | "preparing") => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the import never became ready: {status}"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            other => panic!("unexpected import job {other:?}: {status}"),
        }
    }
    call(owner, client, "job.adopt", json!({"job_id": job_id}))["asset"].clone()
}

fn call_error(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    method: &str,
    params: Value,
) -> lightwell_core::ApiFailure {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered")
        .error
        .expect("an error")
}

/// An independent JSON client discovers the module, its patch action and its reset, and drives one
/// slider gesture as a draft: begin, set, sample the draft, commit once.
#[test]
fn an_independent_client_discovers_basic_and_drives_one_gesture_as_a_draft() {
    let path = catalog("draft");
    let (owner, join) = OwnerHandle::start(&path).expect("the owner loop");
    let client = owner.register();

    // Discovery: the module and both generated methods.
    let modules = call(&owner, client, "module.list", json!({}));
    let basic = modules["modules"]
        .as_array()
        .expect("the modules")
        .iter()
        .find(|module| module["id"] == json!("lightwell.basic"))
        .expect("the Basic module")
        .clone();
    assert_eq!(basic["title"], json!("Basic"));
    assert_eq!(
        basic["hint"],
        json!("Exposure, tone, white balance and colour")
    );
    assert_eq!(basic["developer"], json!(false));
    assert_eq!(basic["effects"][0]["id"], json!(BASIC_EFFECT));
    assert_eq!(basic["effects"][0]["format"], json!(1));
    assert_eq!(basic["effects"][0]["stage"], json!("color"));
    assert_eq!(
        basic["reset"],
        json!({"action": "reset-basic", "preset": {}})
    );
    assert_eq!(
        basic["controls"],
        json!([
            {
                "kind": "group",
                "label": "White balance",
                "reset": {"action": "set-basic", "preset": {"temperature": 0.0, "tint": 0.0}},
                "controls": [
                    {"kind": "number", "action": "set-basic", "parameter": "temperature", "label": "Temperature", "rail": "temperature"},
                    {"kind": "number", "action": "set-basic", "parameter": "tint", "label": "Tint", "rail": "tint"},
                    {"kind": "picker", "label": "Neutral picker"},
                ],
            },
            {
                "kind": "group",
                "label": "Tone",
                "reset": {"action": "set-basic", "preset": {
                    "exposure": 0.0,
                    "contrast": 0.0,
                    "highlights": 0.0,
                    "shadows": 0.0,
                    "whites": 0.0,
                    "blacks": 0.0,
                }},
                "controls": [
                    {"kind": "number", "action": "set-basic", "parameter": "exposure", "label": "Exposure"},
                    {"kind": "number", "action": "set-basic", "parameter": "contrast", "label": "Contrast"},
                    {"kind": "number", "action": "set-basic", "parameter": "highlights", "label": "Highlights"},
                    {"kind": "number", "action": "set-basic", "parameter": "shadows", "label": "Shadows"},
                    {"kind": "number", "action": "set-basic", "parameter": "whites", "label": "Whites"},
                    {"kind": "number", "action": "set-basic", "parameter": "blacks", "label": "Blacks"},
                ],
            },
            {
                "kind": "group",
                "label": "Colour",
                "reset": {"action": "set-basic", "preset": {"vibrance": 0.0, "saturation": 0.0}},
                "controls": [
                    {"kind": "number", "action": "set-basic", "parameter": "vibrance", "label": "Vibrance"},
                    {"kind": "number", "action": "set-basic", "parameter": "saturation", "label": "Saturation"},
                ],
            },
        ]),
        "White balance with its two sliders and the neutral picker, then Tone, then Colour, each \
         with its own group reset"
    );

    let schema = call(&owner, client, "schema.list", json!({}));
    let set = schema["methods"]["edit.set-basic"].clone();
    assert_eq!(set["patch"], json!(true));
    assert_eq!(set["mutates"], json!(true));
    assert_eq!(set["required"], json!(["asset_id", "mutation"]));
    assert!(
        set["optional"]["exposure"].is_string(),
        "every patch field is optional: {set}"
    );
    assert_eq!(
        set["parameters"]
            .as_array()
            .expect("the declared parameters")
            .iter()
            .map(|parameter| parameter["name"].as_str().expect("a name"))
            .collect::<Vec<_>>(),
        [
            "temperature",
            "tint",
            "exposure",
            "contrast",
            "highlights",
            "shadows",
            "whites",
            "blacks",
            "vibrance",
            "saturation",
        ],
        "the schema lists every implemented field in the payload's declared order"
    );
    let exposure = set["parameters"][2].clone();
    assert_eq!(exposure["name"], json!("exposure"));
    assert_eq!(exposure["kind"], json!("number"));
    assert_eq!(exposure["min"], json!(-5.0));
    assert_eq!(exposure["max"], json!(5.0));
    assert_eq!(exposure["required"], json!(false));
    assert_eq!(exposure["default"], json!(0.0));
    assert_eq!(exposure["unit"], json!("EV"));
    assert_eq!(exposure["step"], json!(0.01));
    assert_eq!(exposure["precision"], json!(2));
    let reset = schema["methods"]["edit.reset-basic"].clone();
    assert_eq!(reset["patch"], json!(false));
    assert_eq!(reset["required"], json!(["asset_id", "mutation"]));
    assert_eq!(reset["parameters"], json!([]));

    let asset = import_asset(&owner, client, &jpeg())["asset"]["id"].clone();

    // One gesture: pointer down begins the draft, moves set it, release commits once.
    let begun = call(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    );
    let draft_id = begun["draft_id"].clone();
    assert_eq!(begun["base_revision"], json!(0));
    assert_eq!(begun["fields"], json!({}));
    assert_eq!(begun["conflicted"], json!(false));
    for (step, value) in [(1, 0.25), (2, 0.75), (3, 1.5)] {
        let set = call(
            &owner,
            client,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"exposure": value}}),
        );
        assert_eq!(set["draft_revision"], json!(step));
        assert_eq!(set["fields"], json!({"exposure": value}));
    }
    // A field outside the declared range is refused and changes nothing.
    let refused = call_error(
        &owner,
        client,
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"exposure": 9.0}}),
    );
    assert_eq!(refused.code, "validation");
    assert_eq!(
        call(&owner, client, "draft.read", json!({"draft_id": draft_id}))["fields"],
        json!({"exposure": 1.5})
    );

    // Nothing is committed while the draft is open, and the drafted sample is what a commit gives.
    assert_eq!(
        call(&owner, client, "asset.state", json!({"asset_id": asset}))["revision"],
        json!(0)
    );
    let drafted = call(
        &owner,
        client,
        "render.sample",
        json!({"asset_id": asset, "x": 4, "y": 4, "draft_id": draft_id}),
    );
    assert_eq!(
        drafted["draft"],
        json!({"draft_id": draft_id, "draft_revision": 3})
    );
    let stored = call(
        &owner,
        client,
        "render.sample",
        json!({"asset_id": asset, "x": 4, "y": 4}),
    );
    assert_ne!(stored["rgba"], drafted["rgba"]);

    let committed = call(
        &owner,
        client,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": {"expected_revision": 0, "request_id": "gesture", "actor": "test"}}),
    );
    assert_eq!(committed["outcome"], json!("applied"));
    assert_eq!(committed["revision"], json!(1));
    let entry = call(
        &owner,
        client,
        "history.inspect",
        json!({"asset_id": asset, "entry_id": committed["current_entry_id"]}),
    );
    assert_eq!(entry["label"], json!("Exposure +1.50 EV"));
    assert_eq!(entry["parameters"], json!({"exposure": 1.5}));
    assert_eq!(entry["action_id"], json!("set-basic"));
    assert_eq!(
        call(&owner, client, "session.state", json!({}))["draft"],
        json!(null),
        "committing ends the draft"
    );
    assert_eq!(
        call(
            &owner,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": 4, "y": 4})
        )["rgba"],
        drafted["rgba"],
        "the committed stack produces exactly what the draft previewed"
    );
    let history = call(
        &owner,
        client,
        "history.list",
        json!({"asset_id": asset, "limit": 50}),
    );
    assert_eq!(
        history["entries"].as_array().expect("entries").len(),
        2,
        "the import and one entry for the whole gesture"
    );

    // A gesture that returns to its start commits no entry at all.
    let begun = call(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    );
    let draft_id = begun["draft_id"].clone();
    call(
        &owner,
        client,
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"exposure": 3.0}}),
    );
    call(
        &owner,
        client,
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"exposure": 1.5}}),
    );
    let unchanged = call(
        &owner,
        client,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": {"expected_revision": 1, "request_id": "return", "actor": "test"}}),
    );
    assert_eq!(unchanged["outcome"], json!("no-op"));
    assert_eq!(
        call(
            &owner,
            client,
            "history.list",
            json!({"asset_id": asset, "limit": 50})
        )["entries"]
            .as_array()
            .expect("entries")
            .len(),
        2,
        "a gesture that returned to its start added no row"
    );

    owner.disconnect(client);
    owner.stop();
    join.join().expect("the owner loop ends");
    fs::remove_file(path).expect("the catalog is removed");
}
