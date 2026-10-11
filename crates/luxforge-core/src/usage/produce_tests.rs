//! The producers' shared pieces: which tool groups a transaction changed, which interaction a
//! mask command is, and the image IDs kept for adopted photographs.
use super::*;
use crate::{
    Layer, ModuleRegistry,
    modules::{BASIC_EFFECT, CROP_EFFECT, VIGNETTE_EFFECT},
    usage::{ImageKey, SessionClock, SessionStart, Trigger},
};
use serde_json::json;

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        layers,
        ..Recipe::default()
    }
}

#[test]
fn usage_changed_tools_names_each_group_whose_layers_differ_and_ignores_identity() {
    let registry = ModuleRegistry::builtin();
    let basic = Layer::new(BASIC_EFFECT, json!({"exposure": 0.5}));
    let crop = Layer::new(CROP_EFFECT, json!({"x": 0.1}));
    let before = recipe(vec![basic.clone(), crop.clone()]);

    // The same stack with new layer identities changes nothing.
    let renumbered = recipe(vec![
        Layer::new(BASIC_EFFECT, basic.payload.clone()),
        Layer::new(CROP_EFFECT, crop.payload.clone()),
    ]);
    assert!(changed_tools(&registry, &before, &renumbered).is_empty());

    // A changed payload is its group's change alone.
    let exposed = recipe(vec![
        Layer::new(BASIC_EFFECT, json!({"exposure": 1.0})),
        crop.clone(),
    ]);
    let changed = changed_tools(&registry, &before, &exposed);
    assert_eq!(changed.iter().collect::<Vec<_>>(), [Tool::Basic]);

    // A layer added and one removed, in one transaction: two groups, in the registry's order.
    let both = recipe(vec![
        basic,
        Layer::new(VIGNETTE_EFFECT, json!({"amount": -20})),
    ]);
    let changed = changed_tools(&registry, &before, &both);
    assert_eq!(
        changed.iter().collect::<Vec<_>>(),
        [Tool::Crop, Tool::Vignette]
    );

    // An effect no built-in tool provides is no tool's change.
    let proof = recipe(vec![Layer::new("luxforge.pixel.replace", json!({}))]);
    assert!(changed_tools(&registry, &recipe(vec![]), &proof).is_empty());
}

#[test]
fn usage_a_mask_command_maps_to_its_interaction_and_nothing_else_does() {
    for (action, feature) in [
        ("mask.create-linear", Feature::MaskCreate),
        ("mask.create-colour-range", Feature::MaskCreate),
        ("mask.add-radial", Feature::MaskComponentAdd),
        ("mask.add-stroke", Feature::MaskEdit),
        ("mask.delete-stroke", Feature::MaskEdit),
        ("mask.add-colour-range-sample", Feature::MaskEdit),
        ("mask.delete-colour-range-sample", Feature::MaskEdit),
        ("mask.set-linear", Feature::MaskEdit),
        ("mask.set-amount", Feature::MaskEdit),
        ("mask.set-component-invert", Feature::MaskEdit),
        ("mask.delete", Feature::MaskDelete),
        ("mask.delete-component", Feature::MaskComponentDelete),
        ("mask.duplicate", Feature::MaskDuplicate),
        ("mask.rename", Feature::MaskRename),
        ("mask.rename-component", Feature::MaskRename),
        ("mask.reorder", Feature::MaskReorder),
        ("mask.reorder-component", Feature::MaskReorder),
    ] {
        assert_eq!(mask_feature(action), Some(feature), "{action}");
    }
    for other in [
        "set-basic",
        "apply-settings",
        "mask.list",
        "mask.sample-input",
    ] {
        assert_eq!(mask_feature(other), None, "{other}");
    }
}

#[test]
fn usage_an_adopted_photograph_keeps_its_image_id_for_its_generation_only() {
    let collector = Arc::new(Collector::new());
    let mut producer = Producer::new(collector.clone());
    let asset = AssetId::new();
    let fingerprint = "c".repeat(64);
    assert_eq!(producer.on(), None);
    assert_eq!(
        producer.adopt_if_on(&asset, &fingerprint),
        None,
        "off: nothing"
    );

    collector.start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::manual().0,
        ImageKey::from_bytes([9; 32]),
    );
    let generation = producer.on().unwrap();
    let image = producer.adopt(generation, &asset, &fingerprint).unwrap();
    assert_eq!(producer.open_image(generation, asset.as_str()), Some(image));
    assert_eq!(
        producer.image_of(generation, &AssetId::new(), &fingerprint),
        Some(image),
        "a copy of the same bytes, another photograph row, has the same ID"
    );
    // Another generation: the kept ID is not used, and the new key gives another.
    collector.stop();
    collector.start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::manual().0,
        ImageKey::from_bytes([10; 32]),
    );
    let next = producer.on().unwrap();
    assert_eq!(producer.open_image(next, asset.as_str()), None);
    assert_ne!(producer.adopt(next, &asset, &fingerprint), Some(image));
}

impl Producer {
    fn adopt_if_on(&mut self, asset: &AssetId, fingerprint: &str) -> Option<ImageId> {
        let generation = self.on()?;
        self.adopt(generation, asset, fingerprint)
    }
}
