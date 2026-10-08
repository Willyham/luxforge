//! Fixtures shared by the view-model and desktop tests: descriptors, entries and recipe rows built
//! from core values alone, with no editor, so the view model's tests reach nothing in `app/`. The
//! descriptors are built here rather than taken from the registry where a test proves the desktop
//! knows no tool by name.
use luxforge_core::{
    ActionDescriptor, ActionStyle, AssetId, Availability, CanvasInteraction, ChoiceStyle,
    ColorStyle, Control, CropPayload, CurveBackground, CurveChannel, EffectStage, EntryId,
    HistoryEntry, LayerId, MAX_ANGLE, MIN_ANGLE, ModuleDescriptor, ModuleLayout, NumberStyle,
    ParameterDescriptor, ParameterKind, RailDecoration, RecipeDescription, Snapshot,
};
use serde_json::{Map, Value, json};

/// Record `selection` of the open photograph in `session` as the owner records it, without moving
/// the session's generation: how a test puts the desktop on a previewed entry or back on current.
/// With no photograph open there is nothing to preview, so nothing is recorded.
pub(crate) fn show(
    session: &mut luxforge_core::ClientSession,
    state: Option<&luxforge_core::EditorState>,
    selection: luxforge_core::HistorySelection,
) {
    let Some(state) = state else {
        return;
    };
    let asset = state.asset.id.clone();
    match selection {
        luxforge_core::HistorySelection::Current => {
            session.preview.selections.remove(&asset);
        }
        luxforge_core::HistorySelection::Entry(entry_id) => {
            session.preview.selections.insert(
                asset,
                luxforge_core::AssetSelection {
                    entry_id,
                    geometry_from: None,
                },
            );
        }
    }
}

pub(crate) const CROP_EFFECT: &str = "luxforge.geometry.crop";

pub(crate) const CROP_ASPECTS: [&str; 7] =
    ["free", "original", "1:1", "3:2", "4:3", "16:9", "custom"];

/// A descriptor-only fixture: the desktop must generate these controls without knowing a
/// provider's identity. The production developer proof is tested separately through the API.
pub(crate) fn controls_descriptor() -> ModuleDescriptor {
    const SET: &str = "fixture-set";
    const NOTES: &str = "Descriptor fixture; curve samples come from its declared query";
    let identity = json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
    let curve = |name: &str, monotone: bool| {
        let curve = ParameterDescriptor::curve(name, 2, 8)
            .step(0.01)
            .notes(NOTES);
        if monotone { curve.monotone() } else { curve }
    };
    let descriptor = ModuleDescriptor {
        id: "fixture.controls".into(),
        title: "Fixture controls".into(),
        actions: vec![ActionDescriptor {
            preset: true,
            patch: true,
            parameters: vec![
                ParameterDescriptor::number("amount", -10.0, 10.0)
                    .soft_min(-5.0)
                    .soft_max(5.0)
                    .step(0.1)
                    .fine_step(0.01)
                    .zero(0.0)
                    .default(0.0)
                    .notes(NOTES),
                ParameterDescriptor::integer("count", 0, 20)
                    .step(1.0)
                    .default(2)
                    .notes(NOTES),
                ParameterDescriptor::boolean("enabled")
                    .default(false)
                    .notes(NOTES),
                ParameterDescriptor::enumeration("mode", ["one", "two", "three"])
                    .default("one")
                    .notes(NOTES),
                ParameterDescriptor::color("rgb")
                    .default(json!([32, 64, 128]))
                    .notes(NOTES),
                curve("master", true).default(identity.clone()),
                curve("red", false).default(identity),
                ParameterDescriptor::number("coordinate", 0.0, 100.0)
                    .step(1.0)
                    .fine_step(0.1)
                    .default(5.0)
                    .notes(NOTES),
            ],
            ..ActionDescriptor::new(SET, "Set fixture", "One field patch")
        }],
        queries: vec![ActionDescriptor {
            preset: true,
            parameters: vec![curve("master", true), curve("red", false)],
            ..ActionDescriptor::new("fixture-samples", "Sample curve", "Module samples")
        }],
        controls: vec![
            Control::group(
                "Fixture group",
                vec![
                    Control::number(SET, "amount", "Amount")
                        .rail(RailDecoration::Temperature)
                        .into(),
                    Control::number(SET, "count", "Count")
                        .number_style(NumberStyle::Stepper)
                        .into(),
                    Control::number(SET, "coordinate", "Coordinate")
                        .number_style(NumberStyle::Field)
                        .into(),
                    Control::toggle(SET, "enabled", "Enabled").into(),
                    Control::choice(SET, "mode", "Mode")
                        .choice_style(ChoiceStyle::Menu)
                        .into(),
                    Control::color_field(SET, "rgb", "Colour")
                        .color_style(ColorStyle::Picker)
                        .into(),
                    Control::curve(
                        SET,
                        [("master", "Master"), ("red", "Red")]
                            .map(|(parameter, label)| CurveChannel {
                                parameter: parameter.into(),
                                label: label.into(),
                            })
                            .into(),
                        "Curve",
                        "fixture-samples",
                    )
                    .background(CurveBackground::Histogram)
                    .into(),
                    Control::action(SET, "Reset amount")
                        .action_style(ActionStyle::Icon)
                        .icon("reset")
                        .preset(Map::from_iter([("amount".to_owned(), json!(0.0))]))
                        .into(),
                ],
            )
            .into(),
        ],
        ..ModuleDescriptor::default()
    };
    descriptor
        .validate()
        .expect("the whole-vocabulary fixture is a valid descriptor");
    descriptor
}

/// A `layout: tabs` fixture with two top-level groups, each one slider over its own field of the
/// same patch action: the minimal shape the colour mixer declares, used to test tab selection
/// without depending on the mixer module being linked.
pub(crate) fn tabs_descriptor() -> ModuleDescriptor {
    const SET: &str = "fixture-set";
    let field = |name: &str| {
        ParameterDescriptor::number(name, -10.0, 10.0)
            .default(0.0)
            .notes("test")
    };
    let group = |label: &str, name: &str| -> Control {
        Control::group(label, vec![Control::number(SET, name, label).into()]).into()
    };
    let descriptor = ModuleDescriptor {
        id: "fixture.tabs".into(),
        title: "Fixture tabs".into(),
        layout: ModuleLayout::Tabs,
        actions: vec![ActionDescriptor {
            preset: true,
            patch: true,
            parameters: vec![field("first"), field("second")],
            ..ActionDescriptor::new(SET, "Set fixture", "One field patch")
        }],
        controls: vec![group("First", "first"), group("Second", "second")],
        ..ModuleDescriptor::default()
    };
    descriptor
        .validate()
        .expect("a two-group layout: tabs fixture is a valid descriptor");
    descriptor
}

pub(crate) fn entry(asset: &AssetId, sequence: u64, parent: Option<&EntryId>) -> HistoryEntry {
    HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence,
        action_id: "test".into(),
        label: "Test".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: sequence,
        result_revision: sequence,
        snapshot: Snapshot::original(asset.clone()),
        undo_parent: parent.cloned(),
        restore_target: None,
    }
}

/// The crop module's descriptor as the desktop would fetch it through `module.list`. It is built
/// here rather than taken from the registry so these tests do not depend on the module being
/// linked: the desktop drives everything from the descriptor and knows no tool by name.
pub(crate) fn crop_descriptor() -> ModuleDescriptor {
    let number = |name: &str, min: f64, max: f64, required: bool, default: Option<Value>| {
        ParameterDescriptor {
            name: name.into(),
            kind: ParameterKind::Number { min, max },
            required,
            default,
            unit: None,
            step: None,
            precision: None,
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
            notes: "test".into(),
        }
    };
    let rectangle = ["x", "y", "width", "height"]
        .map(|name| number(name, 0.0, 1.0, true, None))
        .to_vec();
    // The angle as the crop module declares it: its unit and the steps its stepper moves by.
    let angle = || {
        number("angle", MIN_ANGLE, MAX_ANGLE, false, Some(json!(0.0)))
            .unit("deg")
            .step(0.5)
            .fine_step(0.05)
    };
    let mut crop = vec![angle()];
    crop.extend(rectangle.clone());
    let mut fit = vec![
        ParameterDescriptor {
            name: "aspect".into(),
            kind: ParameterKind::Enum {
                options: CROP_ASPECTS.iter().map(|option| (*option).into()).collect(),
            },
            required: false,
            default: Some(json!("free")),
            unit: None,
            step: None,
            precision: None,
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
            notes: "test".into(),
        },
        number("aspect-width", 1.0, 10000.0, false, None),
        number("aspect-height", 1.0, 10000.0, false, None),
        angle(),
    ];
    fit.push(number("center-x", 0.0, 1.0, false, None));
    fit.push(number("center-y", 0.0, 1.0, false, None));
    ModuleDescriptor {
        id: "luxforge.crop".into(),
        title: "Crop".into(),
        hint: Some("Frame, ratio and angle".into()),
        effects: vec![luxforge_core::EffectDescriptor {
            id: CROP_EFFECT.into(),
            format: 1,
            stage: EffectStage::Geometry,
            order: 10,
            maskable: false,
            artifacts: false,
            single: false,
            sources: Vec::new(),
        }],
        actions: vec![
            ActionDescriptor {
                preset: true,
                id: "crop".into(),
                title: "Crop".into(),
                notes: "test".into(),
                patch: false,
                parameters: crop,
            },
            ActionDescriptor {
                preset: true,
                id: "crop-fit".into(),
                title: "Fit crop".into(),
                notes: "test".into(),
                patch: false,
                parameters: fit,
            },
            ActionDescriptor {
                preset: true,
                id: "crop-reset".into(),
                title: "Reset crop".into(),
                notes: "test".into(),
                patch: false,
                parameters: Vec::new(),
            },
        ],
        queries: Vec::new(),
        controls: vec![Control::Group(luxforge_core::GroupControl {
            label: "Crop".into(),
            reset: None,
            collapsed: false,
            layout: luxforge_core::ModuleLayout::Stacked,
            view: false,
            controls: vec![Control::Action(luxforge_core::ActionControl {
                action: "crop-reset".into(),
                label: "Reset crop".into(),
                preset: Map::new(),
                style: Default::default(),
                icon: None,
                variants: Vec::new(),
            })],
            variants: Vec::new(),
        })],
        reset: Some(luxforge_core::ResetAction {
            action: "crop-reset".into(),
            preset: Map::new(),
        }),
        canvas: Some(CanvasInteraction::CropFrame {
            effect: CROP_EFFECT.into(),
            action: "crop".into(),
            angle: "angle".into(),
            x: "x".into(),
            y: "y".into(),
            width: "width".into(),
            height: "height".into(),
            fit_action: "crop-fit".into(),
            aspect: "aspect".into(),
            title: "Crop".into(),
            shortcut: Some("R".into()),
            icon: Some("crop".into()),
        }),
        developer: false,
        collapsed: false,
        layout: luxforge_core::ModuleLayout::Stacked,
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    }
}

/// One layer of the crop module's effect carrying that payload.
pub(crate) fn crop_layer(payload: CropPayload) -> luxforge_core::Layer {
    luxforge_core::Layer {
        id: LayerId::new(),
        effect_id: CROP_EFFECT.into(),
        effect_format: 1,
        payload: serde_json::to_value(payload).expect("a serializable payload"),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The rows the owner's `recipe.describe` gives an entry, from the core's own built-in modules and
/// its own description ([`luxforge_core::ModuleRegistry::describe_recipe`]): each layer's provider,
/// summary and values, and the core's answer to whether it is neutral. The desktop derives none of
/// this from a payload, so its tests describe a stack as the owner does. Each row's input stage and
/// orientation are left unknown; [`described_at`] reports them for a test that reads them.
pub(crate) fn described(entry: &HistoryEntry) -> RecipeDescription {
    let mut described = described_at(entry, (1, 1));
    for row in &mut described.layers {
        row.input_stage = None;
        row.input_orientation = None;
    }
    described.output_stage = None;
    described.output_orientation = None;
    described
}

/// [`described`] for an asset of `source` extents, with each row's input stage and orientation and
/// the stack's output as the core's own stage fold reports them, as the owner does.
pub(crate) fn described_at(entry: &HistoryEntry, source: (u32, u32)) -> RecipeDescription {
    luxforge_core::ModuleRegistry::developer().describe_recipe(source.0, source.1, entry)
}

/// The Nikon Z6's camera matrix and as-shot gains, from the supplied NEF's metadata: a real camera
/// whose as-shot white balance has a temperature and tint equivalent in range.
pub(crate) const Z6_CAM_XYZ: [[f32; 3]; 4] = [
    [0.9943, -0.3269, -0.0839],
    [-0.5323, 1.3269, 0.2259],
    [-0.1198, 0.2083, 0.7557],
    [0.0; 3],
];

pub(crate) const Z6_AS_SHOT: [f32; 3] = [1.683_593_8, 1.0, 1.345_703_1];

/// A RAW source as the owner reports one: the Z6's typed camera interpretation, read from the JSON
/// object `asset.state` carries.
pub(crate) fn raw_source() -> luxforge_core::SourceKind {
    let rect = json!({"x": 0, "y": 0, "width": 6048, "height": 4032});
    serde_json::from_value(json!({
        "kind": "raw",
        "metadata": {
            "make": "Nikon",
            "model": "Z 6",
            "mode": "NikonZ6Lossless14",
            "layout": "mosaic",
            "sensor_width": 6048,
            "sensor_height": 4032,
            "active_area": rect,
            "default_crop": rect,
            "cfa_width": 2,
            "cfa_height": 2,
            "cfa": [0, 1, 1, 2],
            "black_cfa": [0, 0, 0, 0],
            "black_base": 1008.0,
            "black_channels": [0.0, 0.0, 0.0, 0.0],
            "black_repeat_width": 0,
            "black_repeat_height": 0,
            "black_repeat": [],
            "sensor_white": 15520.0,
            "as_shot_gains": Z6_AS_SHOT,
            "libraw_flip": 0,
            "rgb_cam": [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            "cam_xyz": Z6_CAM_XYZ,
            "backend": "test",
            "exif_orientation": 1,
            "libraw_inset": null,
            "format_identity": "test",
            "warnings": [],
        },
    }))
    .expect("a RAW interpretation")
}

/// An entry whose stack is one RAW development layer holding `payload`.
pub(crate) fn raw_entry(
    asset: &AssetId,
    sequence: u64,
    parent: Option<&EntryId>,
    payload: &luxforge_core::RawPayload,
) -> HistoryEntry {
    let mut entry = entry(asset, sequence, parent);
    entry.snapshot = entry
        .snapshot
        .with_layer_inserted(0, payload.layer(LayerId::new()))
        .expect("a RAW development layer");
    entry
}

/// The descriptors a developer run of the desktop, as a debug build is, would fetch through
/// `module.list`: every linked module, the pixel and controls proofs included.
pub(crate) fn descriptors() -> Vec<ModuleDescriptor> {
    luxforge_core::ModuleRegistry::developer()
        .descriptors()
        .into_iter()
        .cloned()
        .collect()
}

/// One `preset.list` row: a Luxforge preset when `report` is `None`, an imported one otherwise,
/// holding one Basic exposure field.
pub(crate) fn listed(
    name: &str,
    group: &str,
    report: Option<luxforge_core::ReportCounts>,
) -> luxforge_core::PresetSummary {
    luxforge_core::PresetSummary {
        id: luxforge_core::PresetId::new(),
        name: name.into(),
        group: group.into(),
        settings: json!({"set-basic": {"exposure": 0.5}})
            .as_object()
            .cloned()
            .expect("an object"),
        origin: match report {
            Some(_) => luxforge_core::PresetOrigin::LightroomXmp {
                file_name: Some("look.xmp".into()),
                uuid: None,
                process_version: None,
                preset_type: None,
            },
            None => luxforge_core::PresetOrigin::Luxforge {},
        },
        report,
        actor: "test".into(),
        created_ms: 0,
        updated_ms: 0,
        unavailable: Vec::new(),
    }
}

/// A nonlinear fixture shared by canvas mapping tests. It uses the production core registry.
pub(crate) fn perspective_mapping() -> luxforge_core::GeometryMap {
    let mut recipe = luxforge_core::Recipe::default();
    recipe.layers.push(luxforge_core::Layer {
        id: luxforge_core::LayerId::new(),
        effect_id: luxforge_core::PERSPECTIVE_EFFECT.into(),
        effect_format: 1,
        payload: json!({"horizontal": 40, "vertical": -25}),
        mask: None,
        artifacts: Vec::new(),
    });
    luxforge_core::stage_transform(
        &luxforge_core::ModuleRegistry::assemble(&luxforge_core::RegistryOptions::default())
            .unwrap(),
        6000,
        4000,
        &recipe,
    )
    .unwrap()
}

pub(crate) use luxforge_core::Orientation as TestOrientation;
pub(crate) fn nonlinear_mapping(
    orientation: luxforge_core::Orientation,
    horizontal: i64,
    vertical: i64,
) -> luxforge_core::GeometryMap {
    use luxforge_core::{EditorService, Layer, ModuleRegistry, Mutation, Recipe};
    use serde_json::json;
    static LENS: std::sync::OnceLock<Layer> = std::sync::OnceLock::new();
    let lens = LENS
        .get_or_init(|| {
            let catalog = luxforge_testbase::paths::temp_path("canvas-warp.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg");
            let asset = service.import(&source).unwrap().asset.id;
            let entry = service.state(&asset).unwrap().current_entry.id;
            let rows = luxforge_testbase::wait_for("the offline lens index", || {
                match service.run_query(
                    &asset,
                    &entry,
                    "lens-profiles",
                    json!({"assume-uncorrected":true}),
                ) {
                    Ok(rows) => Some(rows),
                    Err(error) if error.kind == luxforge_core::ErrorKind::NotReady => None,
                    Err(error) => panic!("{error}"),
                }
            });
            // The detected profile, offered in the answer's status rather than as a row.
            let row = &rows["status"]["suggestion"];
            assert!(
                row["match"] == "lens-model" && row["eligible"] == true,
                "{rows}"
            );
            service
                .apply_action(
                    &asset,
                    Mutation {
                        expected_revision: 0,
                        request_id: "canvas-lens".into(),
                        actor: "test".into(),
                    },
                    "select-lens-profile",
                    json!({"profile":row["key"],"focal":24.0,"assume-uncorrected":true}),
                )
                .unwrap();
            let lens = service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers[0]
                .clone();
            drop(service);
            std::fs::remove_file(catalog).unwrap();
            lens
        })
        .clone();
    let recipe = Recipe {
        layers: vec![
            Layer::orientation(orientation),
            lens,
            Layer::new(
                luxforge_core::PERSPECTIVE_EFFECT,
                json!({"horizontal":horizontal,"vertical":vertical}),
            ),
        ],
        ..Recipe::default()
    };
    let map =
        luxforge_core::stage_transform(&ModuleRegistry::builtin(), 6000, 4000, &recipe).unwrap();
    // The map the UI receives has crossed the same JSON boundary as render.transform.
    serde_json::from_value(serde_json::to_value(map).unwrap()).unwrap()
}
