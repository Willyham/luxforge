use super::*;
use crate::{CURVE_EFFECT, EFFECT_FORMAT, MaskId, Stage};

fn stage() -> CompileStage {
    CompileStage::exact(Stage {
        width: 32,
        height: 32,
    })
}

/// The Standard look a new RAW starts with, at `amount`.
fn look(registry: &ModuleRegistry, amount: f64) -> Layer {
    let mut payload = registry
        .module("luxforge.look")
        .unwrap()
        .original(&crate::OriginalContext {
            source: crate::SourceTag::Raw,
            raw: None,
            header: &crate::catalog_types::HeaderMetadata::default(),
            preferences: crate::OriginalPreferences::default(),
        })
        .unwrap()
        .unwrap()
        .payload;
    payload["amount"] = json!(amount);
    Layer::new(LOOK_EFFECT, payload)
}

/// A row through `operations`, in order.
fn through(operations: &[ColorOperation]) -> Vec<[f32; 3]> {
    let mut row: Vec<[f32; 3]> = (0..64)
        .map(|i| [i as f32 / 40., i as f32 / 50., i as f32 / 70.])
        .collect();
    for operation in operations {
        for unit in operation.units() {
            unit.apply_row(0, 0, &mut row);
        }
    }
    row
}

fn compiled(registry: &ModuleRegistry, layer: &Layer) -> ColorOperation {
    let (module, _) = registry.effect(&layer.effect_id).unwrap();
    let Processing::Color(operation) = module
        .compile(&layer.effect_id, EFFECT_FORMAT, &layer.payload, stage())
        .unwrap()
    else {
        panic!("{} is pointwise", layer.effect_id)
    };
    operation
}

/// Basic keeps its stored white balance and takes the candidate's eight fields; the global Look
/// after it follows, and a masked one or any other layer is left out and reported.
#[test]
fn the_model_is_stored_basic_at_the_candidate_then_the_global_look_after_it() {
    let registry = ModuleRegistry::builtin();
    let look = look(&registry, 80.);
    let mut masked = look.clone();
    masked.mask = Some(MaskId::new());
    let stored = Layer::new(
        BASIC_EFFECT,
        json!({"temperature": 12, "exposure": 1.5, "shadows": 30}),
    );
    let curve = Layer::new(CURVE_EFFECT, json!({"rgb": [[0, 0.1], [1, 1]]}));
    let layers = [stored, curve, masked, look.clone()];
    let model = forward_model(&registry, &layers, 0, stage()).unwrap();
    let values = AutoToneValues {
        exposure: 0.25,
        contrast: 10.,
        ..Default::default()
    };
    let mut payload = values.fields();
    payload.insert("temperature".into(), json!(12));
    let expected = [
        compiled(&registry, &Layer::new(BASIC_EFFECT, Value::Object(payload))),
        compiled(&registry, &look),
    ];
    assert_eq!(through(&model.compile(values).unwrap()), through(&expected));
    assert_eq!(model.search(), ExposureSearch::Monotone);
    assert_eq!(
        model.explanation(),
        json!({"used": [BASIC_EFFECT, LOOK_EFFECT], "omitted": [
            {"effect": CURVE_EFFECT, "masked": false, "count": 1},
            {"effect": LOOK_EFFECT, "masked": true, "count": 1},
        ]})
    );
}

/// A Look before Basic is part of the input Auto analyses, so the model does not apply it again;
/// without a Basic layer the model is the one a commit would insert, at the neutral payload.
#[test]
fn a_look_before_basic_is_input_not_model() {
    let registry = ModuleRegistry::builtin();
    let layers = [look(&registry, 200.), Layer::new(BASIC_EFFECT, json!({}))];
    let model = forward_model(&registry, &layers, 1, stage()).unwrap();
    let values = AutoToneValues {
        exposure: -0.5,
        ..Default::default()
    };
    assert_eq!(
        through(&model.compile(values).unwrap()),
        through(&[compiled(
            &registry,
            &Layer::new(BASIC_EFFECT, Value::Object(values.fields()))
        )])
    );
    assert_eq!(model.search(), ExposureSearch::Monotone);
    assert_eq!(model.explanation()["used"], json!([BASIC_EFFECT]));
    let inserted = forward_model(&registry, &layers[..1], 1, stage()).unwrap();
    assert_eq!(
        through(&inserted.compile(values).unwrap()),
        through(&model.compile(values).unwrap())
    );
}

/// The Look declares its response monotonic up to amount 100 and not past it, which is what
/// chooses the Exposure search; a Neutral look is monotonic.
#[test]
fn the_looks_declared_monotonicity_chooses_the_exposure_search() {
    let registry = ModuleRegistry::builtin();
    let (module, _) = registry.effect(LOOK_EFFECT).unwrap();
    for (amount, search) in [
        (0., ExposureSearch::Monotone),
        (100., ExposureSearch::Monotone),
        (100.5, ExposureSearch::Exhaustive),
        (200., ExposureSearch::Exhaustive),
    ] {
        let layers = [Layer::new(BASIC_EFFECT, json!({})), look(&registry, amount)];
        let model = forward_model(&registry, &layers, 0, stage()).unwrap();
        assert_eq!(model.search(), search, "amount {amount}");
        assert_eq!(
            module
                .monotonic_luminance(LOOK_EFFECT, EFFECT_FORMAT, &layers[1].payload)
                .unwrap(),
            search == ExposureSearch::Monotone
        );
    }
    assert!(
        module
            .monotonic_luminance(LOOK_EFFECT, EFFECT_FORMAT, &json!({"look": "neutral"}))
            .unwrap()
    );
}
