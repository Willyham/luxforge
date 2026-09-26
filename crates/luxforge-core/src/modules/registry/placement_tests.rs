//! Tests of where a committed layer joins a stack and which layer a target owns.
use super::{tests::*, *};
use crate::{
    BASIC_EFFECT, CROP_EFFECT, EFFECT_FORMAT, Layer, MaskId, ORIENTATION_EFFECT, Orientation,
    PIXEL_EFFECT, RAW_EFFECT,
    modules::{CropPayload, EffectStage, ModuleDescriptor},
};
use serde_json::json;

/// The one lookup of a module's own layer answers per target exactly as the compile refuses per
/// target: a masked layer of a maskable effect belongs only to its mask, a layer of an effect
/// that is not maskable belongs to every target, and two layers for one target are refused with
/// the compile's own words, whatever else the stack holds.
#[test]
fn a_modules_own_layer_is_found_for_its_target_only() {
    let registry = ModuleRegistry::builtin();
    let first = gradient_mask("Mask 1");
    let second = gradient_mask("Mask 2");
    let global = basic_layer();
    let in_first = bound(basic_layer(), &first);
    let crop = Layer::crop(CropPayload::NEUTRAL);
    let stack = [
        Layer::pixel(0, 0, [1, 2, 3]),
        global.clone(),
        in_first.clone(),
        crop.clone(),
    ];
    let found = |target: Option<&MaskId>, effect: &str| {
        registry
            .own_layer(&stack, effect, target)
            .unwrap()
            .map(|(index, layer)| (index, layer.id.clone()))
    };
    assert_eq!(found(None, BASIC_EFFECT), Some((1, global.id.clone())));
    assert_eq!(
        found(Some(&first.id), BASIC_EFFECT),
        Some((2, in_first.id.clone()))
    );
    assert_eq!(found(Some(&second.id), BASIC_EFFECT), None);
    for target in [None, Some(&first.id)] {
        assert_eq!(
            found(target, CROP_EFFECT),
            Some((3, crop.id.clone())),
            "an effect that is not maskable belongs to every target"
        );
    }
    let ambiguous = registry
        .own_layer(&[global.clone(), basic_layer()], BASIC_EFFECT, None)
        .expect_err("two global layers");
    assert_eq!(ambiguous.kind, ErrorKind::Validation);
    assert_eq!(ambiguous.detail, "ambiguous Basic layers");
    assert!(
        registry
            .own_layer(&[global, in_first], BASIC_EFFECT, None)
            .is_ok(),
        "a global and a masked layer are two targets, not an ambiguity"
    );
    // Every built-in module that owns one layer declares it; the transform's orientation does
    // not, because a fold leaves a neutral orientation stored after the crop beside the one ahead.
    for effect in [
        BASIC_EFFECT,
        crate::MIXER_EFFECT,
        crate::PRESENCE_EFFECT,
        crate::VIGNETTE_EFFECT,
        CROP_EFFECT,
    ] {
        assert!(registry.effect_single(effect), "{effect}");
    }
    for effect in [crate::PIXEL_EFFECT, ORIENTATION_EFFECT, crate::RAW_EFFECT] {
        assert!(!registry.effect_single(effect), "{effect}");
    }
}

/// The order a person can see: a masked layer of an effect follows the global layer of that
/// effect and the masked layers of earlier masks, and nothing else moves.
#[test]
fn a_masked_layer_is_placed_after_the_global_layer_and_by_its_masks_index() {
    let registry = ModuleRegistry::builtin();
    let first = gradient_mask("Mask 1");
    let second = gradient_mask("Mask 2");
    let third = gradient_mask("Mask 3");
    let masks = vec![first.clone(), second.clone(), third.clone()];
    let global = basic_layer();
    let in_first = bound(basic_layer(), &first);
    let in_third = bound(basic_layer(), &third);
    let crop = Layer::crop(CropPayload {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    for (case, layers, target, expected) in [
        ("an empty stack", vec![], Some(&first), 0),
        ("a global layer only", vec![global.clone()], Some(&first), 1),
        (
            "after the global layer and before the tail",
            vec![global.clone(), crop.clone()],
            Some(&second),
            1,
        ),
        (
            "between two masks",
            vec![global.clone(), in_first.clone(), in_third.clone()],
            Some(&second),
            2,
        ),
        (
            "before a later mask",
            vec![global.clone(), in_third.clone()],
            Some(&first),
            1,
        ),
        (
            "after an earlier mask",
            vec![global.clone(), in_first.clone()],
            Some(&third),
            2,
        ),
        (
            // The global layer keeps its own placement, and never lands after a masked layer of
            // its own effect.
            "the global layer under existing masked layers",
            vec![in_first.clone(), in_third.clone()],
            None,
            0,
        ),
        (
            "the global layer of an effect with none",
            vec![crop.clone()],
            None,
            0,
        ),
    ] {
        assert_eq!(
            registry.insertion_index_for_target(
                &layers,
                BASIC_EFFECT,
                target.map(|mask| &mask.id),
                &masks,
            ),
            expected,
            "{case}"
        );
    }
    // An unmasked target is placed exactly where the stage rule alone places it, so nothing about
    // masking moves an ordinary layer.
    for layers in [
        vec![],
        vec![crop.clone()],
        vec![global.clone(), crop.clone()],
    ] {
        assert_eq!(
            registry.insertion_index_for_target(&layers, BASIC_EFFECT, None, &masks),
            registry.insertion_index_for(&layers, BASIC_EFFECT),
            "the global target follows the delivered placement rule"
        );
    }
}

/// Reordering the mask list re-sorts exactly the masked layers of each effect, in the positions
/// they already occupy, and moves nothing else.
#[test]
fn sorting_masked_layers_moves_only_them() {
    let registry = ModuleRegistry::builtin();
    let first = gradient_mask("Mask 1");
    let second = gradient_mask("Mask 2");
    let crop = Layer::crop(CropPayload {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    let global = basic_layer();
    let in_first = bound(basic_layer(), &first);
    let in_second = bound(basic_layer(), &second);
    let mut layers = vec![
        Layer::pixel(0, 0, [1, 2, 3]),
        global.clone(),
        in_first.clone(),
        in_second.clone(),
        crop.clone(),
    ];
    let identities = |layers: &[Layer]| {
        layers
            .iter()
            .map(|layer| layer.id.clone())
            .collect::<Vec<_>>()
    };
    let before = identities(&layers);
    // The mask list as it stands: the stack is already sorted, so nothing moves.
    registry.sort_masked_layers(&mut layers, &[first.clone(), second.clone()]);
    assert_eq!(identities(&layers), before, "an already ordered stack");
    // The masks swap places, and with them exactly the two masked layers.
    registry.sort_masked_layers(&mut layers, &[second.clone(), first.clone()]);
    assert_eq!(
        identities(&layers),
        vec![
            before[0].clone(),
            global.id.clone(),
            in_second.id.clone(),
            in_first.id.clone(),
            crop.id.clone(),
        ],
        "only the masked layers moved"
    );
    // A stack with no masked layers is untouched, and so is a stack whose masks are unchanged.
    let mut unmasked = vec![Layer::pixel(0, 0, [1, 2, 3]), global.clone(), crop.clone()];
    let before = identities(&unmasked);
    registry.sort_masked_layers(&mut unmasked, &[first, second]);
    assert_eq!(identities(&unmasked), before);
}

#[test]
fn a_pixel_layer_joins_the_stack_before_the_first_geometry_layer() {
    let registry = ModuleRegistry::builtin();
    let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
    let turn = || {
        Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        })
    };
    let crop = || {
        Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        })
    };
    // `geometry` is where a default-order geometry layer such as the orientation goes: ahead
    // of the crop, whose own order is later, and otherwise at the end of the tail.
    for (case, layers, expected, geometry) in [
        ("an empty stack", vec![], 0, 0),
        ("geometry only", vec![turn(), crop()], 0, 1),
        ("pixels only", vec![pixel(), pixel()], 2, 2),
        // A stored stack with a turn after the crop keeps its order.
        (
            "a pixel before the tail",
            vec![pixel(), crop(), turn()],
            1,
            1,
        ),
        (
            // Such a stack renders as it always did; a new edit still joins the content stage.
            "an interleaved pixel after geometry",
            vec![pixel(), turn(), pixel(), crop()],
            1,
            3,
        ),
        (
            "a layer no provider declares does not open the tail",
            vec![test_layer("test.absent"), turn()],
            1,
            2,
        ),
    ] {
        assert_eq!(
            registry.insertion_index(&layers, EffectStage::Pixel, 0),
            expected,
            "{case}"
        );
        // A colour-stage layer joins the stack by the same rule, so a Basic layer lands before
        // the quarter-turns, reflections and crop that carry it.
        assert_eq!(
            registry.insertion_index(&layers, EffectStage::Color, 0),
            expected,
            "{case}: colour joins where a pixel edit does"
        );
        assert_eq!(
            registry.insertion_index(&layers, EffectStage::Geometry, 0),
            geometry,
            "{case}: the orientation goes ahead of the crop"
        );
        assert_eq!(
            registry.insertion_index_for(&layers, ORIENTATION_EFFECT),
            geometry,
            "{case}: the orientation effect's own placement"
        );
        assert_eq!(
            registry.insertion_index_for(&layers, CROP_EFFECT),
            layers.len(),
            "{case}: a crop ends the tail"
        );
        // A client reading the same rule from its module list places every effect alike.
        let listed: Vec<ModuleDescriptor> = registry.descriptors().into_iter().cloned().collect();
        for effect in [
            PIXEL_EFFECT,
            ORIENTATION_EFFECT,
            CROP_EFFECT,
            crate::VIGNETTE_EFFECT,
            "test.absent",
        ] {
            assert_eq!(
                insertion_index_among(&listed, &layers, effect),
                registry.insertion_index_for(&layers, effect),
                "{case}: {effect} from the module list"
            );
        }
        // The host reads the same rule from an effect's own descriptor.
        assert_eq!(
            registry.insertion_index_for(&layers, PIXEL_EFFECT),
            expected,
            "{case}: the pixel effect's own placement"
        );
    }
    assert_eq!(
        registry.effect_stage(PIXEL_EFFECT),
        Some(EffectStage::Pixel)
    );
    assert_eq!(
        registry.effect_stage(CROP_EFFECT),
        Some(EffectStage::Geometry)
    );
    assert_eq!(registry.effect_stage("test.absent"), None);
}

/// Every stage's region, over stacks that mix them all: a pixel or colour layer joins the
/// content region before the first spatial, geometry or finish layer, a spatial layer follows
/// the pointwise work and precedes the tail, a geometry layer precedes the first finish layer
/// and a finish layer goes last, with a leading source layer always keeping index zero.
#[test]
fn every_stage_joins_the_region_the_placement_table_names() {
    let registry = staged_registry();
    let source = || test_layer(RAW_EFFECT);
    let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
    let basic = || test_layer(BASIC_EFFECT);
    let mixer = || test_layer(MIXER_EFFECT);
    let spatial = || test_layer(SPATIAL_EFFECT);
    let turn = || {
        Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        })
    };
    let finish = || test_layer(FINISH_EFFECT);
    // (case, stack, pixel, colour order 0, colour order 10, spatial, geometry, finish)
    for (case, layers, expected) in [
        ("an empty stack", vec![], [0, 0, 0, 0, 0, 0]),
        (
            "the canonical stack of every stage",
            vec![
                source(),
                pixel(),
                basic(),
                mixer(),
                spatial(),
                turn(),
                finish(),
            ],
            [4, 3, 4, 5, 6, 7],
        ),
        ("a source layer alone", vec![source()], [1, 1, 1, 1, 1, 1]),
        (
            "the geometry tail only",
            vec![turn(), turn()],
            [0, 0, 0, 0, 2, 2],
        ),
        (
            "a finish layer over the tail",
            vec![turn(), finish()],
            [0, 0, 0, 0, 1, 2],
        ),
        (
            "a spatial layer before the tail",
            vec![basic(), spatial(), turn()],
            [1, 1, 1, 2, 3, 3],
        ),
        // The colour region is read in order: an order-0 layer goes before an order-10 one
        // whichever was committed first, and neither existing layer moves.
        (
            "one colour layer of order 10",
            vec![mixer()],
            [1, 0, 1, 1, 1, 1],
        ),
        (
            "one colour layer of order 0",
            vec![basic()],
            [1, 1, 1, 1, 1, 1],
        ),
        (
            "both colour orders before the tail",
            vec![basic(), mixer(), turn()],
            [2, 1, 2, 2, 3, 3],
        ),
        (
            // A stored stack the host did not build keeps every layer where it is; the region
            // still ends at the first layer of a later stage.
            "a pixel layer stored after the tail",
            vec![turn(), pixel()],
            [0, 0, 0, 0, 2, 2],
        ),
    ] {
        let placement = [
            registry.insertion_index(&layers, EffectStage::Pixel, 0),
            registry.insertion_index(&layers, EffectStage::Color, 0),
            registry.insertion_index(&layers, EffectStage::Color, 10),
            registry.insertion_index(&layers, EffectStage::Spatial, 0),
            registry.insertion_index(&layers, EffectStage::Geometry, 0),
            registry.insertion_index(&layers, EffectStage::Finish, 0),
        ];
        assert_eq!(placement, expected, "{case}");
        assert_eq!(
            registry.insertion_index(&layers, EffectStage::Source, 0),
            0,
            "{case}: a source layer prepares the content stage"
        );
        // The same answers through the effects' own descriptors.
        assert_eq!(
            [
                registry.insertion_index_for(&layers, PIXEL_EFFECT),
                registry.insertion_index_for(&layers, BASIC_EFFECT),
                registry.insertion_index_for(&layers, MIXER_EFFECT),
                registry.insertion_index_for(&layers, SPATIAL_EFFECT),
                registry.insertion_index_for(&layers, CROP_EFFECT),
                registry.insertion_index_for(&layers, FINISH_EFFECT),
            ],
            expected,
            "{case}: read from each effect's descriptor"
        );
    }

    // Committing the two colour orders in either sequence leaves the same stack.
    let mut committed_low_first = vec![basic()];
    committed_low_first.insert(
        registry.insertion_index_for(&committed_low_first, MIXER_EFFECT),
        mixer(),
    );
    let mut committed_high_first = vec![mixer()];
    committed_high_first.insert(
        registry.insertion_index_for(&committed_high_first, BASIC_EFFECT),
        basic(),
    );
    for (case, stack) in [
        ("order 0 first", committed_low_first),
        ("order 10 first", committed_high_first),
    ] {
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            vec![BASIC_EFFECT, MIXER_EFFECT],
            "{case}: the declared order decides, not the commit sequence"
        );
    }

    // `module.list` reports the stage and the order of every effect.
    let descriptors = serde_json::to_value(registry.descriptors()).expect("descriptor JSON");
    let effect_of = |module: &str| {
        descriptors
            .as_array()
            .expect("an array")
            .iter()
            .find(|descriptor| descriptor["id"] == json!(module))
            .expect("a registered module")["effects"][0]
            .clone()
    };
    assert_eq!(
        effect_of("test.mixer"),
        json!({"id": MIXER_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "order": 10})
    );
    assert_eq!(
        effect_of("test.spatial"),
        json!({"id": SPATIAL_EFFECT, "format": EFFECT_FORMAT, "stage": "spatial", "order": 0})
    );
    assert_eq!(
        effect_of("test.finish"),
        json!({"id": FINISH_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0})
    );
}
