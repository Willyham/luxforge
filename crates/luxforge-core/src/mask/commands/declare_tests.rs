//! Tests of what the family publishes: the command table, the controls, the host descriptor and
//! each command's schema.
use super::declare::geometry_controls;
use super::tests::registry;
use super::*;
use crate::mask::{
    component_band_control, component_geometry_is_drawn, component_parameters,
    declared_geometry_kinds, knows_component_kind, stroke_kind,
};
use crate::{
    ActionDescriptor, ChoiceControl, ComponentId, Control, ControlVariant, MaskId,
    ModuleDescriptor, NumberControl, NumberStyle, ParameterDescriptor, ParameterKind, RangeControl,
    ToggleControl,
};
use serde_json::{Value, json};

#[test]
fn the_declared_family_is_the_designs_method_table() {
    let methods: Vec<&str> = all().iter().map(|command| command.method).collect();
    assert_eq!(
        methods,
        [
            // The kind-independent commands, in the order the design's method table lists them.
            "mask.delete",
            "mask.rename",
            "mask.rename-component",
            "mask.duplicate",
            "mask.set-amount",
            "mask.set-invert",
            "mask.reorder",
            "mask.set-component-mode",
            "mask.set-component-invert",
            "mask.delete-component",
            "mask.reorder-component",
            // The brush's own two, declared rather than generated: a brush's geometry is drawn,
            // so it has no create, add or set method and these are the only way a stroke reaches
            // a mask.
            "mask.add-stroke",
            "mask.delete-stroke",
            // Then three geometry methods per registered kind, generated from the host's own
            // kind table and in its order.
            "mask.create-linear",
            "mask.add-linear",
            "mask.set-linear",
            "mask.create-radial",
            "mask.add-radial",
            "mask.set-radial",
            // Then the two range selections, which the same generation covers because their
            // geometry is declared as numbers.
            "mask.create-luminance-range",
            "mask.add-luminance-range",
            "mask.set-luminance-range",
            "mask.create-colour-range",
            "mask.add-colour-range",
            "mask.set-colour-range",
            // And the sample methods of the one kind that holds a list of picked colours.
            "mask.add-colour-range-sample",
            "mask.delete-colour-range-sample",
        ]
    );
    assert!(
        all()
            .iter()
            .all(|command| command.method == command.action.id),
        "a command's method name is its durable action identity"
    );
    assert!(
        all()
            .iter()
            .all(|command| !crate::valid_name(command.method)),
        "a mask command identity can never be a module action identity"
    );
    // `mask.list` and `mask.sample-input` read, so they are the host descriptor's queries and
    // never one of its actions; every command is an ordinary mutation.
    let reading: Vec<&str> = descriptor()
        .queries
        .iter()
        .map(|query| query.id.as_str())
        .collect();
    assert_eq!(reading, [LIST, SAMPLE_INPUT]);
    assert!(find(LIST).is_none() && find(SAMPLE_INPUT).is_none());
    // The host descriptor lists exactly the command table, in its order, and its controls.
    assert_eq!(descriptor().id, HOST_MODULE);
    assert_eq!(
        descriptor()
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        methods
    );
    assert_eq!(descriptor().controls.as_slice(), controls());
    assert!(descriptor().effects.is_empty(), "the host owns no layer");
}

/// Registering a kind with a sample limit is enough to make its swatches editable: the two
/// sample methods are generated, listed by `schema.list`, resolvable through the table by the
/// operation and the kind, and each declares exactly what one swatch or one removal takes.
///
/// The list itself is deliberately not a declared parameter — the closed vocabulary has numbers,
/// integers, enums, colours, booleans and curves and no list of any of them — so this is the
/// whole of how a client edits it, and nothing here names a kind.
#[test]
fn registering_a_sampling_kind_is_enough_to_make_its_swatches_editable() {
    let schemas = crate::schemas(&registry());
    let listed = schemas["methods"].as_object().expect("a method listing");
    let mut kinds = 0usize;
    for kind in crate::mask::sampling_kinds() {
        kinds += 1;
        let limit = crate::mask::component_sample_limit(kind).expect("a sampling kind");
        // The add method declares one parameter per channel of a sampled colour, each a finite
        // number, and nothing else: the swatch is the whole of its input.
        let add = sample(SampleOp::Add, kind).expect("its add method is generated");
        assert_eq!(add.method, format!("mask.add-{kind}-sample"));
        // A swatch is edited on one component of one mask, always both and never a name or a
        // stroke: the two identities come first, required, as they do for every command but
        // `mask.add-stroke`.
        let identities: Vec<(&str, bool)> = add
            .action
            .parameters
            .iter()
            .filter(|parameter| parameter.kind.is_identity())
            .map(|parameter| (parameter.name.as_str(), parameter.required))
            .collect();
        assert_eq!(identities, [("mask", true), ("component", true)]);
        assert!(!add.action.patch, "a swatch is appended, not patched");
        let declared: Vec<&str> = add
            .action
            .parameters
            .iter()
            .skip(2)
            .map(|parameter| parameter.name.as_str())
            .collect();
        assert_eq!(
            declared,
            crate::mask::component_sample_parameters(kind)
                .expect("a sampling kind")
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>()
        );
        for parameter in add.action.parameters.iter().skip(2) {
            let ParameterKind::Number { min, max } = parameter.kind else {
                panic!("{} is not a number", parameter.name);
            };
            assert!(min.is_finite() && max.is_finite() && min < max);
            assert!(parameter.unit.is_some() && parameter.step.is_some());
        }
        // The delete method takes the swatch's position, bounded by the kind's own limit, so a
        // request past the list is refused by the declaration rather than by the plan.
        let delete = sample(SampleOp::Delete, kind).expect("its delete method is generated");
        assert_eq!(delete.method, format!("mask.delete-{kind}-sample"));
        let index = delete
            .action
            .parameter("index")
            .expect("a position to remove");
        assert_eq!(
            index.kind,
            ParameterKind::Integer {
                min: 0,
                max: limit as i64 - 1
            }
        );
        for method in [add.method, delete.method] {
            assert!(listed.get(method).is_some(), "schema.list omits {method}");
            assert_eq!(
                find(method).map(|command| command.method),
                Some(method),
                "dispatch cannot resolve {method}"
            );
        }
    }
    assert_eq!(
        kinds, 1,
        "the colour range is this build's one kind that holds sampled colours"
    );
    // And the other side: a kind that samples nothing generates neither method and says so.
    for kind in crate::mask::component_kinds() {
        if crate::mask::component_sample_limit(kind).is_some() {
            continue;
        }
        assert!(sample(SampleOp::Add, kind).is_none(), "{kind}");
        assert!(sample(SampleOp::Delete, kind).is_none(), "{kind}");
        assert!(
            crate::mask::component_sample_parameters(kind).is_none(),
            "{kind}"
        );
    }
}

/// Registering a kind is **sufficient** to make it creatable, addable and patchable: for every
/// kind in the host's own table, all three geometry methods exist, are listed by `schema.list`,
/// and declare exactly that kind's parameters and no other kind's.
///
/// This is the property the delivered `GEOMETRY` table broke — it forced every field through a
/// normalized-position descriptor and listed only `linear`, so the radial gradient was evaluable
/// and not creatable. It is asserted over the table rather than over a list of names, so a kind
/// added later is covered by this test on the day it is registered.
#[test]
fn registering_a_kind_is_enough_to_make_it_creatable_addable_and_patchable() {
    let schemas = crate::schemas(&registry());
    let listed = schemas["methods"].as_object().expect("a method listing");
    let mut kinds = 0usize;
    for kind in declared_geometry_kinds() {
        kinds += 1;
        let declared: Vec<String> = component_parameters(kind, true)
            .expect("the table's own kind")
            .into_iter()
            .map(|parameter| parameter.name)
            .collect();
        for (op, method, extra) in [
            (GeometryOp::Create, format!("mask.create-{kind}"), vec![]),
            (
                GeometryOp::Add,
                format!("mask.add-{kind}"),
                vec!["mask", "mode"],
            ),
            (
                GeometryOp::Set,
                format!("mask.set-{kind}"),
                vec!["mask", "component"],
            ),
        ] {
            let command = find(&method).unwrap_or_else(|| panic!("{kind} declares no {method}"));
            assert_eq!(command.op, MaskOp::Geometry(GeometryMethod { op, kind }));
            assert_eq!(command.method, command.action.id, "{method}");
            assert!(
                listed.get(&method).is_some(),
                "schema.list does not list {method}"
            );
            let names: Vec<&str> = command
                .action
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect();
            // The objects the method addresses first, as identity parameters, then the values
            // it sets.
            let mut expected: Vec<&str> = extra;
            expected.extend(declared.iter().map(String::as_str));
            assert_eq!(names, expected, "{method} declares the wrong parameters");
            assert_eq!(
                command.action.patch,
                op == GeometryOp::Set,
                "only a patch method patches"
            );
            // A generated control has to be usable, not merely present: every geometry parameter
            // is a number over a finite range, with the display hints a number field needs and a
            // soft range inside its hard one.
            for parameter in &command.action.parameters {
                if parameter.name == "mode" || parameter.kind.is_identity() {
                    continue;
                }
                let where_ = format!("{method} {}", parameter.name);
                let ParameterKind::Number { min, max } = parameter.kind else {
                    panic!("{where_} is not a number");
                };
                assert!(min.is_finite() && max.is_finite() && min < max, "{where_}");
                assert!(parameter.unit.is_some(), "{where_} declares no unit");
                let step = parameter.step.expect("a step");
                let fine = parameter.fine_step.expect("a fine step");
                assert!(step.is_finite() && step > 0.0, "{where_}");
                assert!(fine.is_finite() && fine > 0.0 && fine <= step, "{where_}");
                let precision = parameter.precision.expect("a precision");
                assert!(precision <= 6, "{where_}");
                // A precision is a display hint and must never hide the step a key press moves
                // by: the step is a whole number of the shown decimals.
                let shown = step * 10f64.powi(i32::from(precision));
                assert!(
                    (shown - shown.round()).abs() < 1e-9,
                    "{where_} shows {precision} decimals, which hides its step {step}"
                );
                let soft_min = parameter.soft_min.unwrap_or(min);
                let soft_max = parameter.soft_max.unwrap_or(max);
                assert!(
                    soft_min >= min && soft_max <= max && soft_min < soft_max,
                    "{where_} declares a soft range outside {min}..={max}"
                );
                if let Some(zero) = parameter.zero {
                    assert!((min..=max).contains(&zero), "{where_}");
                }
            }
        }
    }
    assert_eq!(
        kinds, 4,
        "linear, radial and the two range selections are the kinds whose geometry is declared \
         as numbers"
    );
    // And the other side of the same contract: a kind whose geometry is drawn is evaluable
    // without being generated over, so it registers, parses and renders while declaring no
    // parameter, no geometry method and no control.
    let drawn: Vec<&str> = crate::mask::component_kinds()
        .filter(|kind| component_geometry_is_drawn(kind))
        .collect();
    assert_eq!(drawn, vec!["brush"]);
    assert_eq!(
        drawn,
        vec![stroke_kind()],
        "a stroke starts a component of the one drawn kind"
    );
    for kind in drawn {
        assert!(knows_component_kind(kind));
        assert!(component_parameters(kind, true).is_none());
        for method in [
            format!("mask.create-{kind}"),
            format!("mask.add-{kind}"),
            format!("mask.set-{kind}"),
        ] {
            assert!(find(&method).is_none(), "{method} should not be generated");
            assert!(listed.get(&method).is_none(), "schema.list lists {method}");
        }
        assert!(
            !controls().iter().any(|control| matches!(
                control,
                Control::Number(NumberControl { action, .. }) if action.contains(kind)
            )),
            "a drawn kind declares no control"
        );
    }
}

#[test]
fn every_control_binds_to_a_parameter_its_own_command_declares() {
    for control in controls() {
        let (action, parameters) = match control {
            Control::Number(NumberControl {
                action, parameter, ..
            })
            | Control::Toggle(ToggleControl {
                action, parameter, ..
            })
            | Control::Choice(ChoiceControl {
                action, parameter, ..
            }) => (action, vec![parameter]),
            Control::Range(RangeControl {
                action,
                low,
                high,
                low_feather,
                high_feather,
                ..
            }) => (
                action,
                [
                    Some(low),
                    Some(high),
                    low_feather.as_ref(),
                    high_feather.as_ref(),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
            other => panic!("unexpected mask control {other:?}"),
        };
        let command = find(action).unwrap_or_else(|| panic!("no command {action}"));
        for parameter in parameters {
            assert!(
                command.action.parameter(parameter).is_some(),
                "{action} declares no parameter {parameter}"
            );
        }
    }
}

/// The luminance band is one axis, so its patch method gets the one `range` control over its
/// four numbers, first and on a black-to-white rail, and keeps the four number fields beneath
/// it. No other kind is a band: a colour range is swatches and a radius, and the geometric kinds
/// are positions.
#[test]
fn the_luminance_band_declares_one_range_control_above_its_number_fields() {
    let bands: Vec<(usize, &Control)> = controls()
        .iter()
        .enumerate()
        .filter(|(_, control)| matches!(control, Control::Range(_)))
        .collect();
    assert_eq!(bands.len(), 1, "{bands:?}");
    let (at, band) = bands[0];
    assert_eq!(
        serde_json::to_value(band).unwrap(),
        json!({"kind":"range","action":"mask.set-luminance-range","low":"low","high":"high",
               "low_feather":"low_feather","high_feather":"high_feather","label":"Range",
               "rail":{"gradient":{"stops":[[0,0,0],[255,255,255]]}}})
    );
    let fields: Vec<&str> = controls()[at + 1..]
        .iter()
        .take(4)
        .map(|control| match control {
            Control::Number(NumberControl {
                action,
                parameter,
                style: NumberStyle::Field,
                ..
            }) if action == "mask.set-luminance-range" => parameter.as_str(),
            other => panic!("a band's number field follows it, not {other:?}"),
        })
        .collect();
    assert_eq!(fields, ["low", "low_feather", "high", "high_feather"]);
    for kind in declared_geometry_kinds().filter(|kind| *kind != "luminance-range") {
        assert!(
            component_band_control(kind, "mask.set-x").is_none(),
            "{kind}"
        );
    }
}
/// A command's `schema.list` entry, as a client reads it.
fn schema(method: &str) -> Value {
    crate::schemas(&registry())["methods"][method].clone()
}

#[test]
fn the_schema_of_each_command_states_its_identities_and_its_parameters() {
    let create = schema("mask.create-linear");
    assert_eq!(create["mutates"], json!(true));
    assert_eq!(
        create["required"],
        json!(["asset_id", "mutation", "x0", "y0", "x1", "y1"]),
        "the kind is in the method name, so it is not a parameter"
    );
    // And a radial's create declares its own fields and none of the linear's, which is the whole
    // point of generating a method per kind.
    assert_eq!(
        schema("mask.create-radial")["required"],
        json!([
            "asset_id", "mutation", "x", "y", "radius_x", "radius_y", "angle", "feather"
        ])
    );
    assert_eq!(
        schema("mask.add-radial")["required"],
        json!([
            "asset_id", "mutation", "mask", "mode", "x", "y", "radius_x", "radius_y", "angle",
            "feather"
        ])
    );
    let patch = schema("mask.set-linear");
    assert_eq!(patch["patch"], json!(true));
    assert_eq!(
        patch["required"],
        json!(["asset_id", "mutation", "mask", "component"])
    );
    assert_eq!(
        patch["optional"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["x0", "x1", "y0", "y1"],
        "every field of a patch is optional, whatever it declares"
    );
    let list = schema(LIST);
    assert_eq!(list["mutates"], json!(false));
    assert_eq!(list["required"], json!(["asset_id"]));
    assert_eq!(
        list["optional"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["entry_id"]
    );
    let rename = schema("mask.rename");
    assert_eq!(
        rename["required"],
        json!(["asset_id", "mutation", "mask", "name"])
    );
    let rename_component = schema("mask.rename-component");
    assert_eq!(
        rename_component["required"],
        json!(["asset_id", "mutation", "mask", "component", "name"])
    );
}

#[test]
fn the_generic_parameter_check_is_the_only_path_a_value_takes() {
    let create = &find("mask.create-linear").unwrap().action;
    // Out of range, wrong type, unknown and missing, all refused by the delivered check.
    assert_eq!(
        crate::check_parameters(create, &json!({"x0":3.0,"y0":0,"x1":0,"y1":1}))
            .unwrap_err()
            .detail,
        "parameter x0 must be a number within -1..=2"
    );
    assert_eq!(
        crate::check_parameters(create, &json!({"mode":"add","x0":0,"y0":0,"x1":0,"y1":1}))
            .unwrap_err()
            .detail,
        "unknown parameter mode for action mask.create-linear",
        "the first component of a mask is always add, so no mode can be requested"
    );
    assert_eq!(
        crate::check_parameters(create, &json!({"x0":0,"y0":0,"x1":0}))
            .unwrap_err()
            .detail,
        "missing required parameter y1 for action mask.create-linear"
    );
    // One kind's field is simply not a parameter of another kind's method, so the closed
    // vocabulary refuses it by name instead of a radius silently passing a position's range.
    assert_eq!(
        crate::check_parameters(create, &json!({"x0":0,"y0":0,"x1":0,"y1":1,"radius_x":0.5}))
            .unwrap_err()
            .detail,
        "unknown parameter radius_x for action mask.create-linear"
    );
    let radial = &find("mask.create-radial").unwrap().action;
    assert_eq!(
        crate::check_parameters(
            radial,
            &json!({"x":0.5,"y":0.5,"radius_x":0.0,"radius_y":0.3,"angle":0,"feather":50})
        )
        .unwrap_err()
        .detail,
        "parameter radius_x must be a number within 0.0001..=64",
        "a radius takes the study's distance range, which a position's range could not express"
    );
    assert_eq!(
        crate::check_parameters(
            radial,
            &json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.3,"angle":270,"feather":50})
        )
        .unwrap_err()
        .detail,
        "parameter angle must be a number within -180..=180"
    );
    // A patch fills no defaults and demands nothing but the identities it addresses.
    let patch = &find("mask.set-linear").unwrap().action;
    let (mask, component) = (MaskId::new(), ComponentId::new());
    let addressed = json!({"mask": mask.as_str(), "component": component.as_str(), "y1": 0.5});
    assert_eq!(
        crate::check_parameters(patch, &addressed).unwrap(),
        addressed.as_object().unwrap().clone()
    );
    assert_eq!(
        crate::check_parameters(patch, &json!({"mask": mask.as_str(), "y1": 0.5}))
            .unwrap_err()
            .detail,
        "missing required parameter component for action mask.set-linear"
    );
}

#[test]
fn a_generated_number_field_binds_only_a_number_parameter() {
    // A kind whose geometry holds a vertex list beside its numbers — the polygon proposed next —
    // gets a field for each number and nothing for the list, rather than a number field that
    // could never hold its value.
    let generated: Vec<Control> = geometry_controls(
        "mask.set-polygon",
        vec![
            ParameterDescriptor::points("points", 3, 64),
            ParameterDescriptor::number("feather", 0.0, 1.0),
            ParameterDescriptor::integer("sides", 3, 64),
            ParameterDescriptor::boolean("closed"),
        ],
    )
    .collect();
    assert_eq!(
        generated,
        vec![Control::from(
            Control::number("mask.set-polygon", "feather", "Feather")
                .number_style(NumberStyle::Field)
        )]
    );
    // And every kind registered today is all numbers, so each of its declared parameters has
    // exactly one field, in declaration order, and nothing else is generated for it.
    for kind in declared_geometry_kinds() {
        let action = geometry(GeometryOp::Set, kind)
            .expect("every kind generates its patch method")
            .method;
        let fields: Vec<&str> = controls()
            .iter()
            .filter_map(|control| match control {
                Control::Number(NumberControl {
                    action: bound,
                    parameter,
                    ..
                }) if bound == action => Some(parameter.as_str()),
                _ => None,
            })
            .collect();
        let declared: Vec<String> = component_parameters(kind, false)
            .expect("the table's own kind")
            .into_iter()
            .map(|parameter| parameter.name)
            .collect();
        assert_eq!(fields, declared, "{kind}");
    }
}

#[test]
fn the_host_descriptor_meets_the_rules_a_module_descriptor_does() {
    let host = descriptor();
    host.validate_host()
        .expect("the host's mask descriptor is a valid host descriptor");
    assert!(
        registry()
            .host_descriptors()
            .iter()
            .all(|published| std::ptr::eq(*published, host))
    );
    // The three allowances are the host's alone: its method-name identities, its payload field
    // names and its identity parameters are each refused on a module.
    let error = host
        .validate()
        .expect_err("a module may not declare mask.*");
    assert_eq!(error.detail, "invalid action identity mask.delete");
    let module_like = |action: ActionDescriptor| ModuleDescriptor {
        id: "luxforge.masks".to_owned(),
        title: "Masks".to_owned(),
        actions: vec![action],
        ..ModuleDescriptor::default()
    };
    let radial = geometry(GeometryOp::Create, "radial")
        .expect("a generated radial")
        .action
        .clone();
    let payload_names = module_like(ActionDescriptor {
        id: "create-radial".to_owned(),
        ..radial
    });
    assert_eq!(
        payload_names.validate().expect_err("radius_x").detail,
        "invalid parameter name radius_x of action create-radial"
    );
    let delete = find("mask.delete").expect("mask.delete").action.clone();
    let identities = module_like(ActionDescriptor {
        id: "delete".to_owned(),
        ..delete
    });
    assert!(
        identities
            .validate()
            .expect_err("an identity parameter")
            .detail
            .contains("which only a host command declares")
    );

    // Everything else a module is held to still holds, and a host identity is a method name and
    // nothing looser: a malformed one is refused as a module's is.
    for (id, refused) in [
        (
            "mask.Create-linear",
            "invalid action identity mask.Create-linear",
        ),
        ("create-linear", "invalid action identity create-linear"),
        ("mask.", "invalid action identity mask."),
        (
            "mask.create.linear",
            "invalid action identity mask.create.linear",
        ),
        (
            "Mask.create-linear",
            "invalid action identity Mask.create-linear",
        ),
    ] {
        let mut broken = host.clone();
        broken.actions[0].id = id.to_owned();
        assert_eq!(broken.validate_host().expect_err(id).detail, refused);
    }
    let mut repeated = host.clone();
    repeated.actions[1].id = repeated.actions[0].id.clone();
    assert_eq!(
        repeated.validate_host().expect_err("a duplicate").detail,
        "duplicate action mask.delete"
    );
    let mut unbound = host.clone();
    unbound
        .controls
        .push(Control::number("mask.set-radial", "radius", "Radius").into());
    assert_eq!(
        unbound
            .validate_host()
            .expect_err("an undeclared parameter")
            .detail,
        "action mask.set-radial has no parameter radius"
    );

    // A variant applies only on the global target, and a host control always addresses a mask,
    // so a host control that declares one is refused, at any depth.
    let variant = |control: Control| {
        let Control::Number(number) = control else {
            panic!("the host's first control is a number field");
        };
        let replacement = number.clone();
        Control::from(number.variant(ControlVariant::control(
            crate::SourceTag::Raw,
            "luxforge.raw",
            replacement,
        )))
    };
    let mut top = host.clone();
    top.controls[0] = variant(top.controls[0].clone());
    let mut nested = host.clone();
    let first = nested.controls.remove(0);
    nested
        .controls
        .insert(0, Control::group("Mask", vec![variant(first)]).into());
    for broken in [top, nested] {
        assert_eq!(
            broken.validate_host().expect_err("a variant").detail,
            "number control of host descriptor luxforge.masks declares variants, which apply \
             only on the global target a host control never addresses"
        );
    }
}

#[test]
fn a_mask_command_and_a_module_action_can_never_collide() {
    // A mask command's identity carries a dot, which an action identity may not, so the two families
    // are disjoint by construction — and a module that declares one anyway is refused by name rather
    // than shadowing the host.
    let mut registry = crate::ModuleRegistry::builtin();
    let error = registry
        .register(std::sync::Arc::new(Colliding))
        .expect_err("a module may not declare a host mask command");
    assert_eq!(error.kind, crate::ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "test.collide declares mask.create-linear, which is a host mask command"
    );
}

/// A module whose one action names a host mask command — a *generated* one, because a generated
/// geometry method carries the same dotted identity every other command does and is protected by the
/// same check.
struct Colliding;

impl crate::ToolModule for Colliding {
    fn descriptor(&self) -> &crate::ModuleDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::ModuleDescriptor> =
            std::sync::LazyLock::new(|| crate::ModuleDescriptor {
                id: "test.collide".into(),
                title: "Collide".into(),
                hint: None,
                effects: Vec::new(),
                actions: vec![crate::ActionDescriptor {
                    id: "mask.create-linear".into(),
                    title: "Create".into(),
                    notes: String::new(),
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: true,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: crate::Availability::Available,
                ..crate::ModuleDescriptor::default()
            });
        &DESCRIPTOR
    }
    fn parse(
        &self,
        _: &str,
        _: &serde_json::Map<String, Value>,
    ) -> Result<crate::ActionInput, crate::Error> {
        unreachable!("registration is refused before anything is parsed")
    }
    fn plan(
        &self,
        _: &crate::ActionInput,
        _: &crate::StageContext<'_>,
    ) -> Result<crate::ActionPlan, crate::Error> {
        unreachable!("registration is refused before anything is planned")
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), crate::Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, crate::Error> {
        Ok(crate::LayerReport::default())
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<crate::Processing, crate::Error> {
        unreachable!("registration is refused before anything is compiled")
    }
}
