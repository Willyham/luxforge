//! The Basic module's Exposure parameter end to end: the compiled colour unit against the
//! independent f64 corpus, mixed geometry/replacement order, and the placement of the one Basic
//! layer before the geometry tail.
//!
//! Numerical rule, from the design's numerical contract and `fixtures/basic/README.md`: the band
//! around a code threshold within which one code of difference is permitted is
//! `1e-6 + 1e-6 · |threshold|`, because production decodes and multiplies in f32.

use super::{basic_layer, mutation};
use luxforge_core::{
    BASIC_EFFECT, CROP_EFFECT, EditorService, Layer, ModuleRegistry, MutationOutcome,
    ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, SnapshotId, SourceImage, Transform,
};
use luxforge_reference::{RefOp, evaluate_pixel, exposure, srgb};
use luxforge_testbase::paths;
use luxforge_testkit::fixtures::render;
use luxforge_testkit::fixtures::{self, recipe, source_of};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, fs};

/// The Exposure contract's relative band around a code threshold.
const CODE_BAND: f64 = 1e-6;

fn exposure_layer(ev: f64) -> Layer {
    basic_layer(json!({ "exposure": ev }))
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
    let raw = fs::read_to_string(paths::fixture("basic/exposure-cases.json")).expect("the corpus");
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
            // The Rust reference agrees with the corpus (proved in `studies/exposure.rs`); this
            // asserts against the committed file itself.
            assert_eq!(
                evaluate_pixel(case.input, &[RefOp::Exposure(ev)]),
                case.expected,
                "the reference disagrees with the corpus at {:?} {ev} EV",
                case.input
            );
            for (channel, input) in case.input.iter().enumerate() {
                let linear = exposure(srgb::decode(*input), ev);
                fixtures::assert_code_near_threshold(
                    pixel[channel],
                    case.expected[channel],
                    linear,
                    CODE_BAND,
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
    let raw =
        fs::read_to_string(paths::fixture("basic/mixed-order.json")).expect("the mixed-order file");
    let file: MixedOrder = serde_json::from_str(&raw).expect("mixed-order cases");
    let source = SourceImage {
        width: file.width,
        height: file.height,
        rgba: file.base_rgba.clone().into(),
        fingerprint: "sha256:mixed-order-fixture".into(),
        orientation: 1,
        capture: Default::default(),
    };
    let registry = ModuleRegistry::developer();
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
/// colour insertion index before that tail, and a later set updates it there in place with every
/// other layer untouched. What a set that changes nothing writes and what a reset keeps are the
/// conformance suite's.
#[test]
fn the_first_set_places_one_layer_before_the_geometry_tail_and_later_sets_update_it() {
    let path = paths::temp_catalog("basic-place");
    let mut service = EditorService::open_with(
        &path,
        std::sync::Arc::new(luxforge_core::ModuleRegistry::developer()),
    )
    .expect("a catalog");
    let asset = service.import(&paths::jpeg()).expect("an import").asset.id;

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
    let before = layers(&service, &asset);
    assert_eq!(
        before
            .iter()
            .map(|layer| layer.effect_id.as_str())
            .collect::<Vec<_>>(),
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
    let others: Vec<Layer> = stack
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, layer)| layer.clone())
        .collect();
    assert_eq!(
        others, before,
        "every other layer keeps its identity, its payload and its place"
    );

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

fn layers(service: &EditorService, asset: &luxforge_core::AssetId) -> Vec<Layer> {
    service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
        .layers
}
