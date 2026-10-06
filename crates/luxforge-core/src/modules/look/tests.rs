//! The look module: its descriptor and placement, `set-look`, `reset-look` and `sample-look`, the
//! stored payload's checks, its report and labels, presets, and compilation.
use super::*;
use crate::{
    BASIC_EFFECT, CURVE_EFFECT, ErrorKind, ModuleRegistry, SnapshotId, check_parameters,
    modules::{FixedStage, Stage},
};
use luxforge_reference::look::LookTone;

fn module() -> LookModule {
    LookModule::new()
}

fn raw_stage() -> FixedStage {
    FixedStage::new(Stage {
        width: 4,
        height: 4,
    })
    .of_kind(SourceTag::Raw)
}

fn input(action: &str, parameters: Value) -> ActionInput {
    module()
        .parse(action, parameters.as_object().unwrap())
        .expect("a request the module parses")
}

/// The plan of `action` over `layers` on a RAW photo, after the host's generic check.
fn plan(layers: &[Layer], action: &str, parameters: Value) -> Result<ActionPlan, Error> {
    plan_on(raw_stage(), layers, action, parameters)
}

fn plan_on(
    stage: FixedStage,
    layers: &[Layer],
    action: &str,
    parameters: Value,
) -> Result<ActionPlan, Error> {
    let module = module();
    let declared = module
        .descriptor()
        .action(action)
        .expect("a declared action");
    let checked = check_parameters(declared, &parameters)?;
    let input = module.parse(action, &checked)?;
    let registry = ModuleRegistry::builtin();
    module.plan(&input, &stage.context(layers, &registry))
}

fn standard_payload(amount: f64) -> Value {
    Payload::Standard(Resolved::standard(amount)).to_value()
}

fn look_layer(payload: Value) -> Layer {
    Layer::new(LOOK_EFFECT, payload)
}

fn committed(plan: ActionPlan) -> Value {
    match plan {
        ActionPlan::Commit(layer) => {
            assert_eq!(layer.effect_id, LOOK_EFFECT);
            layer.payload
        }
        other => panic!("a commit, not {other:?}"),
    }
}

fn updated(plan: ActionPlan, layer: &Layer) -> Value {
    match plan {
        ActionPlan::Update(update) => {
            assert_eq!(update.id, layer.id);
            update.payload
        }
        other => panic!("an update, not {other:?}"),
    }
}

fn units(payload: &Value, at: CompileStage) -> usize {
    match module()
        .compile(LOOK_EFFECT, EFFECT_FORMAT, payload, at)
        .expect("the look compiles")
    {
        Processing::Color(operation) => operation.len(),
        other => panic!("a colour operation, not {other:?}"),
    }
}

fn stage_4() -> CompileStage {
    CompileStage::exact(Stage {
        width: 4,
        height: 4,
    })
}

/// The module is `luxforge.look`, one single colour-stage effect at order 3 on RAW sources only,
/// not maskable, registered between Basic and the Tone curve; neither action is presettable and
/// the module's reset is `reset-look`.
#[test]
fn the_descriptor_declares_a_single_raw_colour_effect_at_order_three() {
    let module = module();
    let descriptor = module.descriptor();
    assert_eq!(descriptor.id, "luxforge.look");
    assert_eq!(descriptor.title, "Look");
    let [effect] = descriptor.effects.as_slice() else {
        panic!("one effect");
    };
    assert_eq!(effect.id, "luxforge.look.look");
    assert_eq!(effect.stage, EffectStage::Color);
    assert_eq!(effect.order, 3);
    assert!(effect.single && !effect.maskable && !effect.artifacts);
    assert_eq!(effect.sources, [SourceTag::Raw]);
    assert!(descriptor.applies_to(SourceTag::Raw));
    assert!(!descriptor.applies_to(SourceTag::Jpeg));
    assert!(descriptor.check_applies_to(SourceTag::Jpeg).is_err());
    let set = descriptor.action(SET_LOOK).unwrap();
    assert!(set.patch && !set.preset);
    assert!(!descriptor.action(RESET_LOOK).unwrap().preset);
    assert_eq!(descriptor.reset.as_ref().unwrap().action, RESET_LOOK);
    assert_eq!(descriptor.queries[0].id, SAMPLE_QUERY);

    let registry = ModuleRegistry::builtin();
    let ids: Vec<String> = registry
        .descriptors()
        .iter()
        .map(|module| module.id.clone())
        .collect();
    let at = |id: &str| ids.iter().position(|one| one == id).unwrap();
    assert_eq!(at("luxforge.look"), at("luxforge.basic") + 1);
    assert_eq!(at("luxforge.curve"), at("luxforge.look") + 1);
}

/// The first `set-look` on a RAW photo without a look layer commits the layer: Standard at 100,
/// or at the amount sent. Neutral there is a no-op, as is an empty patch; an amount alone is
/// refused, since a photo without a look is Neutral. `reset-look` adds Standard at 100.
#[test]
fn a_first_set_on_a_photo_without_a_look_commits_one() {
    assert_eq!(
        committed(plan(&[], SET_LOOK, json!({"look": "standard"})).unwrap()),
        standard_payload(100.0)
    );
    assert_eq!(
        committed(plan(&[], SET_LOOK, json!({"look": "standard", "amount": 80})).unwrap()),
        standard_payload(80.0)
    );
    assert_eq!(
        committed(plan(&[], RESET_LOOK, json!({})).unwrap()),
        standard_payload(100.0)
    );
    assert_eq!(
        plan(&[], SET_LOOK, json!({"look": "neutral"})).unwrap(),
        ActionPlan::NoOp
    );
    assert_eq!(plan(&[], SET_LOOK, json!({})).unwrap(), ActionPlan::NoOp);
    let error = plan(&[], SET_LOOK, json!({"amount": 80})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "a Neutral look has no amount; choose look standard first"
    );
}

/// On a stored look: an amount alone changes only the amount and keeps the stored knots; `look:
/// standard` writes the current Standard's knots and keeps the amount; `look: neutral` writes the
/// Neutral payload; the look already stored is a no-op; `reset-look` returns to Standard at 100,
/// keeping the layer.
#[test]
fn set_look_patches_the_stored_look_in_place() {
    let standard = look_layer(standard_payload(100.0));
    let at_80 = updated(
        plan(
            std::slice::from_ref(&standard),
            SET_LOOK,
            json!({"amount": 80}),
        )
        .unwrap(),
        &standard,
    );
    assert_eq!(at_80, standard_payload(80.0));
    assert_eq!(
        plan(
            std::slice::from_ref(&standard),
            SET_LOOK,
            json!({"look": "standard"})
        )
        .unwrap(),
        ActionPlan::NoOp
    );
    assert_eq!(
        plan(
            std::slice::from_ref(&standard),
            SET_LOOK,
            json!({"amount": 100.0})
        )
        .unwrap(),
        ActionPlan::NoOp
    );
    assert_eq!(
        updated(
            plan(
                std::slice::from_ref(&standard),
                SET_LOOK,
                json!({"look": "neutral"})
            )
            .unwrap(),
            &standard
        ),
        json!({"look": "neutral"})
    );

    // A look written by an earlier Standard keeps its knots under an amount, and takes the
    // current Standard's under `look: standard`, at the amount in force.
    let earlier = look_layer(json!({
        "look": "standard", "amount": 60, "tone": [[0, 0.02], [0.5, 0.6], [1.4, 1]],
        "chroma": 1.1, "knee": 0.85, "fit": null
    }));
    let amount = updated(
        plan(
            std::slice::from_ref(&earlier),
            SET_LOOK,
            json!({"amount": 90}),
        )
        .unwrap(),
        &earlier,
    );
    assert_eq!(amount["tone"], json!([[0.0, 0.02], [0.5, 0.6], [1.4, 1.0]]));
    assert_eq!(amount["amount"], json!(90.0));
    assert_eq!(amount["chroma"], json!(1.1));
    let current = updated(
        plan(
            std::slice::from_ref(&earlier),
            SET_LOOK,
            json!({"look": "standard"}),
        )
        .unwrap(),
        &earlier,
    );
    assert_eq!(current, standard_payload(60.0));
    assert_eq!(
        updated(
            plan(std::slice::from_ref(&earlier), RESET_LOOK, json!({})).unwrap(),
            &earlier
        ),
        standard_payload(100.0)
    );
    assert_eq!(
        plan(std::slice::from_ref(&standard), RESET_LOOK, json!({})).unwrap(),
        ActionPlan::NoOp
    );

    // From Neutral, `look: standard` starts at 100; an amount alone is refused.
    let neutral = look_layer(json!({"look": "neutral"}));
    assert_eq!(
        updated(
            plan(
                std::slice::from_ref(&neutral),
                SET_LOOK,
                json!({"look": "standard"})
            )
            .unwrap(),
            &neutral
        ),
        standard_payload(100.0)
    );
    assert_eq!(
        plan(
            std::slice::from_ref(&neutral),
            SET_LOOK,
            json!({"look": "neutral"})
        )
        .unwrap(),
        ActionPlan::NoOp
    );
    let error = plan(
        std::slice::from_ref(&neutral),
        SET_LOOK,
        json!({"amount": 50}),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
}

/// What names no look is refused: an amount beside `look: neutral`, an amount outside 0..=200, an
/// unknown look, and Match camera, which the schema does not offer and the module refuses with a
/// pointer to `edit.match-camera-look` if it is reached. A JPEG photo is refused by the plan as by
/// the host's kind check.
#[test]
fn set_look_refuses_what_names_no_look() {
    let error = plan(&[], SET_LOOK, json!({"look": "neutral", "amount": 50})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "a Neutral look has no amount; send look standard with an amount"
    );
    for (parameters, detail) in [
        (json!({"amount": 201}), "amount"),
        (json!({"amount": -1}), "amount"),
        (
            json!({"look": "camera"}),
            "parameter look must be one of standard, neutral",
        ),
        (
            json!({"look": "vivid"}),
            "parameter look must be one of standard, neutral",
        ),
    ] {
        let error = plan(&[], SET_LOOK, parameters.clone()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation, "{parameters}");
        assert!(error.detail.contains(detail), "{parameters}: {error}");
    }
    let error = module()
        .parse(SET_LOOK, json!({"look": "camera"}).as_object().unwrap())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("edit.match-camera-look"), "{error}");
    assert!(error.detail.contains("not available yet"), "{error}");

    let jpeg = raw_stage().of_kind(SourceTag::Jpeg);
    let error = plan_on(jpeg, &[], SET_LOOK, json!({"look": "standard"})).unwrap_err();
    assert_eq!(error.detail, "the Look applies to RAW photos only");
    let error = plan_on(jpeg, &[], RESET_LOOK, json!({})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
}

/// The stored form is checked strictly and refused by name, never rewritten: an unknown or
/// missing field, Match camera, every knot rule, a chroma gain at or below 0, a knee outside
/// `(0, 1)`, an amount outside 0..=200, a fit that is not `null`, another effect and another
/// format.
#[test]
fn validate_payload_refuses_every_malformed_stored_look() {
    let module = module();
    let check = |payload: Value| module.validate_payload(LOOK_EFFECT, EFFECT_FORMAT, &payload);
    assert!(check(standard_payload(100.0)).is_ok());
    assert!(check(standard_payload(0.0)).is_ok());
    assert!(check(standard_payload(200.0)).is_ok());
    assert!(check(json!({"look": "neutral"})).is_ok());
    let valid = || standard_payload(100.0).as_object().unwrap().clone();
    let with = |name: &str, value: Value| {
        let mut payload = valid();
        payload.insert(name.into(), value);
        Value::Object(payload)
    };
    let without = |name: &str| {
        let mut payload = valid();
        payload.remove(name);
        Value::Object(payload)
    };
    let many: Vec<[f64; 2]> = (0..25).map(|k| [f64::from(k) / 20.0, 0.0]).collect();
    for (payload, detail) in [
        (json!([]), "Look payload must be a JSON object"),
        (json!({}), "Look payload lacks look"),
        (json!({"look": 1}), "Look payload's look must be a string"),
        (json!({"look": "vivid"}), "unknown look vivid"),
        (json!({"look": "camera"}), "edit.match-camera-look"),
        (
            json!({"look": "neutral", "amount": 100}),
            "unknown field amount in a neutral Look payload",
        ),
        (
            with("extra", json!(1)),
            "unknown field extra in a standard Look payload",
        ),
        (without("tone"), "Look payload lacks tone"),
        (without("fit"), "Look payload lacks fit"),
        (without("knee"), "Look payload lacks knee"),
        (with("amount", json!(200.5)), "Look amount 200.5 is outside"),
        (with("amount", json!(-0.5)), "Look amount -0.5 is outside"),
        (
            with("amount", json!("100")),
            "Look amount must be a finite number",
        ),
        (with("chroma", json!(0)), "Look chroma 0 is not above 0"),
        (
            with("chroma", json!(-1.2)),
            "Look chroma -1.2 is not above 0",
        ),
        (with("knee", json!(0)), "Look knee 0 is outside (0, 1)"),
        (with("knee", json!(1)), "Look knee 1 is outside (0, 1)"),
        (with("fit", json!({})), "Look fit must be null"),
        (
            with("tone", json!([[0, 0]])),
            "Look tone holds 2 to 24 knots, not 1",
        ),
        (
            with("tone", json!(many)),
            "Look tone holds 2 to 24 knots, not 25",
        ),
        (
            with("tone", json!("curve")),
            "Look tone must be a list of knots",
        ),
        (
            with("tone", json!([[0, 0], [1]])),
            "Look tone knot 1 is not two finite numbers",
        ),
        (
            with("tone", json!([[0.1, 0], [1, 1]])),
            "Look tone's first knot is not at x = 0",
        ),
        (
            with("tone", json!([[0, -0.1], [1, 1]])),
            "Look tone's first knot's y is outside [0, 1]",
        ),
        (
            with("tone", json!([[0, 0], [1, 1.01]])),
            "Look tone's last knot's y is above 1",
        ),
        (
            with("tone", json!([[0, 0], [0.5, 0.5], [0.5, 0.6], [1, 1]])),
            "Look tone knot 2's x does not increase",
        ),
        (
            with("tone", json!([[0, 0], [0.5, 0.6], [1, 0.5]])),
            "Look tone knot 2's y decreases",
        ),
    ] {
        let error = check(payload.clone()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation, "{payload}");
        assert!(error.detail.contains(detail), "{payload}: {error}");
    }
    let error = module
        .validate_payload(LOOK_EFFECT, EFFECT_FORMAT + 1, &standard_payload(100.0))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        module
            .validate_payload(CURVE_EFFECT, EFFECT_FORMAT, &standard_payload(100.0))
            .is_err()
    );
}

/// The row reads `Look Standard` (with its amount off 100) or `Look Neutral` and reports the
/// look, amount, chroma, knot count and fit; it is neutral exactly for the current Standard at
/// 100 and for Neutral. A preset captures nothing of it.
#[test]
fn describe_reports_the_look_and_neutral_for_the_starting_looks() {
    let module = module();
    let describe = |payload: Value| {
        module
            .describe(LOOK_EFFECT, EFFECT_FORMAT, &payload)
            .unwrap()
    };
    let standard = describe(standard_payload(100.0));
    assert_eq!(standard.summary, "Look Standard");
    assert!(standard.neutral);
    assert_eq!(
        Value::Object(standard.values),
        json!({"look": "standard", "amount": 100.0, "chroma": 1.2, "knots": 24, "fit": null})
    );
    let at_80 = describe(standard_payload(80.0));
    assert_eq!(at_80.summary, "Look Standard, amount 80");
    assert!(!at_80.neutral);
    let neutral = describe(json!({"look": "neutral"}));
    assert_eq!(neutral.summary, "Look Neutral");
    assert!(neutral.neutral);
    assert_eq!(Value::Object(neutral.values), json!({"look": "neutral"}));
    let mut retuned = standard_payload(100.0);
    retuned["chroma"] = json!(1.25);
    assert!(!describe(retuned.clone()).neutral);
    assert_eq!(describe(retuned).summary, "Look Standard");

    for payload in [standard_payload(80.0), json!({"look": "neutral"})] {
        assert!(
            module
                .settings(LOOK_EFFECT, EFFECT_FORMAT, &payload)
                .unwrap()
                .is_empty()
        );
    }
    // A preset can neither capture nor apply the look: the preset service resolves its actions
    // through the presettable patch lookup, which refuses `set-look`.
    let registry = ModuleRegistry::builtin();
    let error = registry.patch_action(SET_LOOK).err().unwrap();
    assert_eq!(error.detail, "set-look is not presettable");
    let error = registry.patch_action(RESET_LOOK).err().unwrap();
    assert_eq!(error.detail, "reset-look is not a field-patch action");
}

/// History reads `Look Standard`, `Look Neutral`, `Look amount 80` and `Reset Look`.
#[test]
fn labels_name_the_look_or_its_amount() {
    let module = module();
    let label = |action: &str, parameters: Value| {
        module.label(
            module.descriptor().action(action).unwrap(),
            &input(action, parameters),
        )
    };
    assert_eq!(
        label(SET_LOOK, json!({"look": "standard"})),
        "Look Standard"
    );
    assert_eq!(
        label(SET_LOOK, json!({"look": "standard", "amount": 120})),
        "Look Standard"
    );
    assert_eq!(label(SET_LOOK, json!({"look": "neutral"})), "Look Neutral");
    assert_eq!(label(SET_LOOK, json!({"amount": 80})), "Look amount 80");
    assert_eq!(label(SET_LOOK, json!({"amount": 0})), "Look amount 0");
    assert_eq!(label(RESET_LOOK, json!({})), "Reset Look");
}

/// The query answers 257 `[x, T(x)]` samples of the stored curve over `[0, x_max]`, equal to the
/// independent reference, with the chroma gain and amount; a photo whose look is Neutral, or that
/// has none, answers the identity over `[0, 1]` and a gain of 1. No pixel is read: the fixed stage
/// has none.
#[test]
fn sample_look_answers_the_stored_curve_and_chroma() {
    let registry = ModuleRegistry::builtin();
    let stage = raw_stage();
    let sample = |layers: &[Layer]| {
        module()
            .query(SAMPLE_QUERY, &Map::new(), &stage.context(layers, &registry))
            .unwrap()
    };
    let answer = sample(&[look_layer(standard_payload(80.0))]);
    let points = answer["points"].as_array().unwrap();
    assert_eq!(points.len(), SAMPLE_SEGMENTS + 1);
    let reference = LookTone::new(&STANDARD_KNOTS);
    let x_max = STANDARD_KNOTS[STANDARD_KNOTS.len() - 1][0];
    for (index, pair) in points.iter().enumerate() {
        let (x, y) = (pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap());
        assert_eq!(x, x_max * index as f64 / 256.0);
        let expected = reference.value(x);
        assert!((y - expected).abs() <= 1e-12, "{x}: {y} against {expected}");
    }
    assert_eq!(points[0], json!([0.0, 0.0]));
    assert_eq!(points[256], json!([x_max, 1.0]));
    assert_eq!(answer["chroma"], json!(1.2));
    assert_eq!(answer["amount"], json!(80.0));
    assert_eq!(answer["look"], json!("standard"));

    for layers in [vec![], vec![look_layer(json!({"look": "neutral"}))]] {
        let answer = sample(&layers);
        assert_eq!(answer["look"], json!("neutral"));
        assert_eq!(answer["chroma"], json!(1.0));
        let points = answer["points"].as_array().unwrap();
        assert_eq!(points.len(), SAMPLE_SEGMENTS + 1);
        assert!(points.iter().all(|pair| pair[0] == pair[1]));
    }
    let error = module()
        .query("sample-other", &Map::new(), &stage.context(&[], &registry))
        .unwrap_err();
    assert_eq!(error.detail, "unknown query sample-other");
}

/// Neutral and amount 0 compile to no units, the identity path; every other look to one. In the
/// GPU shape every look is one unit, so an Amount drag through 0 keeps one program sequence.
#[test]
fn neutral_and_amount_zero_compile_to_nothing_outside_the_gpu_shape() {
    let shaped = stage_4().shaped(true);
    for (payload, cpu) in [
        (json!({"look": "neutral"}), 0),
        (standard_payload(0.0), 0),
        (standard_payload(0.5), 1),
        (standard_payload(100.0), 1),
        (standard_payload(200.0), 1),
    ] {
        assert_eq!(units(&payload, stage_4()), cpu, "{payload}");
        assert_eq!(units(&payload, shaped), 1, "{payload}");
    }
    let error = module()
        .compile(
            LOOK_EFFECT,
            EFFECT_FORMAT,
            &json!({"look": "vivid"}),
            stage_4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
}

/// Basic, the look and the Tone curve land in that order whatever order they are added in (orders
/// 0, 3 and 5 in the colour stage), and the stack renders as Basic's units, then the look's, then
/// the curve's, applied in that order to each pixel and quantized once.
#[test]
fn basic_the_look_and_the_tone_curve_render_in_that_order_whatever_order_they_were_added() {
    let registry = ModuleRegistry::builtin();
    let basic = || Layer::new(BASIC_EFFECT, json!({"exposure": 0.7, "saturation": 20}));
    let look = || look_layer(standard_payload(140.0));
    let curve = || {
        Layer::new(
            CURVE_EFFECT,
            json!({"luminance": [[0, 0.02], [0.5, 0.55], [1, 0.97]]}),
        )
    };
    let makers: [&dyn Fn() -> Layer; 3] = [&basic, &look, &curve];
    let permutations = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut placed = None;
    for permutation in permutations {
        let mut layers: Vec<Layer> = Vec::new();
        for index in permutation {
            let layer = makers[index]();
            let at = registry.insertion_index_for_target(&layers, &layer.effect_id, None, &[]);
            layers.insert(at, layer);
        }
        let effects: Vec<&str> = layers
            .iter()
            .map(|layer| layer.effect_id.as_str())
            .collect();
        assert_eq!(
            effects,
            [BASIC_EFFECT, LOOK_EFFECT, CURVE_EFFECT],
            "added as {permutation:?}"
        );
        placed.get_or_insert(layers);
    }
    let layers = placed.unwrap();

    let (width, height) = (8u32, 4u32);
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|i| {
            let k = i as f32 / (width * height) as f32;
            [
                0.02 + 1.4 * k,
                0.6 - 0.5 * k,
                0.05 + 0.3 * (k * 7.0).sin().abs(),
            ]
        })
        .collect();
    let planes: Vec<f32> = (0..3)
        .flat_map(|channel| pixels.iter().map(move |pixel| pixel[channel]))
        .collect();
    let source = crate::LinearImage::new(width, height, planes).unwrap();
    let recipe = crate::Recipe {
        layers: layers.clone(),
        ..crate::Recipe::default()
    };
    let raster = crate::render::testing::render_linear(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe,
        Default::default(),
    )
    .expect("the stack renders");

    let stage = CompileStage::exact(Stage { width, height });
    let compiled: Vec<Arc<dyn PointwiseColor>> = layers
        .iter()
        .flat_map(|layer| {
            let (provider, _) = registry.effect(&layer.effect_id).unwrap();
            match provider
                .compile(&layer.effect_id, layer.effect_format, &layer.payload, stage)
                .unwrap()
            {
                Processing::Color(operation) => operation.units().to_vec(),
                other => panic!("a colour operation, not {other:?}"),
            }
        })
        .collect();
    assert!(compiled.len() >= 3);
    for (index, pixel) in pixels.iter().enumerate() {
        let mut row = [*pixel];
        let (x, y) = (index as u32 % width, index as u32 / width);
        for unit in &compiled {
            unit.apply_row(y, x, &mut row);
        }
        let expected = crate::colour::srgb::quantize_pixel(row[0]);
        let at = index * 4;
        assert_eq!(
            &raster.rgba[at..at + 3],
            &expected,
            "pixel {index}: {pixel:?}"
        );
    }
}

fn original_on(source: SourceTag, raw_look: RawLook) -> Option<OriginalLayer> {
    let header = crate::catalog_types::HeaderMetadata::default();
    module()
        .original(&OriginalContext {
            source,
            raw: None,
            header: &header,
            preferences: crate::OriginalPreferences { raw_look },
        })
        .expect("the look never refuses an Original")
}

#[test]
fn a_new_raw_photo_starts_from_the_standard_look_unless_neutral_is_preferred() {
    let layer = original_on(SourceTag::Raw, RawLook::Standard).expect("a Standard look");
    assert_eq!(layer.effect_id, LOOK_EFFECT);
    assert_eq!(layer.payload, standard_payload(100.0));
    module()
        .validate_payload(LOOK_EFFECT, EFFECT_FORMAT, &layer.payload)
        .expect("the starting look is a valid stored look");
    let report = module()
        .describe(LOOK_EFFECT, EFFECT_FORMAT, &layer.payload)
        .unwrap();
    assert!(report.neutral, "the starting Standard look is not an edit");
    assert_eq!(original_on(SourceTag::Raw, RawLook::Neutral), None);
    assert_eq!(original_on(SourceTag::Jpeg, RawLook::Standard), None);
}
