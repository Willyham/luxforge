//! Descriptors, parameters and canvas modes the descriptor tests build on, each written with the
//! typed builders.
use super::*;
use serde_json::{Map, Value, json};

pub(super) fn integer(name: &str) -> ParameterDescriptor {
    ParameterDescriptor::integer(name, 0, 10)
        .required(true)
        .unit("px")
        .notes("test")
}

pub(super) fn number(name: &str, min: f64, max: f64) -> ParameterDescriptor {
    ParameterDescriptor::number(name, min, max)
        .required(true)
        .notes("test")
}

/// A descriptor whose one parameter carries these decimal hints and no controls, so only the
/// hint rule under test can fail.
pub(super) fn with_hints(
    step: Option<f64>,
    precision: Option<u8>,
    parameter: ParameterDescriptor,
) -> ModuleDescriptor {
    ModuleDescriptor {
        actions: vec![ActionDescriptor {
            parameters: vec![ParameterDescriptor {
                step,
                precision,
                ..parameter
            }],
            ..action()
        }],
        controls: Vec::new(),
        reset: None,
        ..descriptor()
    }
}

pub(super) fn enumerated(name: &str) -> ParameterDescriptor {
    ParameterDescriptor::enumeration(name, vec!["free", "1:1"])
        .required(true)
        .notes("test")
}

pub(super) fn frame_canvas(action: &str, fit_action: &str) -> CanvasInteraction {
    CanvasInteraction::CropFrame {
        action: action.into(),
        angle: "angle".into(),
        x: "x".into(),
        y: "y".into(),
        width: "width".into(),
        height: "height".into(),
        fit_action: fit_action.into(),
        aspect: "aspect".into(),
        title: "Frame".into(),
        shortcut: Some("R".into()),
        icon: None,
    }
}

/// A frame action, a fit action and the canvas that binds them: the shape the crop module
/// declares, used here to prove every crop-frame rejection.
pub(super) fn frame_descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        actions: vec![
            ActionDescriptor {
                parameters: vec![
                    number("angle", -45.0, 45.0),
                    number("x", 0.0, 1.0),
                    number("y", 0.0, 1.0),
                    number("width", 0.0, 1.0),
                    number("height", 0.0, 1.0),
                ],
                ..ActionDescriptor::new("set-frame", "Set frame", "test")
            },
            ActionDescriptor {
                parameters: vec![enumerated("aspect"), number("angle", -45.0, 45.0)],
                ..ActionDescriptor::new("fit-frame", "Fit frame", "test")
            },
        ],
        controls: vec![Control::Number(NumberControl {
            action: "set-frame".into(),
            parameter: "angle".into(),
            label: "Angle".into(),
            style: crate::NumberStyle::Slider,
            rail: None,
            reset: None,
            variants: Vec::new(),
        })],
        // The shared descriptor's reset names an action this one does not declare.
        reset: None,
        canvas: Some(frame_canvas("set-frame", "fit-frame")),
        ..descriptor()
    }
}

/// The frame descriptor with one action's parameter list replaced and no controls, so only the
/// canvas rule under test can fail.
pub(super) fn frame_with(
    action_id: &str,
    parameters: Vec<ParameterDescriptor>,
) -> ModuleDescriptor {
    let mut descriptor = frame_descriptor();
    descriptor.controls = Vec::new();
    for action in &mut descriptor.actions {
        if action.id == action_id {
            action.parameters = parameters.clone();
        }
    }
    descriptor
}

/// The read-only query shape a module declares: two integer coordinates.
pub(super) fn query() -> ActionDescriptor {
    ActionDescriptor {
        parameters: vec![integer("x"), integer("y")],
        ..ActionDescriptor::new("neutral-sample", "Neutral sample", "test")
    }
}

pub(super) fn sample_apply(query: &str, x: &str, y: &str, action: &str) -> CanvasInteraction {
    CanvasInteraction::SampleApply {
        query: query.into(),
        x: x.into(),
        y: y.into(),
        action: action.into(),
        title: "Pick".into(),
        shortcut: Some("W".into()),
        icon: None,
    }
}

/// A module declaring one query and the sample-apply canvas that binds it to an action: the
/// shape the Basic module declares, used here to prove every sample-apply rejection.
pub(super) fn sample_descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        queries: vec![query()],
        // A pick canvas declares the picker control that reaches it.
        controls: vec![Control::Picker(PickerControl {
            label: "Pick".into(),
            variants: Vec::new(),
        })],
        canvas: Some(sample_apply("neutral-sample", "x", "y", "set-thing")),
        ..descriptor()
    }
}

pub(super) fn action() -> ActionDescriptor {
    ActionDescriptor {
        parameters: vec![
            integer("x"),
            ParameterDescriptor::color("rgb")
                .required(true)
                .notes("test"),
            ParameterDescriptor::enumeration("mode", vec!["fast", "exact"])
                .default(json!("exact"))
                .notes("test"),
        ],
        ..ActionDescriptor::new("set-thing", "Set thing", "test")
    }
}

/// A `points` parameter declaring these bounds, on a module with no controls: a path is drawn
/// on the canvas and the control vocabulary has no widget for one, so the parameter stands on
/// its own and only the bounds rule under test can fail.
pub(super) fn points_module(points_min: usize, points_max: usize) -> ModuleDescriptor {
    ModuleDescriptor {
        actions: vec![ActionDescriptor {
            parameters: vec![
                ParameterDescriptor::points("path", points_min, points_max)
                    .required(true)
                    .notes("the drawn path"),
            ],
            ..action()
        }],
        controls: Vec::new(),
        reset: None,
        ..descriptor()
    }
}

/// The test module with one number control whose field reset runs `action` with `preset`.
pub(super) fn number_reset(action: &str, preset: Value) -> ModuleDescriptor {
    ModuleDescriptor {
        controls: vec![Control::Number(NumberControl {
            action: "set-thing".into(),
            parameter: "x".into(),
            label: "X".into(),
            style: crate::NumberStyle::Slider,
            rail: None,
            reset: Some(ResetAction {
                action: action.into(),
                preset: preset.as_object().unwrap().clone(),
            }),
            variants: Vec::new(),
        })],
        ..descriptor()
    }
}

pub(super) fn descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: "test.module".into(),
        title: "Test".into(),
        hint: Some("A test module".into()),
        effects: vec![EffectDescriptor::new(
            "test.module.effect",
            EffectStage::Pixel,
        )],
        actions: vec![action()],
        queries: Vec::new(),
        controls: vec![Control::Group(GroupControl {
            label: "Test".into(),
            reset: Some(ResetAction {
                action: "set-thing".into(),
                preset: json!({"x": 0}).as_object().unwrap().clone(),
            }),
            controls: vec![
                Control::Number(NumberControl {
                    action: "set-thing".into(),
                    parameter: "x".into(),
                    label: "X".into(),
                    style: crate::NumberStyle::Slider,
                    rail: None,
                    reset: None,
                    variants: Vec::new(),
                }),
                Control::Action(ActionControl {
                    action: "set-thing".into(),
                    label: "Apply".into(),
                    preset: Map::new(),
                    style: crate::ActionStyle::Default,
                    icon: None,
                    variants: Vec::new(),
                }),
            ],
            collapsed: false,
            variants: Vec::new(),
        })],
        reset: Some(ResetAction {
            action: "set-thing".into(),
            preset: Map::new(),
        }),
        canvas: None,
        developer: false,
        collapsed: false,
        layout: ModuleLayout::Stacked,
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    }
}

pub(super) fn string(name: &str, max_length: usize, required: bool) -> ParameterDescriptor {
    ParameterDescriptor {
        kind: ParameterKind::String { max_length },
        required,
        unit: None,
        ..integer(name)
    }
}

pub(super) fn settings(name: &str) -> ParameterDescriptor {
    ParameterDescriptor {
        kind: ParameterKind::Settings,
        unit: None,
        ..integer(name)
    }
}

/// A module shaped like the presets module: one action taking a settings set, a name and an
/// optional library identity, and the one presets control that submits it.
pub(super) fn presets_descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        effects: Vec::new(),
        actions: vec![ActionDescriptor {
            parameters: vec![
                settings("settings"),
                string("name", 128, true),
                string("preset-id", 96, false),
            ],
            ..ActionDescriptor::new("apply-thing", "Apply thing", "test")
        }],
        controls: vec![Control::Presets(PresetsControl {
            action: "apply-thing".into(),
        })],
        reset: None,
        ..descriptor()
    }
}

/// The presets descriptor with its action's parameters replaced.
pub(super) fn presets_with(parameters: Vec<ParameterDescriptor>) -> ModuleDescriptor {
    let mut descriptor = presets_descriptor();
    descriptor.actions[0].parameters = parameters;
    descriptor
}

/// Every way a presets control can be bound wrongly, with the fragment its error names.
pub(super) fn presets_cases() -> Vec<(&'static str, ModuleDescriptor, &'static str)> {
    vec![
        (
            "a presets control naming an undeclared action",
            ModuleDescriptor {
                controls: vec![Control::Presets(PresetsControl {
                    action: "missing".into(),
                })],
                ..presets_descriptor()
            },
            "references undeclared action missing",
        ),
        (
            "a presets action without settings",
            presets_with(vec![string("name", 128, true)]),
            "action apply-thing has no parameter settings",
        ),
        (
            "a presets action whose settings is another kind",
            presets_with(vec![
                ParameterDescriptor {
                    kind: ParameterKind::Curve {
                        points_min: 2,
                        points_max: 4,
                        monotone: false,
                        fixed_x: None,
                    },
                    unit: None,
                    ..integer("settings")
                },
                string("name", 128, true),
            ]),
            "needs a required settings parameter settings",
        ),
        (
            "a presets action whose settings is optional",
            presets_with(vec![
                ParameterDescriptor {
                    required: false,
                    ..settings("settings")
                },
                string("name", 128, true),
            ]),
            "needs a required settings parameter settings",
        ),
        (
            "a presets action without a name",
            presets_with(vec![settings("settings")]),
            "action apply-thing has no parameter name",
        ),
        (
            "a presets action whose name is not a string",
            presets_with(vec![settings("settings"), enumerated("name")]),
            "needs a required string parameter name",
        ),
        (
            "a presets action whose name has a default",
            presets_with(vec![
                settings("settings"),
                ParameterDescriptor {
                    default: Some(json!("Preset")),
                    ..string("name", 128, true)
                },
            ]),
            "needs a required string parameter name",
        ),
        (
            "a presets action whose library identity is required",
            presets_with(vec![
                settings("settings"),
                string("name", 128, true),
                string("preset-id", 96, true),
            ]),
            "may declare only an optional string parameter preset-id",
        ),
        (
            "a presets action whose library identity is not a string",
            presets_with(vec![
                settings("settings"),
                string("name", 128, true),
                ParameterDescriptor {
                    required: false,
                    ..integer("preset-id")
                },
            ]),
            "may declare only an optional string parameter preset-id",
        ),
        (
            "a presets action with another parameter",
            presets_with(vec![
                settings("settings"),
                string("name", 128, true),
                integer("x"),
            ]),
            "declares parameter x beyond settings, name and preset-id",
        ),
        (
            "a presets control on a field patch",
            ModuleDescriptor {
                actions: vec![ActionDescriptor {
                    patch: true,
                    ..presets_descriptor().actions[0].clone()
                }],
                ..presets_descriptor()
            },
            "is a field patch",
        ),
        (
            "two presets controls in one module",
            ModuleDescriptor {
                controls: vec![
                    Control::Presets(PresetsControl {
                        action: "apply-thing".into(),
                    }),
                    Control::Group(GroupControl {
                        label: "Nested".into(),
                        reset: None,
                        controls: vec![Control::Presets(PresetsControl {
                            action: "apply-thing".into(),
                        })],
                        collapsed: false,
                        variants: Vec::new(),
                    }),
                ],
                ..presets_descriptor()
            },
            "declares 2 presets controls; a module declares at most one",
        ),
    ]
}

pub(super) fn controls_descriptor() -> ModuleDescriptor {
    let mut descriptor = descriptor();
    let bool_param = ParameterDescriptor {
        name: "enabled".into(),
        kind: ParameterKind::Boolean,
        required: false,
        default: Some(json!(false)),
        ..number("enabled", 0.0, 1.0)
    };
    let curve_kind = ParameterKind::Curve {
        points_min: 2,
        points_max: 4,
        monotone: true,
        fixed_x: Some(vec![0.0, 0.5, 1.0]),
    };
    let curve_param = ParameterDescriptor {
        name: "curve".into(),
        kind: curve_kind,
        required: false,
        default: Some(json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]])),
        ..number("curve", 0.0, 1.0)
    };
    let mut numeric = number("amount", -10.0, 10.0);
    numeric.soft_min = Some(-5.0);
    numeric.soft_max = Some(5.0);
    numeric.fine_step = Some(0.01);
    numeric.zero = Some(0.0);
    let action = ActionDescriptor {
        patch: true,
        parameters: vec![
            bool_param,
            enumerated("mode"),
            curve_param.clone(),
            numeric,
            ParameterDescriptor {
                kind: ParameterKind::Color,
                ..number("rgb", 0.0, 1.0)
            },
        ],
        ..ActionDescriptor::new("set-controls", "Set controls", "test")
    };
    descriptor.actions = vec![action];
    descriptor.queries = vec![ActionDescriptor {
        parameters: vec![ParameterDescriptor {
            required: false,
            default: None,
            ..curve_param
        }],
        ..ActionDescriptor::new("sample-curve", "Sample curve", "test")
    }];
    descriptor.controls = vec![
        Control::Toggle(ToggleControl {
            action: "set-controls".into(),
            parameter: "enabled".into(),
            label: "Enabled".into(),
        }),
        Control::Choice(ChoiceControl {
            action: "set-controls".into(),
            parameter: "mode".into(),
            label: "Mode".into(),
            style: ChoiceStyle::Menu,
        }),
        Control::Number(NumberControl {
            action: "set-controls".into(),
            parameter: "amount".into(),
            label: "Amount".into(),
            style: NumberStyle::Stepper,
            rail: Some(RailDecoration::Hue),
            reset: None,
            variants: Vec::new(),
        }),
        Control::Curve(CurveControl {
            action: "set-controls".into(),
            channels: vec![CurveChannel {
                parameter: "curve".into(),
                label: "Master".into(),
            }],
            label: "Curve".into(),
            sample_query: "sample-curve".into(),
            background: CurveBackground::Histogram,
        }),
        Control::Action(ActionControl {
            action: "set-controls".into(),
            label: "Run".into(),
            preset: json!({"amount": 0.0}).as_object().unwrap().clone(),
            style: ActionStyle::Icon,
            icon: Some("rotate-left".into()),
            variants: Vec::new(),
        }),
        Control::Color(ColorControl {
            action: "set-controls".into(),
            parameter: "rgb".into(),
            label: "Colour".into(),
            style: ColorStyle::Picker,
        }),
        Control::Group(GroupControl {
            label: "More".into(),
            controls: Vec::new(),
            reset: None,
            collapsed: true,
            variants: Vec::new(),
        }),
    ];
    descriptor.reset = None;
    descriptor
}

/// A module whose one action declares a band: two edges on one axis, two shoulder widths, an
/// integer and a second action, so each refusal below has something of the wrong shape to bind.
pub(super) fn range_descriptor(control: impl Into<Control>) -> ModuleDescriptor {
    let level = |name: &str| number(name, 0.0, 100.0);
    let mut descriptor = descriptor();
    descriptor.actions = vec![
        ActionDescriptor {
            patch: true,
            parameters: vec![
                level("low"),
                number("low-feather", 0.0, 50.0),
                level("high"),
                number("high-feather", 0.0, 50.0),
                number("wide", 0.0, 200.0),
                integer("count"),
            ],
            ..ActionDescriptor::new("set-band", "Set band", "test")
        },
        action(),
    ];
    descriptor.controls = vec![control.into()];
    descriptor.reset = None;
    descriptor
}

/// A descriptor with two top-level groups, each one slider, over the base descriptor's own
/// declared action: the minimal shape `layout: tabs` accepts.
pub(super) fn two_group_descriptor() -> ModuleDescriptor {
    let group = |label: &str| {
        Control::Group(GroupControl {
            label: label.into(),
            reset: None,
            controls: vec![Control::Number(NumberControl {
                action: "set-thing".into(),
                parameter: "x".into(),
                label: "X".into(),
                style: crate::NumberStyle::Slider,
                rail: None,
                reset: None,
                variants: Vec::new(),
            })],
            collapsed: false,
            variants: Vec::new(),
        })
    };
    ModuleDescriptor {
        controls: vec![group("First"), group("Second")],
        ..descriptor()
    }
}
