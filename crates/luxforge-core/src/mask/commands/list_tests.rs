//! Tests of `mask.list`'s listing.
use super::plan::strokes_payload;
use super::tests::{created, registry};
use super::*;
use crate::mask::stroke_kind;
use crate::{
    Component, ComponentMode, Layer, LayerId, Mask, Recipe,
    path::{self, Stroke, StrokeId},
};
use serde_json::json;

#[test]
fn the_listing_reports_values_components_and_the_bound_layers() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let mut recipe = recipe;
    let layer = Layer {
        id: LayerId::new(),
        effect_id: crate::BASIC_EFFECT.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: Some(mask.clone()),
        artifacts: Vec::new(),
    };
    recipe.layers.push(layer.clone());
    // A kind this build cannot evaluate is listed, kept and reported as unavailable.
    let mut future = Mask::new("Future");
    let name = future.next_component_name("cloud");
    future
        .components
        .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
    recipe.masks.push(future);
    let entry = crate::EntryId::new();
    let listed = listing(entry.clone(), &recipe, &registry());
    assert_eq!(listed.entry_id, entry);
    assert_eq!(listed.masks.len(), 2);
    let report = &listed.masks[0];
    assert_eq!(report.index, 0);
    assert_eq!(report.name, "Mask 1");
    assert_eq!(report.amount, Mask::FULL_AMOUNT);
    assert!(!report.invert);
    assert_eq!(report.components.len(), 1);
    assert_eq!(report.components[0].name, "Linear 1");
    assert_eq!(report.components[0].kind, "linear");
    assert!(report.components[0].available);
    assert_eq!(
        report.components[0].payload,
        json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
    );
    assert_eq!(report.layers.len(), 1);
    assert_eq!(report.layers[0].id, layer.id);
    assert_eq!(report.layers[0].title.as_deref(), Some("Basic"));
    assert_eq!(listed.masks[1].components[0].name, "Cloud 1");
    assert!(
        !listed.masks[1].components[0].available,
        "a kind this build does not know is named, kept and reported"
    );
    assert!(listed.masks[1].layers.is_empty());
}

#[test]
fn the_listing_reports_each_strokes_settings_in_stored_order() {
    // Sizes on the stored grid, so the reported radius is the posted one exactly; any other
    // size is reported as the grid step it was stored at.
    let mut recipe = Recipe::default();
    let add = Stroke::capture(&[[0.2, 0.2], [0.4, 0.4]], 0.0625, 50.0, 100.0, false).unwrap();
    let erase = Stroke::capture(&[[0.3, 0.3], [0.5, 0.2]], 0.03125, 30.0, 80.0, true)
        .unwrap()
        .with_colour_limit(path::ColourLimit::sampled([200, 120, 40], 50.0).unwrap());
    let add = recipe.strokes.insert(add);
    let erase = recipe.strokes.insert(erase);
    // A reference the store does not hold is listed with no settings rather than dropped.
    let missing = StrokeId::parse("0".repeat(32)).unwrap();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name(stroke_kind());
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        stroke_kind(),
        strokes_payload(&[add.clone(), erase.clone(), missing.clone()]),
    ));
    recipe.masks.push(mask);
    let listed = listing(crate::EntryId::new(), &recipe, &registry());
    let strokes = &listed.masks[0].components[0].strokes;
    assert_eq!(
        strokes.iter().map(|stroke| &stroke.id).collect::<Vec<_>>(),
        [&add, &erase, &missing]
    );
    assert_eq!(
        strokes[0].settings,
        Some(StrokeSettings {
            erase: false,
            size: 0.0625,
            feather: 50.0,
            flow: 100.0,
            colour: None,
        })
    );
    assert_eq!(
        strokes[1].settings,
        Some(StrokeSettings {
            erase: true,
            size: 0.03125,
            feather: 30.0,
            flow: 80.0,
            colour: Some(StrokeColour {
                seed: [200, 120, 40],
                refine: 50.0,
            }),
        })
    );
    assert_eq!(strokes[2].settings, None);
    let spelled = serde_json::to_value(&strokes[1]).unwrap();
    assert_eq!(
        spelled,
        json!({"id": erase.as_str(), "settings": {"erase": true, "size": 0.03125, "feather": 30.0,
            "flow": 80.0, "colour": {"seed": [200, 120, 40], "refine": 50.0}}})
    );
    // A component whose geometry is declared as numbers references no stroke.
    let (gradient, _) = created();
    let listed = listing(crate::EntryId::new(), &gradient, &registry());
    assert!(listed.masks[0].components[0].strokes.is_empty());
}
