//! Tests of the registration rules a descriptor is checked against.
use super::testing::*;
use super::*;
use crate::ErrorKind;
use serde::Deserialize;
use serde_json::{Map, json};

fn query_choice_descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: "test.querychoice".into(),
        title: "Query choice".into(),
        queries: vec![ActionDescriptor {
            parameters: vec![
                ParameterDescriptor::string("text", 64),
                ParameterDescriptor::integer("page", 0, 99),
                ParameterDescriptor::number("focal", 0.5, 2000.0),
            ],
            ..ActionDescriptor::new("profiles", "Profiles", "test")
        }],
        actions: vec![ActionDescriptor {
            parameters: vec![
                ParameterDescriptor::string("key", 32),
                ParameterDescriptor::number("focal", 0.5, 2000.0),
            ],
            ..ActionDescriptor::new("select-profile", "Select profile", "test")
        }],
        controls: vec![Control::QueryChoice(QueryChoiceControl {
            label: "Profile".into(),
            query: "profiles".into(),
            text: "text".into(),
            page: "page".into(),
            action: "select-profile".into(),
            key: "key".into(),
            shared: vec!["focal".into()],
        })],
        ..ModuleDescriptor::default()
    }
}

#[test]
fn query_choice_registration_refuses_unbound_or_mismatched_parameters() {
    query_choice_descriptor().validate().unwrap();
    let mutate: [fn(&mut ModuleDescriptor); 6] = [
        |d| d.queries.clear(),
        |d| d.actions[0].patch = true,
        |d| d.actions[0].parameters[0] = ParameterDescriptor::integer("key", 0, 99),
        |d| d.queries[0].parameters[0] = ParameterDescriptor::integer("text", 0, 99),
        |d| d.actions[0].parameters[1] = ParameterDescriptor::integer("focal", 1, 2000),
        |d| d.controls.push(d.controls[0].clone()),
    ];
    for mutate in mutate {
        let mut descriptor = query_choice_descriptor();
        mutate(&mut descriptor);
        assert_eq!(
            descriptor.validate().unwrap_err().kind,
            ErrorKind::Validation
        );
    }
}

#[test]
fn query_choice_rows_contract_is_validated_by_the_host() {
    let descriptor = query_choice_descriptor();
    let row = json!({"key": "lf1-0", "title": "Lens", "eligible": true, "subtitle": "test", "reasons": []});
    descriptor
        .validate_query_choice_answer("profiles", &json!({"rows": [row.clone()]}))
        .unwrap();
    for key in ["key", "title", "eligible"] {
        let mut malformed = row.clone();
        malformed.as_object_mut().unwrap().remove(key);
        let error = descriptor
            .validate_query_choice_answer("profiles", &json!({"rows": [malformed]}))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.detail.contains("test.querychoice"));
    }
    for malformed in [
        json!({}),
        json!({"rows": vec![row; 101]}),
        json!({"rows": [{"key":"a", "title":"A", "eligible":true, "reasons":"bad"}]}),
    ] {
        assert!(
            descriptor
                .validate_query_choice_answer("profiles", &malformed)
                .is_err()
        );
    }
    descriptor
        .validate_query_choice_answer("different-query", &json!({}))
        .unwrap();
}

#[test]
fn a_points_parameter_is_declared_within_the_host_path_bound() {
    let limit = crate::path::POSTED_POINTS_PER_STROKE;
    points_module(1, limit)
        .validate()
        .expect("the whole bound is declarable");
    points_module(1, 1)
        .validate()
        .expect("a one-position path is a legal declaration");
    for (min, max) in [(0, 8), (1, limit + 1), (8, 4)] {
        assert_eq!(
            points_module(min, max).validate().unwrap_err().detail,
            format!("parameter path declares invalid path point bounds; 1..={limit} is the limit")
        );
    }
    // A path is not numeric and not a curve, so it carries none of the numeric display hints.
    let hinted = ModuleDescriptor {
        actions: vec![ActionDescriptor {
            parameters: vec![ParameterDescriptor {
                step: Some(0.1),
                ..points_module(1, 8).actions[0].parameters[0].clone()
            }],
            ..points_module(1, 8).actions[0].clone()
        }],
        ..points_module(1, 8)
    };
    assert_eq!(
        hinted.validate().unwrap_err().detail,
        "parameter path declares a step or precision but is not numeric or a curve"
    );
}

/// A number control may declare what resetting its field runs. It is validated like a group's
/// reset, lists in `module.list` beside the control's other fields, and a control without one
/// lists no `reset` at all, so every other control keeps its shape.
#[test]
fn a_number_control_declares_its_own_field_reset() {
    let declared = number_reset("set-thing", json!({"mode": "fast"}));
    declared
        .validate()
        .expect("a reset naming this module's action");
    let listed = serde_json::to_value(&declared).unwrap();
    assert_eq!(
        listed["controls"][0],
        json!({"kind":"number","action":"set-thing","parameter":"x","label":"X",
            "reset":{"action":"set-thing","preset":{"mode":"fast"}}})
    );
    assert_eq!(ModuleDescriptor::deserialize(&listed).unwrap(), declared);
    let plain = serde_json::to_value(descriptor()).unwrap();
    let number = &plain["controls"][0]["controls"][0];
    assert_eq!(number["kind"], "number");
    assert!(number.get("reset").is_none(), "{number}");
    // A preset naming another module's action is refused like any undeclared action.
    let error = number_reset("reset-raw", json!({}))
        .validate()
        .expect_err("another module's action");
    assert_eq!(
        error.detail,
        "module test.module references undeclared action reset-raw"
    );
}

#[test]
fn identity_rules_accept_declared_names_and_reject_malformed_ones() {
    for value in ["luxforge.pixel", "a", "luxforge.pixel.replace", "a1.b2"] {
        assert!(valid_identity(value), "{value}");
    }
    for value in [
        "",
        "Luxforge.pixel",
        "luxforge..pixel",
        ".pixel",
        "pixel.",
        "lux_forge",
        "1pixel",
        "set-pixel",
    ] {
        assert!(!valid_identity(value), "{value}");
    }
    for value in ["set-pixel", "transform", "rotate-left", "x", "crop-16-9"] {
        assert!(valid_name(value), "{value}");
    }
    for value in [
        "",
        "Set-Pixel",
        "set--pixel",
        "-set",
        "set-",
        "set.pixel",
        "1set",
    ] {
        assert!(!valid_name(value), "{value}");
    }
}

#[test]
fn a_crop_frame_binds_an_owned_geometry_effect() {
    for foreign in [false, true] {
        let mut module = frame_descriptor();
        if foreign {
            let Some(CanvasInteraction::CropFrame { effect, .. }) = &mut module.canvas else {
                panic!("a crop-frame fixture")
            };
            *effect = "test.foreign.effect".into();
        } else {
            module.effects[0].stage = EffectStage::Pixel;
        }
        let error = module
            .validate()
            .expect_err("the frame must own a geometry effect");
        assert!(error.detail.contains("not an owned geometry effect"));
    }
}

#[test]
fn descriptors_reject_malformed_identities_duplicates_and_invalid_controls() {
    assert!(descriptor().validate().is_ok());
    assert!(
        frame_descriptor().validate().is_ok(),
        "a crop frame over declared number parameters is accepted"
    );
    assert!(
        sample_descriptor().validate().is_ok(),
        "a sample-apply over a declared query and action is accepted"
    );
    assert_eq!(
        sample_descriptor().query("neutral-sample").map(|q| &q.id),
        Some(&"neutral-sample".to_owned())
    );
    // A picker is a declared control bound to the module's own pick canvas, with the serialized
    // shape a client discovers it by, and it nests in a group like every other control.
    assert_eq!(
        serde_json::to_value(Control::Picker(PickerControl {
            label: "Neutral picker".into(),
            variants: Vec::new(),
        }))
        .unwrap(),
        json!({"kind": "picker", "label": "Neutral picker"})
    );
    let nested = ModuleDescriptor {
        controls: vec![Control::Group(GroupControl {
            label: "White balance".into(),
            reset: None,
            controls: vec![Control::Picker(PickerControl {
                label: "Pick".into(),
                variants: Vec::new(),
            })],
            collapsed: false,
            variants: Vec::new(),
        })],
        ..sample_descriptor()
    };
    assert!(
        nested.validate().is_ok(),
        "a picker inside a group satisfies the module's one-picker rule"
    );
    assert_eq!(
        ModuleDescriptor::deserialize(&serde_json::to_value(&nested).unwrap()).unwrap(),
        nested,
        "a picker round-trips through JSON"
    );
    assert!(
        descriptor().queries.is_empty(),
        "queries are optional and default to none"
    );
    let cases: Vec<(&str, ModuleDescriptor)> = vec![
        (
            "module identity",
            ModuleDescriptor {
                id: "Test Module".into(),
                ..descriptor()
            },
        ),
        (
            "module title",
            ModuleDescriptor {
                title: "  ".into(),
                ..descriptor()
            },
        ),
        (
            "effect identity",
            ModuleDescriptor {
                effects: vec![EffectDescriptor::new("Test-Effect", EffectStage::Pixel)],
                ..descriptor()
            },
        ),
        (
            "duplicate effect",
            ModuleDescriptor {
                effects: vec![
                    descriptor().effects[0].clone(),
                    descriptor().effects[0].clone(),
                ],
                ..descriptor()
            },
        ),
        (
            "duplicate action",
            ModuleDescriptor {
                actions: vec![action(), action()],
                ..descriptor()
            },
        ),
        (
            "action identity",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    id: "Set.Thing".into(),
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "duplicate parameter",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![integer("x"), integer("x")],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "empty integer range",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![ParameterDescriptor {
                        kind: ParameterKind::Integer { min: 5, max: 1 },
                        ..integer("x")
                    }],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "empty enum",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![
                        ParameterDescriptor::enumeration("mode", Vec::<String>::new())
                            .required(true)
                            .notes("test"),
                    ],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "default out of range",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![ParameterDescriptor {
                        default: Some(json!(99)),
                        ..integer("x")
                    }],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "undeclared action",
            ModuleDescriptor {
                controls: vec![Control::Number(NumberControl {
                    action: "missing".into(),
                    parameter: "x".into(),
                    label: "X".into(),
                    style: crate::NumberStyle::Slider,
                    rail: None,
                    reset: None,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "undeclared parameter",
            ModuleDescriptor {
                controls: vec![Control::Number(NumberControl {
                    action: "set-thing".into(),
                    parameter: "missing".into(),
                    label: "X".into(),
                    style: crate::NumberStyle::Slider,
                    rail: None,
                    reset: None,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "wrong control kind",
            ModuleDescriptor {
                controls: vec![Control::Color(ColorControl {
                    action: "set-thing".into(),
                    parameter: "x".into(),
                    label: "X".into(),
                    style: crate::ColorStyle::Fields,
                })],
                ..descriptor()
            },
        ),
        (
            "preset out of range",
            ModuleDescriptor {
                controls: vec![Control::Action(ActionControl {
                    action: "set-thing".into(),
                    label: "Apply".into(),
                    preset: json!({"x": 99}).as_object().unwrap().clone(),
                    style: crate::ActionStyle::Default,
                    icon: None,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "preset names an undeclared parameter",
            ModuleDescriptor {
                controls: vec![Control::Action(ActionControl {
                    action: "set-thing".into(),
                    label: "Apply".into(),
                    preset: json!({"missing": 1}).as_object().unwrap().clone(),
                    style: crate::ActionStyle::Default,
                    icon: None,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "canvas parameter is not an integer",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::PointPick {
                    action: "set-thing".into(),
                    x: "x".into(),
                    y: "rgb".into(),
                    title: "Pick".into(),
                    shortcut: None,
                    icon: None,
                    commit: false,
                }),
                ..descriptor()
            },
        ),
        (
            "empty number range",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![number("angle", 5.0, 1.0)],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "non-finite number range",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![number("angle", 0.0, f64::INFINITY)],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "number default out of range",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    parameters: vec![ParameterDescriptor {
                        default: Some(json!(2.5)),
                        ..number("angle", -1.0, 1.0)
                    }],
                    ..action()
                }],
                controls: Vec::new(),
                ..descriptor()
            },
        ),
        (
            "number control on a color parameter",
            ModuleDescriptor {
                controls: vec![Control::Number(NumberControl {
                    action: "set-thing".into(),
                    parameter: "rgb".into(),
                    label: "RGB".into(),
                    style: crate::NumberStyle::Slider,
                    rail: None,
                    reset: None,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "crop frame names an undeclared action",
            ModuleDescriptor {
                canvas: Some(frame_canvas("missing", "fit-frame")),
                controls: Vec::new(),
                ..frame_descriptor()
            },
        ),
        (
            "crop frame names an undeclared fit action",
            ModuleDescriptor {
                canvas: Some(frame_canvas("set-frame", "missing")),
                controls: Vec::new(),
                ..frame_descriptor()
            },
        ),
        (
            "crop frame parameter is missing",
            frame_with(
                "set-frame",
                vec![
                    number("angle", -45.0, 45.0),
                    number("x", 0.0, 1.0),
                    number("y", 0.0, 1.0),
                    number("width", 0.0, 1.0),
                ],
            ),
        ),
        (
            "crop frame parameter is not a number",
            frame_with(
                "set-frame",
                vec![
                    integer("angle"),
                    number("x", 0.0, 1.0),
                    number("y", 0.0, 1.0),
                    number("width", 0.0, 1.0),
                    number("height", 0.0, 1.0),
                ],
            ),
        ),
        (
            "crop frame aspect is not an enum",
            frame_with(
                "fit-frame",
                vec![number("aspect", 0.0, 1.0), number("angle", -45.0, 45.0)],
            ),
        ),
        (
            "crop frame fit action has no angle",
            frame_with("fit-frame", vec![enumerated("aspect")]),
        ),
        (
            "crop frame fit angle is not a number",
            frame_with("fit-frame", vec![enumerated("aspect"), integer("angle")]),
        ),
        (
            "module reset names an undeclared action",
            ModuleDescriptor {
                reset: Some(ResetAction {
                    action: "missing".into(),
                    preset: Map::new(),
                }),
                ..descriptor()
            },
        ),
        (
            "module reset preset out of range",
            ModuleDescriptor {
                reset: Some(ResetAction {
                    action: "set-thing".into(),
                    preset: json!({"x": 99}).as_object().unwrap().clone(),
                }),
                ..descriptor()
            },
        ),
        (
            "group reset names an undeclared parameter",
            ModuleDescriptor {
                controls: vec![Control::Group(GroupControl {
                    label: "Test".into(),
                    controls: Vec::new(),
                    reset: Some(ResetAction {
                        action: "set-thing".into(),
                        preset: json!({"missing": 1}).as_object().unwrap().clone(),
                    }),
                    collapsed: false,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "group reset preset out of range",
            ModuleDescriptor {
                controls: vec![Control::Group(GroupControl {
                    label: "Test".into(),
                    controls: Vec::new(),
                    reset: Some(ResetAction {
                        action: "set-thing".into(),
                        preset: json!({"mode": "sloppy"}).as_object().unwrap().clone(),
                    }),
                    collapsed: false,
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "number reset names an undeclared action",
            number_reset("missing", json!({})),
        ),
        (
            "number reset names an undeclared parameter",
            number_reset("set-thing", json!({"missing": 1})),
        ),
        (
            "number reset preset out of range",
            number_reset("set-thing", json!({"x": 99})),
        ),
        (
            "number reset preset of the wrong kind",
            number_reset("set-thing", json!({"mode": 3})),
        ),
        (
            "canvas mode without a title",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::PointPick {
                    action: "set-thing".into(),
                    x: "x".into(),
                    y: "x".into(),
                    title: "  ".into(),
                    shortcut: None,
                    icon: None,
                    commit: false,
                }),
                ..descriptor()
            },
        ),
        (
            "canvas shortcut is not one uppercase letter",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::PointPick {
                    action: "set-thing".into(),
                    x: "x".into(),
                    y: "x".into(),
                    title: "Pick".into(),
                    shortcut: Some("r".into()),
                    icon: None,
                    commit: false,
                }),
                ..descriptor()
            },
        ),
        (
            "canvas shortcut is a word",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::PointPick {
                    action: "set-thing".into(),
                    x: "x".into(),
                    y: "x".into(),
                    title: "Pick".into(),
                    shortcut: Some("RR".into()),
                    icon: None,
                    commit: false,
                }),
                ..descriptor()
            },
        ),
        (
            // `set-thing` also declares `rgb` and `mode`, which a pick cannot supply.
            "a committing pick whose action takes more than the coordinates",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::PointPick {
                    action: "set-thing".into(),
                    x: "x".into(),
                    y: "x".into(),
                    title: "Pick".into(),
                    shortcut: None,
                    icon: None,
                    commit: true,
                }),
                ..descriptor()
            },
        ),
        (
            "a query with an invalid identity",
            ModuleDescriptor {
                queries: vec![ActionDescriptor {
                    id: "Neutral.Sample".into(),
                    ..query()
                }],
                canvas: None,
                ..descriptor()
            },
        ),
        (
            "a duplicate query",
            ModuleDescriptor {
                queries: vec![query(), query()],
                canvas: None,
                ..descriptor()
            },
        ),
        (
            "a query parameter with an empty range",
            ModuleDescriptor {
                queries: vec![ActionDescriptor {
                    parameters: vec![ParameterDescriptor {
                        kind: ParameterKind::Integer { min: 9, max: 1 },
                        ..integer("x")
                    }],
                    ..query()
                }],
                canvas: None,
                ..descriptor()
            },
        ),
        (
            "a sample-apply naming an undeclared query",
            ModuleDescriptor {
                canvas: Some(sample_apply("missing", "x", "y", "set-thing")),
                ..sample_descriptor()
            },
        ),
        (
            "a sample-apply naming an undeclared coordinate",
            ModuleDescriptor {
                canvas: Some(sample_apply("neutral-sample", "x", "z", "set-thing")),
                ..sample_descriptor()
            },
        ),
        (
            "a sample-apply whose coordinate is not an integer",
            ModuleDescriptor {
                queries: vec![ActionDescriptor {
                    parameters: vec![integer("x"), number("y", 0.0, 10.0)],
                    ..query()
                }],
                ..sample_descriptor()
            },
        ),
        (
            "a sample-apply naming an undeclared action",
            ModuleDescriptor {
                canvas: Some(sample_apply("neutral-sample", "x", "y", "missing")),
                ..sample_descriptor()
            },
        ),
        (
            "a sample-apply without a title",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::SampleApply {
                    query: "neutral-sample".into(),
                    x: "x".into(),
                    y: "y".into(),
                    action: "set-thing".into(),
                    title: "  ".into(),
                    shortcut: Some("W".into()),
                    icon: None,
                }),
                ..sample_descriptor()
            },
        ),
        (
            "a sample-apply whose shortcut is not one uppercase letter",
            ModuleDescriptor {
                canvas: Some(CanvasInteraction::SampleApply {
                    query: "neutral-sample".into(),
                    x: "x".into(),
                    y: "y".into(),
                    action: "set-thing".into(),
                    title: "Pick".into(),
                    shortcut: Some("w".into()),
                    icon: None,
                }),
                ..sample_descriptor()
            },
        ),
        // A picker binds to the module's own pick canvas, so it needs one and there is
        // exactly one of it; and a pick canvas needs the control that reaches it.
        (
            "a picker on a module with no canvas at all",
            ModuleDescriptor {
                controls: vec![Control::Picker(PickerControl {
                    label: "Pick".into(),
                    variants: Vec::new(),
                })],
                ..descriptor()
            },
        ),
        (
            "a picker on a crop-frame canvas, which is not a pick",
            ModuleDescriptor {
                controls: vec![Control::Picker(PickerControl {
                    label: "Pick".into(),
                    variants: Vec::new(),
                })],
                ..frame_descriptor()
            },
        ),
        (
            "two pickers in one module",
            ModuleDescriptor {
                controls: vec![
                    Control::Picker(PickerControl {
                        label: "Pick".into(),
                        variants: Vec::new(),
                    }),
                    Control::Group(GroupControl {
                        label: "Nested".into(),
                        reset: None,
                        controls: vec![Control::Picker(PickerControl {
                            label: "Pick again".into(),
                            variants: Vec::new(),
                        })],
                        collapsed: false,
                        variants: Vec::new(),
                    }),
                ],
                ..sample_descriptor()
            },
        ),
        (
            "an unlabelled picker",
            ModuleDescriptor {
                controls: vec![Control::Picker(PickerControl {
                    label: "  ".into(),
                    variants: Vec::new(),
                })],
                ..sample_descriptor()
            },
        ),
        (
            "a step that is zero",
            with_hints(Some(0.0), None, number("angle", -45.0, 45.0)),
        ),
        (
            "a negative step",
            with_hints(Some(-0.5), None, number("angle", -45.0, 45.0)),
        ),
        (
            "a step that is not finite",
            with_hints(Some(f64::NAN), None, number("angle", -45.0, 45.0)),
        ),
        (
            "an infinite step",
            with_hints(Some(f64::INFINITY), None, number("angle", -45.0, 45.0)),
        ),
        (
            "a precision above six",
            with_hints(None, Some(7), number("angle", -45.0, 45.0)),
        ),
        (
            "a step on a boolean parameter",
            with_hints(
                Some(1.0),
                None,
                ParameterDescriptor {
                    kind: ParameterKind::Boolean,
                    ..integer("x")
                },
            ),
        ),
        (
            "a precision on an enum parameter",
            with_hints(None, Some(2), enumerated("mode")),
        ),
        (
            "a string that declares no characters",
            with_hints(None, None, string("name", 0, true)),
        ),
        (
            "a string longer than 256 characters",
            with_hints(None, None, string("name", 257, true)),
        ),
        (
            "a string default longer than its bound",
            with_hints(
                None,
                None,
                ParameterDescriptor {
                    default: Some(json!("abcde")),
                    ..string("name", 4, false)
                },
            ),
        ),
        (
            "a string default with a control character",
            with_hints(
                None,
                None,
                ParameterDescriptor {
                    default: Some(json!("a\tb")),
                    ..string("name", 4, false)
                },
            ),
        ),
        (
            "a step on a string parameter",
            with_hints(Some(1.0), None, string("name", 4, true)),
        ),
        (
            "a settings default that is not a settings set",
            with_hints(
                None,
                None,
                ParameterDescriptor {
                    default: Some(json!({})),
                    ..settings("settings")
                },
            ),
        ),
    ];
    let cases = cases.into_iter().chain(
        presets_cases()
            .into_iter()
            .map(|(case, descriptor, _)| (case, descriptor)),
    );
    for (case, descriptor) in cases {
        let error = descriptor
            .validate()
            .expect_err(&format!("{case} must be rejected"));
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
    }
}

/// A presets control binds to an action shaped exactly for it, at most once per module, and
/// keeps the serialized form a client discovers it by.
#[test]
fn a_presets_control_needs_its_own_action_with_exactly_the_preset_parameters() {
    let valid = presets_descriptor();
    valid.validate().expect("the presets shape is accepted");
    let nested = ModuleDescriptor {
        controls: vec![Control::Group(GroupControl {
            label: "Library".into(),
            reset: None,
            controls: vec![Control::Presets(PresetsControl {
                action: "apply-thing".into(),
            })],
            collapsed: false,
            variants: Vec::new(),
        })],
        ..presets_descriptor()
    };
    nested
        .validate()
        .expect("one presets control inside a group is still one");
    assert_eq!(
        serde_json::to_value(&valid.controls[0]).unwrap(),
        json!({"kind": "presets", "action": "apply-thing"})
    );
    assert_eq!(
        ModuleDescriptor::deserialize(&serde_json::to_value(&valid).unwrap()).unwrap(),
        valid,
        "a presets descriptor round-trips through JSON"
    );
    for (case, descriptor, fragment) in presets_cases() {
        let error = descriptor
            .validate()
            .expect_err(&format!("{case} must be rejected"));
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert!(error.detail.contains(fragment), "{case}: {error}");
    }
}

#[test]
fn patch_action_buttons_name_at_least_one_field_and_declared_resets_may_name_a_group() {
    let mut descriptor = controls_descriptor();
    if let Control::Action(ActionControl { preset: fields, .. }) = &mut descriptor.controls[4] {
        fields.clear();
    }
    let error = descriptor.validate().expect_err("an empty preset");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "action control for patch action set-controls needs at least one preset field"
    );
    // A button may set a group's worth of fields at once, as As shot sets temperature and
    // tint.
    if let Control::Action(ActionControl { preset: fields, .. }) = &mut descriptor.controls[4] {
        *fields = json!({"amount": 0.0, "enabled": true})
            .as_object()
            .unwrap()
            .clone();
    }
    descriptor
        .validate()
        .expect("a button may name several fields of its patch");
    // A group reset is a separate declared gesture, and may intentionally restore several
    // parameters of a patch action at once without making a button's patch ambiguous.
    descriptor.controls[4] = Control::Action(ActionControl {
        action: "set-controls".into(),
        label: "Run".into(),
        preset: json!({"amount": 0.0}).as_object().unwrap().clone(),
        style: ActionStyle::Icon,
        icon: Some("rotate-left".into()),
        variants: Vec::new(),
    });
    descriptor.reset = Some(ResetAction {
        action: "set-controls".into(),
        preset: json!({"amount": 0.0, "enabled": false})
            .as_object()
            .unwrap()
            .clone(),
    });
    descriptor.validate().expect("a reset may restore a group");
}

#[test]
fn invalid_new_bindings_and_hints_name_the_control_or_parameter() {
    let base = controls_descriptor();
    for (case, edit, fragment) in [
        (
            "toggle",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Toggle(ToggleControl { parameter, .. }) = &mut d.controls[0] {
                    *parameter = "amount".into();
                }
            }) as Box<dyn Fn(&mut ModuleDescriptor)>,
            "toggle control for amount",
        ),
        (
            "choice",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Choice(ChoiceControl { parameter, .. }) = &mut d.controls[1] {
                    *parameter = "enabled".into();
                }
            }),
            "choice control for enabled",
        ),
        (
            "choice labels",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Choice(ChoiceControl { labels, .. }) = &mut d.controls[1] {
                    *labels = vec!["Only one".into()];
                }
            }),
            "choice control for mode of action set-controls labels 1 of its",
        ),
        (
            "rail",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Number(NumberControl { parameter, .. }) = &mut d.controls[2] {
                    *parameter = "enabled".into();
                }
            }),
            "number control for enabled",
        ),
        (
            "curve",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Curve(CurveControl { channels, .. }) = &mut d.controls[3] {
                    channels[0].parameter = "enabled".into();
                }
            }),
            "curve control for enabled",
        ),
        (
            "query",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Curve(CurveControl { sample_query, .. }) = &mut d.controls[3] {
                    *sample_query = "missing".into();
                }
            }),
            "undeclared query missing",
        ),
        (
            "icon",
            Box::new(|d: &mut ModuleDescriptor| {
                if let Control::Action(ActionControl { icon, .. }) = &mut d.controls[4] {
                    *icon = Some("Bad_Icon".into());
                }
            }),
            "invalid icon name Bad_Icon",
        ),
        (
            "soft",
            Box::new(|d: &mut ModuleDescriptor| d.actions[0].parameters[3].soft_min = Some(-11.0)),
            "parameter amount declares a soft range",
        ),
        (
            "fine",
            Box::new(|d: &mut ModuleDescriptor| d.actions[0].parameters[3].fine_step = Some(0.0)),
            "parameter amount declares a fine step",
        ),
        (
            "zero",
            Box::new(|d: &mut ModuleDescriptor| d.actions[0].parameters[3].zero = Some(11.0)),
            "parameter amount declares a zero",
        ),
    ] {
        let mut d = base.clone();
        edit(&mut d);
        let error = d.validate().expect_err(case);
        assert!(error.detail.contains(fragment), "{case}: {error}");
    }
}

#[test]
fn a_range_control_binds_number_parameters_of_one_action() {
    let full: Control = Control::range("set-band", "low", "high", "Range")
        .feathers("low-feather", "high-feather")
        .rail(RailDecoration::Gradient {
            stops: vec![[0, 0, 0], [255, 255, 255]],
        })
        .into();
    let descriptor = range_descriptor(full.clone());
    descriptor.validate().expect("four number parameters");
    // The shape a client reads, and nothing it did not declare.
    let serialized = serde_json::to_value(&full).unwrap();
    assert_eq!(
        serialized,
        json!({"kind":"range","action":"set-band","low":"low","high":"high",
               "low_feather":"low-feather","high_feather":"high-feather","label":"Range",
               "rail":{"gradient":{"stops":[[0,0,0],[255,255,255]]}}})
    );
    assert_eq!(
        ModuleDescriptor::deserialize(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
        descriptor
    );
    // The shoulders are optional: a band of two edges is a range too, and serializes without
    // them.
    let edges: Control = Control::range("set-band", "low", "high", "Range").into();
    range_descriptor(edges.clone())
        .validate()
        .expect("two edges and no shoulders");
    assert_eq!(
        serde_json::to_value(&edges).unwrap(),
        json!({"kind":"range","action":"set-band","low":"low","high":"high","label":"Range"})
    );
    assert_eq!(edges.kind_name(), "range");

    for (case, control, detail) in [
        (
            "wrong action",
            Control::range("set-thing", "low", "high", "Range"),
            "action set-thing has no parameter low",
        ),
        (
            "undeclared action",
            Control::range("set-missing", "low", "high", "Range"),
            "module test.module references undeclared action set-missing",
        ),
        (
            "missing parameter",
            Control::range("set-band", "low", "high", "Range").feathers("low-feather", "gone"),
            "action set-band has no parameter gone",
        ),
        (
            "non-number parameter",
            Control::range("set-band", "low", "count", "Range"),
            "range control for count of action set-band is not a number",
        ),
        (
            "one parameter twice",
            Control::range("set-band", "low", "high", "Range")
                .feathers("low-feather", "low-feather"),
            "range control of action set-band binds low-feather twice",
        ),
        (
            "two axes",
            Control::range("set-band", "low", "wide", "Range"),
            "range control of action set-band binds low and wide, which declare different \
             ranges",
        ),
        (
            "no label",
            Control::range("set-band", "low", "high", " "),
            "range control of action set-band has no label",
        ),
        (
            "one stop",
            Control::range("set-band", "low", "high", "Range").rail(RailDecoration::Gradient {
                stops: vec![[0, 0, 0]],
            }),
            "range control of action set-band needs 2..=8 gradient stops",
        ),
    ] {
        let error = range_descriptor(control).validate().expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert_eq!(error.detail, detail, "{case}");
    }

    // A rail is a hint a range may carry, as a number may, and it reads back from JSON.
    let parsed = ModuleDescriptor::deserialize(
        &serde_json::to_value(range_descriptor(
            Control::range("set-band", "low", "high", "Range").rail(RailDecoration::Hue),
        ))
        .unwrap(),
    )
    .expect("a range with a rail reads back");
    assert!(matches!(
        &parsed.controls[0],
        Control::Range(RangeControl {
            rail: Some(RailDecoration::Hue),
            ..
        })
    ));
}

#[test]
fn layout_defaults_to_stacked_and_tabs_needs_at_least_two_top_level_groups() {
    let stacked = descriptor();
    assert_eq!(
        stacked.layout,
        ModuleLayout::Stacked,
        "the default is stacked"
    );
    stacked.validate().expect("stacked is always accepted");

    let tabs = ModuleDescriptor {
        layout: ModuleLayout::Tabs,
        ..two_group_descriptor()
    };
    tabs.validate()
        .expect("tabs is accepted over at least two top-level groups");

    let one_group = ModuleDescriptor {
        layout: ModuleLayout::Tabs,
        ..descriptor()
    };
    let error = one_group
        .validate()
        .expect_err("tabs needs at least two top-level groups");
    assert!(error.detail.contains("layout: tabs"), "{error}");

    let mut non_group = two_group_descriptor();
    non_group.layout = ModuleLayout::Tabs;
    non_group.controls.push(Control::Number(NumberControl {
        action: "set-thing".into(),
        parameter: "x".into(),
        label: "X".into(),
        style: crate::NumberStyle::Slider,
        rail: None,
        reset: None,
        variants: Vec::new(),
    }));
    let error = non_group
        .validate()
        .expect_err("tabs needs every top-level control to be a group");
    assert!(error.detail.contains("layout: tabs"), "{error}");
}
