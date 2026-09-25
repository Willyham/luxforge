//! Tests of stack admission and compilation.
use super::{tests::*, *};
use crate::{
    AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EffectDescriptor, Layer,
    LayerId, Mask, Orientation, RECIPE_FORMAT, Recipe, SnapshotId,
    artifacts::ArtifactTable,
    modules::{
        ActionInput, ActionPlan, Availability, CropPayload, EffectStage, MAX_MASKED_SPATIAL_LAYERS,
        ModuleDescriptor, StageContext,
    },
    render::{
        Entry,
        testing::{render, sample},
    },
};
use serde_json::{Map, Value, json};

/// Whether a stored layer changes nothing is its module's answer, for every module with a
/// neutral form: a field patch at its neutral values however they are spelled (the vignette's
/// is any shape at amount 0), a whole-image crop, the identity orientation and a RAW
/// development at As shot and 0 EV. A pixel replacement has no neutral form, and a layer whose
/// provider is missing or unavailable, or whose payload cannot be read, is never neutral.
#[test]
fn a_layer_is_neutral_by_its_own_modules_rule() {
    let registry = ModuleRegistry::builtin();
    let layer = |effect: &str, payload: Value| Layer::new(effect, payload);
    let as_shot = crate::RawPayload::for_as_shot([2.0, 1.0, 1.5], [[0.5; 3]; 4]).unwrap();
    let cases = [
        (layer(BASIC_EFFECT, json!({})), true),
        (
            layer(BASIC_EFFECT, json!({"exposure": 0.0, "tint": 0})),
            true,
        ),
        (layer(BASIC_EFFECT, json!({"exposure": 0.5})), false),
        (layer(crate::PRESENCE_EFFECT, json!({"texture": 0})), true),
        (layer(crate::PRESENCE_EFFECT, json!({"dehaze": -3})), false),
        (layer(crate::MIXER_EFFECT, json!({"red-hue": 0})), true),
        (
            layer(crate::MIXER_EFFECT, json!({"aqua-luminance": 12})),
            false,
        ),
        (layer(crate::VIGNETTE_EFFECT, json!({})), true),
        (
            layer(
                crate::VIGNETTE_EFFECT,
                json!({"midpoint": 60, "feather": 0}),
            ),
            true,
        ),
        (layer(crate::VIGNETTE_EFFECT, json!({"amount": -10})), false),
        (Layer::crop(CropPayload::NEUTRAL), true),
        (
            Layer::crop(CropPayload {
                angle: 0.0,
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.5,
            }),
            false,
        ),
        (
            Layer::crop(CropPayload {
                angle: 2.0,
                ..CropPayload::NEUTRAL
            }),
            false,
        ),
        (Layer::orientation(Orientation::NEUTRAL), true),
        (
            Layer::orientation(Orientation {
                mirror: true,
                turns: 0,
            }),
            false,
        ),
        (as_shot.layer(LayerId::new()), true),
        (
            crate::RawPayload {
                exposure_ev: 0.25,
                ..as_shot.clone()
            }
            .layer(LayerId::new()),
            false,
        ),
        (Layer::pixel(0, 0, [1, 2, 3]), false),
        (layer(BASIC_EFFECT, json!({"gamma": 1})), false),
        (layer("test.nobody", json!({})), false),
    ];
    for (layer, neutral) in cases {
        assert_eq!(
            registry.layer_neutral(&layer),
            neutral,
            "{} {}",
            layer.effect_id,
            layer.payload
        );
    }
    let mut unavailable = ModuleRegistry::new();
    unavailable
        .register_unavailable(Arc::new(super::BasicModule::new()), "switched off")
        .unwrap();
    assert!(!unavailable.layer_neutral(&layer(BASIC_EFFECT, json!({}))));
}

#[test]
fn an_unavailable_provider_keeps_its_identity_and_fails_evaluation_with_its_layers() {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(TestModule::shared(
            "test.module",
            "test.effect",
            "test-action",
            Availability::Unavailable {
                reason: "not built in this configuration".into(),
            },
        ))
        .unwrap();
    assert!(
        registry.effect("test.effect").is_some(),
        "an unavailable provider keeps its effect identity"
    );
    let first = test_layer("test.effect");
    let second = test_layer("test.effect");
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![
            first.clone(),
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            }),
            second.clone(),
        ],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let expected = format!(
        "unavailable effect test.effect (layers {}, {})",
        first.id, second.id
    );
    for error in [
        registry.validate_recipe(&recipe).unwrap_err(),
        registry.validate_layer(&first).unwrap_err(),
        render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
        sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Incompatible);
    }
    assert_eq!(
        registry.validate_recipe(&recipe).unwrap_err().detail,
        expected
    );
    assert_eq!(
        render(&registry, &source(), SnapshotId::new(), &recipe)
            .unwrap_err()
            .detail,
        expected
    );
    let missing = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![test_layer("test.absent")],
        masks: Vec::new(),
        ..Recipe::default()
    };
    assert_eq!(
        registry.validate_recipe(&missing).unwrap_err().detail,
        format!(
            "unavailable effect test.absent (layers {})",
            missing.layers[0].id
        )
    );
    let _ = AssetId::new();
}

/// Current shapes only: the retired per-action transform effect has no provider, so a stack
/// holding it is refused exactly like any other unavailable effect. Nothing rewrites it, so
/// the data survives the refusal and the owner can open it with a build that provides it.
#[test]
fn a_stack_holding_the_retired_transform_effect_is_refused_without_being_rewritten() {
    let registry = ModuleRegistry::builtin();
    let retired = Layer {
        id: LayerId::new(),
        effect_id: "lightwell.geometry.transform".into(),
        effect_format: EFFECT_FORMAT,
        payload: json!("rotate-right"),
        mask: None,
        artifacts: Vec::new(),
    };
    assert!(registry.effect("lightwell.geometry.transform").is_none());
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![retired.clone()],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let expected = format!(
        "unavailable effect lightwell.geometry.transform (layers {})",
        retired.id
    );
    for error in [
        registry.validate_recipe(&recipe).unwrap_err(),
        registry.validate_layer(&retired).unwrap_err(),
        render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
        sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(error.detail, expected);
    }
    assert_eq!(recipe.layers, vec![retired], "the refused stack is kept");
}

/// A mask reaches a layer only where the layer's input is the content stage the mask is stored
/// in. The stage comes from the effect's provider, so the rule is the registry's and it holds
/// wherever a recipe is validated or compiled.
#[test]
fn a_mask_may_only_reach_a_layer_before_the_geometry_tail() {
    let registry = staged_registry();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
    ));
    let masked = |layer: Layer| Layer {
        mask: Some(mask.id.clone()),
        ..layer
    };
    let recipe = |layer: Layer| Recipe {
        format: RECIPE_FORMAT,
        layers: vec![layer],
        masks: vec![mask.clone()],
        ..Recipe::default()
    };
    // A colour-stage layer addresses the content stage, so it may carry one.
    let colour = recipe(masked(test_layer(MIXER_EFFECT)));
    registry.validate_recipe(&colour).unwrap();
    registry.compile(2, 1, &colour).unwrap();
    for (layer, stage) in [
        (Layer::orientation(Orientation::NEUTRAL), "geometry"),
        (test_layer(FINISH_EFFECT), "finish"),
    ] {
        let refused = recipe(masked(layer.clone()));
        let expected = format!(
            "layer {} carries a mask, which a {stage} effect cannot: a mask is stored in \
             content-stage coordinates",
            refused.layers[0].id
        );
        for error in [
            registry.validate_recipe(&refused).unwrap_err(),
            registry
                .compile(2, 1, &refused)
                .err()
                .expect("a masked layer at the geometry tail never compiles"),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(error.detail, expected);
        }
        // Unmasked, the same layer is the ordinary stack it always was.
        registry.validate_recipe(&recipe(layer)).unwrap();
    }
}

/// A mask is stored in content-stage coordinates, so an effect whose input is not that stage
/// cannot declare itself maskable: the descriptor is refused at registration, by name, rather
/// than carrying a flag nothing could honour.
#[test]
fn a_maskable_effect_is_refused_at_the_geometry_and_finish_stages() {
    let descriptor = |stage: EffectStage| ModuleDescriptor {
        id: "test.maskable".into(),
        title: "Maskable".into(),
        hint: None,
        effects: vec![EffectDescriptor {
            id: "test.maskable.effect".into(),
            format: EFFECT_FORMAT,
            stage,
            order: 0,
            maskable: true,
            artifacts: false,
            single: false,
        }],
        actions: Vec::new(),
        queries: Vec::new(),
        controls: Vec::new(),
        reset: None,
        canvas: None,
        developer: false,
        collapsed: false,
        layout: crate::ModuleLayout::Stacked,
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    };
    for stage in [EffectStage::Geometry, EffectStage::Finish] {
        let error = ModuleRegistry::new()
            .register(TestModule::from_descriptor(descriptor(stage)))
            .expect_err("a maskable effect at the geometry tail");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            format!(
                "effect test.maskable.effect declares maskable at the {} stage, which a mask \
                 stored in content-stage coordinates cannot reach",
                stage.as_str()
            )
        );
    }
    for stage in [
        EffectStage::Source,
        EffectStage::Pixel,
        EffectStage::Color,
        EffectStage::Spatial,
    ] {
        ModuleRegistry::new()
            .register(TestModule::from_descriptor(descriptor(stage)))
            .expect("every content-stage effect may declare it");
    }
}

/// A declared `single` effect is one layer per target: the global layer and each mask are
/// distinct targets, so one effect may hold one layer in each and two layers with the *same*
/// target are still the ambiguity the host refuses without rewriting anything.
#[test]
fn one_layer_per_target_is_what_single_layer_means() {
    let registry = ModuleRegistry::builtin();
    let first = gradient_mask("Mask 1");
    let second = gradient_mask("Mask 2");
    let recipe = |layers: Vec<Layer>| Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks: vec![first.clone(), second.clone()],
        ..Recipe::default()
    };
    // The global layer and one layer per mask: three layers of one single-layer effect, legal.
    let legal = recipe(vec![
        basic_layer(),
        bound(basic_layer(), &first),
        bound(basic_layer(), &second),
    ]);
    registry.compile(64, 48, &legal).expect("one per target");
    // Two layers of one effect with the same target, global or masked, is the old refusal.
    for (case, layers) in [
        ("two global layers", vec![basic_layer(), basic_layer()]),
        (
            "two layers in one mask",
            vec![bound(basic_layer(), &first), bound(basic_layer(), &first)],
        ),
    ] {
        let error = registry
            .compile(64, 48, &recipe(layers))
            .err()
            .expect("ambiguous layers");
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert_eq!(error.detail, "ambiguous Basic layers", "{case}");
    }
}

/// A mask reaches a colour operation and a spatial operation, so a layer that carries one and
/// compiles into anything else is refused by name on every path that would have to draw it.
/// Refusing is what keeps such a layer from being committed at all — the host compiles a stack
/// before it persists one — and is the alternative to the silent omission of rendering it as if it
/// applied everywhere.
///
/// The live case is a point replacement: no delivered pixel effect declares itself maskable, and
/// the recipe model lets a stored layer of one carry a mask, so this refusal is what a stack like
/// that meets. A geometry or finish layer is refused for the earlier reason, that it has no
/// content stage to read a mask in, and that refusal is asserted below too so the two cannot both
/// be removed by accident.
#[test]
fn a_mask_this_build_cannot_evaluate_is_refused_by_name() {
    let registry = ModuleRegistry::builtin();
    let mask = gradient_mask("Mask 1");
    let recipe = |layer: Layer| Recipe {
        format: RECIPE_FORMAT,
        layers: vec![layer],
        masks: vec![mask.clone()],
        ..Recipe::default()
    };
    let layer = Layer::pixel(1, 1, [9, 9, 9]);
    // Unmasked, the same layer compiles as it always has.
    registry
        .compile(256, 256, &recipe(layer.clone()))
        .unwrap_or_else(|error| panic!("an unmasked pixel layer: {error:?}"));
    let refused = recipe(bound(layer, &mask));
    let error = registry
        .compile(256, 256, &refused)
        .err()
        .expect("a masked pixel layer");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert_eq!(
        error.detail,
        format!(
            "layer {} carries a mask on the pixel effect {}, and this build evaluates a \
             mask only on a colour-stage or spatial-stage effect",
            refused.layers[0].id, refused.layers[0].effect_id
        )
    );
    // The refusal reads the stack; it rewrites nothing.
    assert!(refused.layers[0].mask.is_some());

    // The stage rule still refuses the two stages that have no content stage to read a mask in.
    for (stage, layer) in [
        (
            "geometry",
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            }),
        ),
        (
            "finish",
            Layer {
                id: LayerId::new(),
                effect_id: crate::VIGNETTE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"amount": -40.0}),
                mask: None,
                artifacts: Vec::new(),
            },
        ),
    ] {
        let refused = recipe(bound(layer, &mask));
        let error = registry
            .compile(256, 256, &refused)
            .err()
            .unwrap_or_else(|| panic!("a masked {stage} layer compiled"));
        assert_eq!(error.kind, ErrorKind::Validation, "{stage}");
        assert_eq!(
            error.detail,
            format!(
                "layer {} carries a mask, which a {stage} effect cannot: a mask is stored in \
                 content-stage coordinates",
                refused.layers[0].id
            ),
            "{stage}"
        );
        assert!(refused.layers[0].mask.is_some(), "{stage}");
    }
}

/// A masked spatial layer compiles, because the masked spatial primitive is delivered: the mask
/// is attached to the operation the module returned, against the stage the layer receives, which
/// for a stage boundary is also the frame it reads and writes.
#[test]
fn a_masked_spatial_layer_compiles_with_its_mask_attached() {
    let registry = ModuleRegistry::builtin();
    let mask = gradient_mask("Mask 1");
    let presence = Layer {
        id: LayerId::new(),
        effect_id: crate::PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"clarity": 40.0}),
        mask: None,
        artifacts: Vec::new(),
    };
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![bound(presence, &mask)],
        masks: vec![mask],
        ..Recipe::default()
    };
    let compiled = registry
        .compile(256, 200, &recipe)
        .unwrap_or_else(|error| panic!("a masked spatial layer: {error:?}"));
    let entry = compiled.segments[1]
        .entry
        .as_ref()
        .expect("a spatial entry");
    let Entry::Spatial { operation, .. } = entry else {
        panic!("a spatial entry");
    };
    let attached = operation.mask().expect("the mask is attached");
    // The mask is compiled against the stage the layer receives, which is the frame this
    // operation reads and writes: the tile loop needs no mapping at all.
    assert_eq!(
        attached.stage(),
        Stage {
            width: 256,
            height: 200
        }
    );
    assert_eq!(compiled.segments[0].width, 256);
}

/// Each masked spatial layer is a stage boundary and therefore a sequential full frame, so the
/// design caps them at four. The fifth is a `resource-limit` error naming the limit; nothing is
/// dropped, reordered or rendered as if it applied everywhere.
#[test]
fn a_fifth_masked_spatial_layer_is_a_resource_limit() {
    let registry = ModuleRegistry::builtin();
    let masks: Vec<Mask> = (0..MAX_MASKED_SPATIAL_LAYERS + 1)
        .map(|index| gradient_mask(&format!("Mask {index}")))
        .collect();
    let presence = |mask: &Mask| Layer {
        id: LayerId::new(),
        effect_id: crate::PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"clarity": 40.0}),
        mask: Some(mask.id.clone()),
        artifacts: Vec::new(),
    };
    let recipe = |count: usize| Recipe {
        format: RECIPE_FORMAT,
        layers: masks[..count].iter().map(presence).collect(),
        masks: masks.clone(),
        ..Recipe::default()
    };
    registry
        .compile(128, 128, &recipe(MAX_MASKED_SPATIAL_LAYERS))
        .unwrap_or_else(|error| panic!("four masked spatial layers: {error:?}"));
    let error = registry
        .compile(128, 128, &recipe(MAX_MASKED_SPATIAL_LAYERS + 1))
        .err()
        .expect("five masked spatial layers");
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        "this recipe holds 5 masked spatial layers, more than the 4 the host evaluates: each \
         one is a stage boundary and therefore a sequential full frame"
    );
}

/// The missing mask is refused wherever a recipe is evaluated, because compiling checks it and
/// every render, sample and plan compiles.
#[test]
fn a_layer_naming_a_mask_the_recipe_does_not_carry_is_refused_by_every_compile() {
    let registry = ModuleRegistry::builtin();
    let mask = Mask::new("Mask 1");
    let layer = Layer {
        mask: Some(mask.id.clone()),
        ..Layer::pixel(0, 0, [1, 2, 3])
    };
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![layer.clone()],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let expected = format!(
        "layer {} references mask {}, which this recipe does not carry",
        layer.id, mask.id
    );
    for error in [
        registry.validate_recipe(&recipe).unwrap_err(),
        registry
            .compile(2, 1, &recipe)
            .err()
            .expect("a missing mask never compiles"),
        render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
        sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        crate::render::testing::extents(&registry, &source(), &recipe).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(error.detail, expected);
    }
    assert_eq!(recipe.layers, vec![layer], "the refused stack is kept");
}

/// The mask table is checked once, where a recipe enters the service, and compiling trusts it:
/// a table past a per-recipe limit and a component this build cannot read are refused by the
/// admission check, while compiling the same stack — whose masks no layer draws — reads none of
/// the table and answers.
#[test]
fn the_mask_table_is_checked_on_admission_and_not_by_compile() {
    let registry = ModuleRegistry::builtin();
    let mut too_many = Recipe::default();
    for index in 0..=crate::MASKS_PER_RECIPE {
        too_many.masks.push(Mask::new(format!("Mask {index}")));
    }
    let mut unreadable = Mask::new("Mask 1");
    let name = unreadable.next_component_name("future-kind");
    unreadable.components.push(Component::new(
        name,
        ComponentMode::Add,
        "future-kind",
        json!({}),
    ));
    let unknown = Recipe {
        masks: vec![unreadable],
        ..Recipe::default()
    };
    for (recipe, kind, detail) in [
        (
            &too_many,
            ErrorKind::ResourceLimit,
            format!(
                "recipe has {} masks; the limit is {} masks per recipe",
                crate::MASKS_PER_RECIPE + 1,
                crate::MASKS_PER_RECIPE
            ),
        ),
        (
            &unknown,
            ErrorKind::Incompatible,
            "unknown mask component future-kind".to_owned(),
        ),
    ] {
        let refused = registry.validate_recipe(recipe).unwrap_err();
        assert_eq!((refused.kind, &refused.detail), (kind, &detail));
        assert_eq!(
            recipe.validate_mask_table().unwrap_err().detail,
            detail,
            "admission's refusal is the table's own"
        );
        registry
            .compile(4, 4, recipe)
            .expect("compiling does not check the table again");
    }
}

/// A component of a kind this build cannot evaluate is refused by name where a recipe enters the
/// service and wherever the mask would have to be drawn, and nothing about the stored mask is
/// rewritten: the host keeps every byte and says what it could not draw, rather than rendering
/// the layer unmasked or dropping the component. The model's structural check never asks what a
/// kind means, so the stack still reads and round-trips.
#[test]
fn a_component_kind_this_build_does_not_know_is_refused_by_every_compile() {
    let registry = ModuleRegistry::builtin();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("future-kind");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "future-kind",
        json!({"nested": {"points": [[0.25, 0.5], [0.75, 0.5]]}, "flag": true, "n": 3.5}),
    ));
    // A layer that draws the mask: a non-neutral colour layer bound to it.
    let layer = Layer {
        mask: Some(mask.id.clone()),
        ..Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.5}))
    };
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![layer],
        masks: vec![mask.clone()],
        ..Recipe::default()
    };
    // Admission refuses it once, with the kind table's own words.
    let admitted = registry.validate_recipe(&recipe).unwrap_err();
    assert_eq!(admitted.kind, ErrorKind::Incompatible);
    assert_eq!(admitted.detail, "unknown mask component future-kind");
    for error in [
        registry
            .compile(2, 1, &recipe)
            .err()
            .expect("an unknown component kind never compiles"),
        render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
        sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        crate::render::testing::extents(&registry, &source(), &recipe).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(error.detail, "unknown mask component future-kind");
    }
    // The structural model never asks what a kind means, so the refused stack still reads back
    // byte for byte through the persisted shape.
    recipe.validate().unwrap();
    let reopened: Recipe = serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
    assert_eq!(reopened, recipe, "the refused stack is kept");
    assert_eq!(reopened.masks[0].components[0].kind, "future-kind");
}

#[test]
fn payload_format_and_shape_are_validated_by_the_providing_module() {
    let registry = ModuleRegistry::builtin();
    let wrong_format = Layer {
        effect_format: 99,
        ..Layer::pixel(0, 0, [1, 2, 3])
    };
    assert_eq!(
        registry.validate_layer(&wrong_format).unwrap_err().kind,
        ErrorKind::Incompatible
    );
    let wrong_payload = Layer {
        payload: json!({"x": 1}),
        ..Layer::pixel(0, 0, [1, 2, 3])
    };
    assert_eq!(
        registry.validate_layer(&wrong_payload).unwrap_err().kind,
        ErrorKind::Validation
    );
    for wrong_orientation in [
        json!("rotate-sideways"),
        json!({"mirror": false, "turns": 4}),
        json!({"mirror": false, "turns": 0, "flip": true}),
    ] {
        let layer = Layer {
            payload: wrong_orientation.clone(),
            ..Layer::orientation(Orientation::NEUTRAL)
        };
        assert_eq!(
            registry.validate_layer(&layer).unwrap_err().kind,
            ErrorKind::Validation,
            "{wrong_orientation}"
        );
    }
    assert!(
        registry
            .validate_layer(&Layer::orientation(Orientation::NEUTRAL))
            .is_ok()
    );
    assert!(
        registry
            .validate_layer(&Layer::pixel(0, 0, [1, 2, 3]))
            .is_ok()
    );
}

/// The one order the host cannot evaluate: a finish layer is defined in the output coordinates
/// the geometry tail produced, so a geometry layer after it has no stage to address. The stack
/// is refused as it stands, and nothing is rewritten, reordered or dropped.
#[test]
fn a_finish_layer_before_a_geometry_layer_is_refused_by_compilation() {
    let registry = staged_registry();
    let finish = test_layer(FINISH_EFFECT);
    let turn = Layer::orientation(Orientation {
        mirror: false,
        turns: 1,
    });
    let refused = vec![finish.clone(), turn.clone()];
    let error = registry
        .compile_layers(
            2,
            1,
            &refused,
            &[],
            &crate::path::StrokeTable::default(),
            &ArtifactTable::default(),
        )
        .err()
        .expect("a finish layer before geometry never compiles");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error.detail.starts_with("finish layer precedes geometry"),
        "{}",
        error.detail
    );
    assert!(error.detail.contains(finish.id.as_str()));
    assert!(error.detail.contains(turn.id.as_str()));
    assert_eq!(refused.len(), 2, "the refused stack is kept as it stands");
    // The order the host does build compiles, and so does a stack with no geometry at all.
    assert!(
        registry
            .compile_layers(
                2,
                1,
                &[turn, finish.clone()],
                &[],
                &crate::path::StrokeTable::default(),
                &ArtifactTable::default(),
            )
            .is_ok()
    );
    assert!(
        registry
            .compile_layers(
                2,
                1,
                &[finish],
                &[],
                &crate::path::StrokeTable::default(),
                &ArtifactTable::default(),
            )
            .is_ok()
    );
}

const BOUND_EFFECT: &str = "test.bound.effect";

/// An identity colour unit that names the artifact it was compiled with, so a compiled stack
/// shows which artifacts its module received and in what order.
struct Named(String);

impl crate::PointwiseColor for Named {
    fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {}
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        self.0.clone()
    }
}

/// A colour effect that declares artifacts and compiles one named unit per bound artifact, or,
/// when its flag is false, a module that declares them and offers no capability hooks.
struct BoundModule(ModuleDescriptor, bool);

impl ToolModule for BoundModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: Map::new(),
        })
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok("bound".into())
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        Ok(Processing::Color(crate::ColorOperation::neutral()))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        self.1.then_some(self)
    }
}

impl CapabilityModule for BoundModule {
    fn compile_bound(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: Stage,
        artifacts: &[Arc<crate::artifacts::PreparedArtifact>],
    ) -> Result<Processing, Error> {
        Ok(Processing::Color(crate::ColorOperation::new(
            artifacts
                .iter()
                .map(|artifact| {
                    Arc::new(Named(artifact.id.to_string())) as Arc<dyn crate::PointwiseColor>
                })
                .collect(),
        )))
    }
}

fn bound_descriptor() -> ModuleDescriptor {
    ModuleDescriptor::parse(&json!({
        "id": "test.bound",
        "title": "Bound",
        "effects": [{"id": BOUND_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "artifacts": true}],
        "actions": [],
        "controls": [],
        "availability": {"kind": "available"},
    }))
    .unwrap()
}

/// The built-in providers and [`BoundModule`].
fn bound_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(Arc::new(BoundModule(bound_descriptor(), true)))
        .unwrap();
    registry
}

/// A module whose declarations need capability hooks — an artifact effect, a task, an
/// activation or a resource — registers only when it provides them, and only such a module is
/// found by the capability lookup.
#[test]
fn a_module_that_declares_capabilities_registers_only_with_its_hooks() {
    let mut registry = ModuleRegistry::builtin();
    let error = registry
        .register(Arc::new(BoundModule(bound_descriptor(), false)))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error.detail.contains("test.bound")
            && error.detail.contains(BOUND_EFFECT)
            && error.detail.contains("capability hooks"),
        "{}",
        error.detail
    );
    assert!(
        registry.module("test.bound").is_none(),
        "nothing registered"
    );
    let capable = crate::capabilities::testing::capability_descriptor();
    for (declares, descriptor) in [
        ("task", capable.clone()),
        (
            "activation",
            ModuleDescriptor {
                tasks: Vec::new(),
                controls: Vec::new(),
                ..capable.clone()
            },
        ),
        (
            "resource",
            ModuleDescriptor {
                tasks: Vec::new(),
                controls: Vec::new(),
                activation: None,
                ..capable.clone()
            },
        ),
    ] {
        let error = ModuleRegistry::new()
            .register(Arc::new(BoundModule(descriptor, false)))
            .unwrap_err();
        assert!(
            error.detail.contains(declares) && error.detail.contains("capability hooks"),
            "{}",
            error.detail
        );
    }
    registry
        .register(Arc::new(BoundModule(bound_descriptor(), true)))
        .unwrap();
    assert!(registry.capabilities("test.bound").is_some());
    assert!(
        registry
            .capabilities(crate::BasicModule::new().descriptor().id.as_str())
            .is_none()
    );
    assert!(registry.capabilities("test.missing").is_none());
}

/// One byte of verified artifact under an identity that starts with `digit`.
fn bound_artifact(digit: &str) -> Arc<crate::artifacts::PreparedArtifact> {
    let id = crate::ArtifactId::for_hash(&format!("{digit}{}", "d".repeat(63))).unwrap();
    let meta = crate::artifacts::ArtifactMeta {
        kind: "test".into(),
        width: None,
        height: None,
        colour: None,
    };
    Arc::new(crate::artifacts::PreparedArtifact::new(
        id,
        &meta,
        vec![0].into(),
    ))
}

#[test]
fn compile_binds_artifacts_in_listed_order_and_refuses_unbound_ones() {
    let registry = bound_registry();
    let (first, second) = (bound_artifact("1"), bound_artifact("2"));
    let bound: ArtifactTable = [first.clone(), second.clone()].into_iter().collect();
    let layer = Layer {
        artifacts: vec![second.id.clone(), first.id.clone()],
        ..test_layer(BOUND_EFFECT)
    };
    let compiled = registry
        .compile_layers(
            2,
            1,
            std::slice::from_ref(&layer),
            &[],
            &Default::default(),
            &bound,
        )
        .unwrap();
    let Processing::Color(operation) = &compiled.segments[0].operations[0] else {
        panic!("a colour operation");
    };
    let named: Vec<String> = operation
        .units()
        .iter()
        .map(|unit| unit.describe())
        .collect();
    assert_eq!(
        named,
        [second.id.to_string(), first.id.to_string()],
        "the module receives the layer's order"
    );
    // A layer without artifacts is compiled exactly as before, through `compile`.
    let plain = registry
        .compile_layers(
            2,
            1,
            &[test_layer(BOUND_EFFECT)],
            &[],
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
    assert!(plain.segments[0].operations.is_empty());
    // Bytes the table was not bound with are refused rather than evaluated without them,
    // whatever else in the process holds them.
    let partial: ArtifactTable = std::iter::once(first.clone()).collect();
    let error = registry
        .compile_layers(
            2,
            1,
            std::slice::from_ref(&layer),
            &[],
            &Default::default(),
            &partial,
        )
        .err()
        .expect("an unbound artifact never compiles");
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!(
            "artifact {} of layer {} is not bound to this recipe",
            second.id, layer.id
        )
    );
    // An effect that does not declare artifacts cannot be compiled with any.
    let pixel = Layer {
        artifacts: vec![first.id.clone()],
        ..Layer::pixel(0, 0, [1, 2, 3])
    };
    let error = registry
        .compile_layers(2, 1, &[pixel], &[], &Default::default(), &bound)
        .err()
        .expect("a pixel layer never binds an artifact");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("which its effect does not declare"));
}

/// Compiling a recipe reads its artifacts from the table the recipe carries and nowhere else:
/// a listed artifact the table lacks refuses the whole recipe by name, even while the bytes
/// are alive elsewhere, and the table is neither stored nor part of the recipe's equality.
#[test]
fn a_recipe_whose_bound_table_lacks_a_listed_artifact_refuses_to_compile_naming_it() {
    let registry = bound_registry();
    let (first, second) = (bound_artifact("3"), bound_artifact("4"));
    let layer = Layer {
        artifacts: vec![first.id.clone(), second.id.clone()],
        ..test_layer(BOUND_EFFECT)
    };
    let unbound = Recipe {
        layers: vec![layer.clone()],
        ..Recipe::default()
    };
    let partly = Recipe {
        artifacts: std::iter::once(first.clone()).collect(),
        ..unbound.clone()
    };
    for (recipe, missing) in [(&unbound, &first), (&partly, &second)] {
        let error = registry
            .compile(2, 1, recipe)
            .err()
            .expect("a recipe missing a bound artifact never compiles");
        assert_eq!(error.kind, ErrorKind::SourceUnavailable);
        assert_eq!(
            error.detail,
            format!(
                "artifact {} of layer {} is not bound to this recipe",
                missing.id, layer.id
            )
        );
    }
    // Bound with both, the same recipe compiles, and a clone shares the one table.
    let bound = Recipe {
        artifacts: [first.clone(), second.clone()].into_iter().collect(),
        ..unbound.clone()
    };
    assert!(registry.compile(2, 1, &bound).is_ok());
    let clone = bound.clone();
    assert!(clone.artifacts.shares(&bound.artifacts));
    assert!(registry.compile(2, 1, &clone).is_ok());
    // The bytes are never stored and never make two recipes differ.
    assert_eq!(bound, unbound);
    assert_eq!(
        serde_json::to_value(&bound).unwrap(),
        serde_json::to_value(&unbound).unwrap()
    );
    let read: Recipe = serde_json::from_value(serde_json::to_value(&bound).unwrap()).unwrap();
    assert!(read.artifacts.is_empty());
}
