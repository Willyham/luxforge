//! Tests of what each command does to a stack, and the history label it commits.
use super::tests::{apply, created, linear, plan, radial, registry};
use super::*;
use crate::{
    Component, ComponentId, ComponentMode, ErrorKind, Layer, LayerId, Mask, MaskId, Recipe,
    model::{COMPONENTS_PER_MASK, MASKS_PER_RECIPE},
};
use serde_json::{Map, Value, json};

#[test]
fn a_created_mask_carries_one_add_component_named_from_its_kind() {
    let (recipe, change) = created();
    assert_eq!(recipe.masks.len(), 1);
    let mask = &recipe.masks[0];
    assert_eq!(mask.name, "Mask 1");
    assert_eq!(mask.amount, Mask::FULL_AMOUNT);
    assert!(!mask.invert);
    assert_eq!(mask.components.len(), 1);
    assert_eq!(mask.components[0].name, "Linear 1");
    assert_eq!(mask.components[0].mode, ComponentMode::Add);
    assert_eq!(
        mask.components[0].payload,
        json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":1.0})
    );
    assert_eq!(change.label, "Add linear", "one mask needs no prefix");
    assert_eq!(change.mask.as_ref(), Some(&mask.id));
    assert_eq!(change.component.as_ref(), Some(&mask.components[0].id));
}

#[test]
fn an_ordinal_is_never_reused_so_one_label_means_one_component() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let target = MaskTarget {
        mask: Some(mask.clone()),
        ..MaskTarget::default()
    };
    let mut add = linear(0.2, 0.0, 0.8, 1.0);
    add.insert("mode".into(), json!("subtract"));
    let second = apply(&recipe, "mask.add-linear", target.clone(), add).unwrap();
    assert_eq!(second.recipe.masks[0].components[1].name, "Linear 2");
    assert_eq!(second.label, "Add subtract linear");
    // Delete the second and add another of the same kind: the freed ordinal is not reused.
    let component = second.recipe.masks[0].components[1].id.clone();
    let deleted = apply(
        &second.recipe,
        "mask.delete-component",
        MaskTarget {
            component: Some(component),
            ..target.clone()
        },
        Map::new(),
    )
    .unwrap();
    assert_eq!(deleted.label, "Delete Linear 2");
    assert_eq!(deleted.recipe.masks[0].components.len(), 1);
    let mut again = linear(0.3, 0.0, 0.9, 1.0);
    again.insert("mode".into(), json!("intersect"));
    let third = apply(&deleted.recipe, "mask.add-linear", target, again).unwrap();
    assert_eq!(
        third.recipe.masks[0].components[1].name, "Linear 3",
        "an entry reading Update Linear 2 can only ever mean the component it was written about"
    );
}

#[test]
fn a_later_edit_names_the_component_and_a_second_mask_names_the_mask() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let component = recipe.masks[0].components[0].id.clone();
    let target = MaskTarget {
        mask: Some(mask.clone()),
        component: Some(component.clone()),
        ..MaskTarget::default()
    };
    let mut patch = Map::new();
    patch.insert("y1".into(), json!(0.6));
    let updated = apply(&recipe, "mask.set-linear", target.clone(), patch.clone()).unwrap();
    assert_eq!(updated.label, "Update Linear 1");
    assert_eq!(
        updated.recipe.masks[0].components[0].payload,
        json!({"x0":0.0,"y0":0.0,"x1":0.0,"y1":0.6}),
        "a patch merges over the stored payload"
    );
    // A second mask exists, so every row that does not already name its mask names it.
    let two = apply(
        &updated.recipe,
        "mask.create-linear",
        MaskTarget::default(),
        linear(1.0, 0.0, 1.0, 1.0),
    )
    .unwrap();
    assert_eq!(two.label, "Mask 2 · Add linear");
    let again = apply(&two.recipe, "mask.set-linear", target, {
        let mut patch = Map::new();
        patch.insert("y1".into(), json!(0.4));
        patch
    })
    .unwrap();
    assert_eq!(again.label, "Mask 1 · Update Linear 1");
    // A component rename does not already name the mask, so it is prefixed exactly as an
    // ordinary component edit is once a second mask exists.
    let renamed = apply(
        &again.recipe,
        "mask.rename-component",
        MaskTarget {
            mask: Some(mask),
            component: Some(component),
            name: Some("Sky edge".into()),
            ..MaskTarget::default()
        },
        Map::new(),
    )
    .unwrap();
    assert_eq!(renamed.label, "Mask 1 · Rename Linear 1 to Sky edge");
}

#[test]
fn every_row_of_the_designs_granularity_table_reads_as_it_states() {
    let (one, create) = created();
    assert_eq!(create.label, "Add linear");
    let mask = one.masks[0].id.clone();
    let component = one.masks[0].components[0].id.clone();
    let of_mask = MaskTarget {
        mask: Some(mask.clone()),
        ..MaskTarget::default()
    };
    let of_component = MaskTarget {
        component: Some(component),
        ..of_mask.clone()
    };
    // "Drag a radial's handle" → `Update Radial 1`: the same rule over the kind that exists.
    let dragged = apply(&one, "mask.set-linear", of_component.clone(), {
        let mut patch = Map::new();
        patch.insert("x1".into(), json!(0.5));
        patch
    })
    .unwrap();
    assert_eq!(dragged.label, "Update Linear 1");
    // "Add a subtract brush to the same mask" → `Add subtract brush`.
    let mut add = linear(0.1, 0.1, 0.9, 0.9);
    add.insert("mode".into(), json!("subtract"));
    let added = apply(&dragged.recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
    assert_eq!(added.label, "Add subtract linear");
    // "Change Brush 2 to intersect" → `Brush 2 intersect`.
    let second = added.recipe.masks[0].components[1].id.clone();
    let mut mode = Map::new();
    mode.insert("mode".into(), json!("intersect"));
    let changed = apply(
        &added.recipe,
        "mask.set-component-mode",
        MaskTarget {
            component: Some(second),
            ..of_mask.clone()
        },
        mode,
    )
    .unwrap();
    assert_eq!(changed.label, "Linear 2 intersect");
    // The whole-mask modifiers and the component inversion take the same shape.
    let mut amount = Map::new();
    amount.insert("amount".into(), json!(60.0));
    assert_eq!(
        apply(&one, "mask.set-amount", of_mask.clone(), amount)
            .unwrap()
            .label,
        "Amount 60"
    );
    let mut invert = Map::new();
    invert.insert("invert".into(), json!(true));
    assert_eq!(
        apply(&one, "mask.set-invert", of_mask, invert.clone())
            .unwrap()
            .label,
        "Inverted"
    );
    assert_eq!(
        apply(&one, "mask.set-component-invert", of_component, invert)
            .unwrap()
            .label,
        "Linear 1 inverted"
    );
}

/// The correction's own property, at the command level: a radial is created, added as a second
/// component of another kind's mask, and patched on a field no position range could carry.
#[test]
fn a_radial_is_created_added_and_patched_through_its_own_generated_methods() {
    let recipe = Recipe::default();
    let created = apply(
        &recipe,
        "mask.create-radial",
        MaskTarget::default(),
        radial(0.5, 0.5, 0.3, 40.0),
    )
    .unwrap();
    assert_eq!(created.label, "Add radial");
    let mask = &created.recipe.masks[0];
    assert_eq!(mask.components[0].name, "Radial 1");
    assert_eq!(mask.components[0].kind, "radial");
    assert_eq!(mask.components[0].mode, ComponentMode::Add);
    assert_eq!(
        mask.components[0].payload,
        json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.3,"angle":0.0,"feather":40.0})
    );
    // A linear joins the same mask, subtracting: two kinds, one component list.
    let of_mask = MaskTarget {
        mask: Some(mask.id.clone()),
        ..MaskTarget::default()
    };
    let mut add = linear(0.1, 0.1, 0.9, 0.9);
    add.insert("mode".into(), json!("subtract"));
    let two = apply(&created.recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
    assert_eq!(two.label, "Add subtract linear");
    assert_eq!(
        two.recipe.masks[0].components[1].name, "Linear 1",
        "the ordinal counter is per kind, so a mask's first linear is Linear 1 whatever else it holds"
    );
    // And the radial is patched on a radius, an angle and a feather — the three fields the
    // delivered single geometry table could not express at all.
    let of_radial = MaskTarget {
        component: Some(two.recipe.masks[0].components[0].id.clone()),
        ..of_mask.clone()
    };
    let mut patch = Map::new();
    patch.insert("radius_y".into(), json!(0.45));
    patch.insert("angle".into(), json!(-30.0));
    patch.insert("feather".into(), json!(0.0));
    let patched = apply(&two.recipe, "mask.set-radial", of_radial.clone(), patch).unwrap();
    assert_eq!(patched.label, "Update Radial 1");
    assert_eq!(
        patched.recipe.masks[0].components[0].payload,
        json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.45,"angle":-30.0,"feather":0.0}),
        "a patch merges over the stored payload"
    );
    // One kind's patch may not reach another kind's component, and the refusal names both.
    let of_linear = MaskTarget {
        component: Some(two.recipe.masks[0].components[1].id.clone()),
        ..of_mask
    };
    let mut wrong = Map::new();
    wrong.insert("radius_x".into(), json!(0.2));
    assert_eq!(
        plan(
            find("mask.set-radial").unwrap(),
            &two.recipe,
            &of_linear,
            &wrong,
            &registry()
        )
        .unwrap_err()
        .detail,
        "component Linear 1 is a linear component; patch it with mask.set-linear"
    );
}

#[test]
fn a_change_that_changes_nothing_writes_no_entry() {
    let (recipe, _) = created();
    let target = MaskTarget {
        mask: Some(recipe.masks[0].id.clone()),
        component: Some(recipe.masks[0].components[0].id.clone()),
        ..MaskTarget::default()
    };
    let command = find("mask.set-linear").unwrap();
    // The drag ended where it began: the stored payload, in either JSON spelling of a number.
    for value in [json!(1.0), json!(1)] {
        let mut patch = Map::new();
        patch.insert("y1".into(), value);
        assert!(
            matches!(
                plan(command, &recipe, &target, &patch, &registry()).unwrap(),
                MaskOutcome::NoOp
            ),
            "returning to the start is a no-op"
        );
    }
    for (method, name, value) in [
        ("mask.set-amount", "amount", json!(100.0)),
        ("mask.set-invert", "invert", json!(false)),
    ] {
        let mut parameters = Map::new();
        parameters.insert(name.into(), value);
        let command = find(method).unwrap();
        let target = MaskTarget {
            mask: Some(recipe.masks[0].id.clone()),
            ..MaskTarget::default()
        };
        assert!(matches!(
            plan(command, &recipe, &target, &parameters, &registry()).unwrap(),
            MaskOutcome::NoOp
        ));
    }
}

#[test]
fn a_mask_never_exists_empty_and_never_begins_by_subtracting() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let component = recipe.masks[0].components[0].id.clone();
    let target = MaskTarget {
        mask: Some(mask),
        component: Some(component),
        ..MaskTarget::default()
    };
    let only = find("mask.delete-component").unwrap();
    assert_eq!(
        plan(only, &recipe, &target, &Map::new(), &registry())
            .unwrap_err()
            .detail,
        "mask Mask 1 has one component; delete the mask rather than its last component"
    );
    let mut mode = Map::new();
    mode.insert("mode".into(), json!("subtract"));
    assert_eq!(
        plan(
            find("mask.set-component-mode").unwrap(),
            &recipe,
            &target,
            &mode,
            &registry()
        )
        .unwrap_err()
        .detail,
        "mask Mask 1 begins with a subtract component; the first component of a mask is always \
         add"
    );
}

#[test]
fn a_reorder_that_puts_a_subtract_first_is_refused_with_the_models_reason() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let of_mask = MaskTarget {
        mask: Some(mask),
        ..MaskTarget::default()
    };
    let mut add = linear(0.1, 0.1, 0.9, 0.9);
    add.insert("mode".into(), json!("subtract"));
    let two = apply(&recipe, "mask.add-linear", of_mask.clone(), add).unwrap();
    let second = two.recipe.masks[0].components[1].id.clone();
    let mut index = Map::new();
    index.insert("index".into(), json!(0));
    let error = plan(
        find("mask.reorder-component").unwrap(),
        &two.recipe,
        &MaskTarget {
            component: Some(second.clone()),
            ..of_mask.clone()
        },
        &index,
        &registry(),
    )
    .unwrap_err();
    assert_eq!(
        error.detail,
        "mask Mask 1 begins with a subtract component; the first component of a mask is always \
         add"
    );
    // Deleting the leading add would leave the same unreadable mask, so it is refused too.
    let first = two.recipe.masks[0].components[0].id.clone();
    assert_eq!(
        plan(
            find("mask.delete-component").unwrap(),
            &two.recipe,
            &MaskTarget {
                component: Some(first),
                ..of_mask.clone()
            },
            &Map::new(),
            &registry()
        )
        .unwrap_err()
        .detail,
        "mask Mask 1 begins with a subtract component; the first component of a mask is always \
         add"
    );
    // Moving it to the position it already holds changes nothing at all.
    let mut index = Map::new();
    index.insert("index".into(), json!(1));
    assert!(
        matches!(
            plan(
                find("mask.reorder-component").unwrap(),
                &two.recipe,
                &MaskTarget {
                    component: Some(second),
                    ..of_mask
                },
                &index,
                &registry()
            )
            .unwrap(),
            MaskOutcome::NoOp
        ),
        "moving a component to the position it holds is a no-op"
    );
}

/// Both declared limits refuse with a `resource-limit` error that names the count and the
/// limit, and nothing is written. A limit is a refusal, never a silent truncation and never a
/// catalog that grows without one.
#[test]
fn a_list_at_its_limit_and_a_mask_at_its_limit_are_refused_by_name() {
    // Components per mask: fill one to the limit through the command that fills it.
    let (mut recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let of_mask = MaskTarget {
        mask: Some(mask.clone()),
        ..MaskTarget::default()
    };
    while recipe.masks[0].components.len() < COMPONENTS_PER_MASK {
        let mut add = linear(0.1, 0.1, 0.9, 0.9);
        add.insert("mode".into(), json!("add"));
        recipe = apply(&recipe, "mask.add-linear", of_mask.clone(), add)
            .unwrap()
            .recipe;
    }
    let mut add = radial(0.5, 0.5, 0.3, 40.0);
    add.insert("mode".into(), json!("add"));
    let error = plan(
        find("mask.add-radial").unwrap(),
        &recipe,
        &of_mask,
        &add,
        &registry(),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "mask Mask 1 has {COMPONENTS_PER_MASK} components; the limit is \
             {COMPONENTS_PER_MASK} components per mask"
        )
    );

    // Masks per recipe: every creating command refuses at the limit, the duplicate included, and
    // the duplicate refuses before it copies a single layer.
    let mut recipe = Recipe::default();
    while recipe.masks.len() < MASKS_PER_RECIPE {
        recipe = apply(
            &recipe,
            "mask.create-linear",
            MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
        )
        .unwrap()
        .recipe;
    }
    recipe.layers.push(Layer {
        id: LayerId::new(),
        effect_id: crate::BASIC_EFFECT.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: Some(recipe.masks[0].id.clone()),
        artifacts: Vec::new(),
    });
    let full = format!(
        "recipe already has {MASKS_PER_RECIPE} masks; the limit is {MASKS_PER_RECIPE} masks \
         per recipe"
    );
    for (method, target, parameters) in [
        (
            "mask.create-linear",
            MaskTarget::default(),
            linear(0.5, 0.0, 0.5, 1.0),
        ),
        (
            "mask.create-radial",
            MaskTarget::default(),
            radial(0.5, 0.5, 0.3, 40.0),
        ),
        (
            "mask.duplicate",
            MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                ..MaskTarget::default()
            },
            Map::new(),
        ),
    ] {
        let error = plan(
            find(method).unwrap(),
            &recipe,
            &target,
            &parameters,
            &registry(),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit, "{method}");
        assert_eq!(error.detail, full, "{method}");
    }
}

#[test]
fn refusals_name_what_they_refuse() {
    let (recipe, _) = created();
    let stranger = Mask::new("Mask 9");
    assert_eq!(
        plan(
            find("mask.set-invert").unwrap(),
            &recipe,
            &MaskTarget {
                mask: Some(stranger.id.clone()),
                ..MaskTarget::default()
            },
            &{
                let mut fields = Map::new();
                fields.insert("invert".into(), json!(true));
                fields
            },
            &registry()
        )
        .unwrap_err()
        .detail,
        format!("unknown mask {}", stranger.id)
    );
    let absent = ComponentId::new();
    assert_eq!(
        plan(
            find("mask.set-linear").unwrap(),
            &recipe,
            &MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                component: Some(absent.clone()),
                ..MaskTarget::default()
            },
            &Map::new(),
            &registry()
        )
        .unwrap_err()
        .detail,
        format!("mask Mask 1 has no component {absent}")
    );
    // The identities are declared parameters, checked by the generic check before anything is
    // planned: required where a command addresses one, unknown where it does not, and of their
    // own kind.
    let delete = &find("mask.delete").unwrap().action;
    assert_eq!(
        crate::check_parameters(delete, &json!({}))
            .unwrap_err()
            .detail,
        "missing required parameter mask for action mask.delete"
    );
    assert_eq!(
        crate::check_parameters(
            &find("mask.create-linear").unwrap().action,
            &MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                ..MaskTarget::default()
            }
            .request(Value::Object(linear(0.0, 0.0, 0.0, 1.0)))
        )
        .unwrap_err()
        .detail,
        "unknown parameter mask for action mask.create-linear"
    );
    assert_eq!(
        crate::check_parameters(delete, &json!({"mask": ComponentId::new().as_str()}))
            .unwrap_err()
            .detail,
        "parameter mask must be a mask identity"
    );
}

#[test]
fn a_rename_names_both_names_and_a_duplicate_takes_new_identities() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let of_mask = MaskTarget {
        mask: Some(mask.clone()),
        ..MaskTarget::default()
    };
    let renamed = apply(
        &recipe,
        "mask.rename",
        MaskTarget {
            name: Some("Sky".into()),
            ..of_mask.clone()
        },
        Map::new(),
    )
    .unwrap();
    assert_eq!(renamed.label, "Rename Mask 1 to Sky");
    assert_eq!(renamed.recipe.masks[0].name, "Sky");
    // A rename to the name a mask already has changes nothing.
    assert!(matches!(
        plan(
            find("mask.rename").unwrap(),
            &renamed.recipe,
            &MaskTarget {
                name: Some("Sky".into()),
                ..of_mask.clone()
            },
            &Map::new(),
            &registry()
        )
        .unwrap(),
        MaskOutcome::NoOp
    ));
    let copied = apply(&renamed.recipe, "mask.duplicate", of_mask, Map::new()).unwrap();
    assert_eq!(copied.label, "Duplicate Sky");
    assert_eq!(copied.recipe.masks.len(), 2);
    let (source, copy) = (&copied.recipe.masks[0], &copied.recipe.masks[1]);
    assert_ne!(source.id, copy.id);
    assert_ne!(source.components[0].id, copy.components[0].id);
    assert_eq!(copy.name, "Mask 1", "the lowest unused default name");
    assert_eq!(copy.components[0].payload, source.components[0].payload);
    assert_eq!(
        copy.components[0].name, "Linear 1",
        "component names are unique within a mask, not across masks"
    );
    assert_eq!(
        copy.next_ordinal, source.next_ordinal,
        "the copy's next linear is Linear 2, because Linear 1 already names one of its own"
    );
}

/// `mask.rename-component` mirrors `mask.rename`: the same name shape rule, the same no-op on
/// the name a component already has, and — unlike a mask, whose name carries no uniqueness rule
/// across the recipe — a refusal naming a sibling's name, because `Mask::validate` requires every
/// component of one mask to have a distinct name.
#[test]
fn a_component_rename_names_it_and_is_refused_by_the_same_rules_a_mask_rename_is() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let component = recipe.masks[0].components[0].id.clone();
    let target = MaskTarget {
        mask: Some(mask.clone()),
        component: Some(component),
        ..MaskTarget::default()
    };
    let renamed = apply(
        &recipe,
        "mask.rename-component",
        MaskTarget {
            name: Some("Sky edge".into()),
            ..target.clone()
        },
        Map::new(),
    )
    .unwrap();
    assert_eq!(renamed.label, "Rename Linear 1 to Sky edge");
    assert_eq!(renamed.recipe.masks[0].components[0].name, "Sky edge");
    // A rename to the name a component already has changes nothing, exactly as a mask's own.
    assert!(matches!(
        plan(
            find("mask.rename-component").unwrap(),
            &renamed.recipe,
            &MaskTarget {
                name: Some("Sky edge".into()),
                ..target.clone()
            },
            &Map::new(),
            &registry()
        )
        .unwrap(),
        MaskOutcome::NoOp
    ));
    // Empty and over-long names are refused by the same helper a mask's own name is, worded for
    // a component instead.
    let empty = plan(
        find("mask.rename-component").unwrap(),
        &renamed.recipe,
        &MaskTarget {
            name: Some(String::new()),
            ..target.clone()
        },
        &Map::new(),
        &registry(),
    )
    .unwrap_err();
    assert_eq!(
        empty.detail,
        format!(
            "component name must contain 1..={} printable characters",
            crate::MAX_MASK_NAME
        )
    );
    let too_long = plan(
        find("mask.rename-component").unwrap(),
        &renamed.recipe,
        &MaskTarget {
            name: Some("x".repeat(crate::MAX_MASK_NAME + 1)),
            ..target.clone()
        },
        &Map::new(),
        &registry(),
    )
    .unwrap_err();
    assert_eq!(too_long.detail, empty.detail);
    // Unknown mask and unknown component are the family's own shared refusals, reached exactly
    // as every other component command reaches them.
    let stranger = MaskId::new();
    assert_eq!(
        plan(
            find("mask.rename-component").unwrap(),
            &renamed.recipe,
            &MaskTarget {
                mask: Some(stranger.clone()),
                name: Some("Anything".into()),
                ..MaskTarget::default()
            },
            &Map::new(),
            &registry()
        )
        .unwrap_err()
        .detail,
        format!("unknown mask {stranger}")
    );
    let absent = ComponentId::new();
    assert_eq!(
        plan(
            find("mask.rename-component").unwrap(),
            &renamed.recipe,
            &MaskTarget {
                mask: Some(mask.clone()),
                component: Some(absent.clone()),
                name: Some("Anything".into()),
                ..MaskTarget::default()
            },
            &Map::new(),
            &registry()
        )
        .unwrap_err()
        .detail,
        format!("mask Mask 1 has no component {absent}")
    );
    // A second component of the same mask cannot take the name the first already holds.
    let mut second = radial(0.5, 0.5, 0.3, 10.0);
    second.insert("mode".into(), json!("add"));
    let with_radial = apply(
        &renamed.recipe,
        "mask.add-radial",
        MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        },
        second,
    )
    .unwrap();
    let radial_component = with_radial.component.clone().unwrap();
    assert_eq!(
        plan(
            find("mask.rename-component").unwrap(),
            &with_radial.recipe,
            &MaskTarget {
                mask: Some(mask),
                component: Some(radial_component),
                name: Some("Sky edge".into()),
                ..MaskTarget::default()
            },
            &Map::new(),
            &registry()
        )
        .unwrap_err()
        .detail,
        "duplicate component name Sky edge in mask Mask 1"
    );
}

/// A mask without its adjustments is not a useful copy, so `mask.duplicate` copies the layers
/// bound to the mask as well — each with a new identity, bound to the copy, and placed by the
/// ordering rule: after the global layer of its effect and in mask order among the masked ones.
#[test]
fn a_duplicate_copies_the_layers_bound_to_the_mask() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let mut recipe = recipe;
    let layer = |effect: &str, bound: Option<&MaskId>| Layer {
        id: LayerId::new(),
        effect_id: effect.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: bound.cloned(),
        artifacts: Vec::new(),
    };
    let global = layer(crate::BASIC_EFFECT, None);
    recipe.layers = vec![
        global.clone(),
        layer(crate::BASIC_EFFECT, Some(&mask)),
        layer(crate::PRESENCE_EFFECT, Some(&mask)),
    ];
    let copied = apply(
        &recipe,
        "mask.duplicate",
        MaskTarget {
            mask: Some(mask.clone()),
            ..MaskTarget::default()
        },
        Map::new(),
    )
    .unwrap();
    let copy = copied.mask.clone().expect("the duplicate names its copy");
    assert_eq!(copied.recipe.masks[1].id, copy);
    // Five layers: the global one, and each masked layer beside its copy.
    let targets: Vec<Option<MaskId>> = copied
        .recipe
        .layers
        .iter()
        .map(|layer| layer.mask.clone())
        .collect();
    assert_eq!(
        targets,
        vec![
            None,
            Some(mask.clone()),
            Some(copy.clone()),
            Some(mask.clone()),
            Some(copy.clone()),
        ],
        "each copy follows the layer it was copied from, which is the ordering rule"
    );
    assert_eq!(
        copied.recipe.layers[2].effect_id,
        crate::BASIC_EFFECT,
        "a copy keeps its source's effect"
    );
    assert_eq!(
        copied.recipe.layers[2].payload, copied.recipe.layers[1].payload,
        "a copy keeps its source's payload"
    );
    assert_ne!(
        copied.recipe.layers[2].id, copied.recipe.layers[1].id,
        "a copy takes a new identity"
    );
    // The copies are legal: `single_layer` is per target and the two masks are two targets, so
    // the whole stack compiles rather than failing as ambiguous.
    registry()
        .compile(400, 300, &copied.recipe)
        .expect("two masked layers of one effect on two masks are two targets");
    // And the order the placement rule would produce is the order it is already in.
    let mut sorted = copied.recipe.layers.clone();
    registry().sort_masked_layers(&mut sorted, &copied.recipe.masks);
    assert_eq!(
        sorted, copied.recipe.layers,
        "the copies are placed in the order the one re-sort rule states"
    );
}

#[test]
fn a_deleted_mask_takes_its_layers_and_says_which() {
    let (recipe, _) = created();
    let mask = recipe.masks[0].id.clone();
    let mut recipe = recipe;
    for effect in [crate::BASIC_EFFECT, crate::PRESENCE_EFFECT] {
        recipe.layers.push(Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: Some(mask.clone()),
            artifacts: Vec::new(),
        });
    }
    let global = Layer {
        id: LayerId::new(),
        effect_id: crate::BASIC_EFFECT.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: None,
        artifacts: Vec::new(),
    };
    recipe.layers.insert(0, global.clone());
    let deleted = apply(
        &recipe,
        "mask.delete",
        MaskTarget {
            mask: Some(mask),
            ..MaskTarget::default()
        },
        Map::new(),
    )
    .unwrap();
    assert_eq!(deleted.label, "Delete Mask 1 with Basic, Presence");
    assert_eq!(deleted.removed_layers.len(), 2);
    assert_eq!(
        deleted
            .removed_layers
            .iter()
            .map(|layer| layer.title.clone().unwrap())
            .collect::<Vec<_>>(),
        ["Basic", "Presence"]
    );
    assert_eq!(
        deleted.recipe.layers,
        vec![global],
        "the global layer of the same effect stays exactly where it was"
    );
    assert!(deleted.recipe.masks.is_empty());
}

#[test]
fn a_mask_move_resorts_the_masked_layers_and_moves_nothing_else() {
    let (recipe, _) = created();
    let first = recipe.masks[0].id.clone();
    let two = apply(
        &recipe,
        "mask.create-linear",
        MaskTarget::default(),
        linear(1.0, 0.0, 1.0, 1.0),
    )
    .unwrap();
    let second = two.recipe.masks[1].id.clone();
    let mut recipe = two.recipe.clone();
    let masked = |effect: &str, mask: &MaskId| Layer {
        id: LayerId::new(),
        effect_id: effect.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: Some(mask.clone()),
        artifacts: Vec::new(),
    };
    let global = Layer {
        id: LayerId::new(),
        effect_id: crate::BASIC_EFFECT.to_owned(),
        effect_format: crate::EFFECT_FORMAT,
        payload: json!({}),
        mask: None,
        artifacts: Vec::new(),
    };
    recipe.layers = vec![
        global.clone(),
        masked(crate::BASIC_EFFECT, &first),
        masked(crate::BASIC_EFFECT, &second),
        masked(crate::PRESENCE_EFFECT, &first),
        masked(crate::PRESENCE_EFFECT, &second),
    ];
    let before = recipe.layers.clone();
    let mut index = Map::new();
    index.insert("index".into(), json!(0));
    let moved = apply(
        &recipe,
        "mask.reorder",
        MaskTarget {
            mask: Some(second.clone()),
            ..MaskTarget::default()
        },
        index,
    )
    .unwrap();
    assert_eq!(moved.label, "Move Mask 2 to 1");
    assert_eq!(
        moved
            .recipe
            .masks
            .iter()
            .map(|mask| mask.name.as_str())
            .collect::<Vec<_>>(),
        ["Mask 2", "Mask 1"]
    );
    assert_eq!(
        moved.recipe.layers[0], global,
        "an unmasked layer never moves"
    );
    assert_eq!(
        moved.recipe.layers[1], before[2],
        "the masked Basic layers swap, in their masks' new order"
    );
    assert_eq!(moved.recipe.layers[2], before[1]);
    assert_eq!(
        moved.recipe.layers[3], before[4],
        "and so do the masked Presence layers, independently"
    );
    assert_eq!(moved.recipe.layers[4], before[3]);
}
#[test]
fn a_geometry_edit_on_a_kind_this_build_cannot_read_is_incompatible() {
    let mut recipe = Recipe::default();
    let mut future = Mask::new("Mask 1");
    let name = future.next_component_name("cloud");
    future
        .components
        .push(Component::new(name, ComponentMode::Add, "cloud", json!({})));
    let (mask, component) = (future.id.clone(), future.components[0].id.clone());
    recipe.masks.push(future);
    let mut patch = Map::new();
    patch.insert("x0".into(), json!(0.5));
    let error = plan(
        find("mask.set-linear").unwrap(),
        &recipe,
        &MaskTarget {
            mask: Some(mask),
            component: Some(component),
            ..MaskTarget::default()
        },
        &patch,
        &registry(),
    )
    .unwrap_err();
    assert_eq!(error.detail, "unknown mask component cloud");
    assert_eq!(error.kind, ErrorKind::Incompatible);
}

#[test]
fn an_index_outside_the_list_it_addresses_is_refused_with_the_count() {
    let (recipe, _) = created();
    let mut index = Map::new();
    index.insert("index".into(), json!(3));
    assert_eq!(
        plan(
            find("mask.reorder").unwrap(),
            &recipe,
            &MaskTarget {
                mask: Some(recipe.masks[0].id.clone()),
                ..MaskTarget::default()
            },
            &index,
            &registry()
        )
        .unwrap_err()
        .detail,
        "index 3 is outside the 1 masks of this stack"
    );
}
