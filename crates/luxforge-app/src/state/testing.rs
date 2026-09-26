//! Fixtures shared by the view-model and desktop tests: descriptors, entries and recipe rows built
//! from core values alone, with no editor, so the view model's tests reach nothing in `app/`. The
//! descriptors are built here rather than taken from the registry where a test proves the desktop
//! knows no tool by name.
use luxforge_core::{
    ActionDescriptor, AssetId, Availability, CanvasInteraction, Control, CropPayload, EffectStage,
    EntryId, HistoryEntry, LayerId, MAX_ANGLE, MIN_ANGLE, ModuleDescriptor, ParameterDescriptor,
    ParameterKind, RecipeDescription, Snapshot,
};
use serde_json::{Map, Value, json};

pub(crate) const CROP_EFFECT: &str = "luxforge.geometry.crop";

pub(crate) const CROP_ASPECTS: [&str; 7] =
    ["free", "original", "1:1", "3:2", "4:3", "16:9", "custom"];

/// A descriptor-only fixture: the desktop must generate these controls without knowing a
/// provider's identity. The production developer proof is tested separately through the API.
pub(crate) fn controls_descriptor() -> ModuleDescriptor {
    let mut parameters = vec![
        json!({"name":"amount","kind":"number","min":-10.0,"max":10.0,
            "soft_min":-5.0,"soft_max":5.0,"step":0.1,"fine_step":0.01,"zero":0.0,"default":0.0}),
        json!({"name":"count","kind":"integer","min":0,"max":20,"step":1.0,"default":2}),
        json!({"name":"enabled","kind":"boolean","default":false}),
        json!({"name":"mode","kind":"enum","options":["one","two","three"],"default":"one"}),
        json!({"name":"rgb","kind":"color","default":[32,64,128]}),
        json!({"name":"master","kind":"curve","points_min":2,"points_max":8,"monotone":true,
            "step":0.01,"default":[[0.0,0.0],[0.5,0.5],[1.0,1.0]]}),
        json!({"name":"red","kind":"curve","points_min":2,"points_max":8,"monotone":false,
            "step":0.01,"default":[[0.0,0.0],[0.5,0.5],[1.0,1.0]]}),
        json!({"name":"coordinate","kind":"number","min":0.0,"max":100.0,"step":1.0,"fine_step":0.1,"default":5.0}),
    ];
    for parameter in &mut parameters {
        parameter["required"] = json!(false);
        parameter["notes"] =
            json!("Descriptor fixture; curve samples come from its declared query");
    }
    let queries: Vec<Value> = parameters
        .iter()
        .filter(|parameter| parameter["kind"] == "curve")
        .cloned()
        .map(|mut parameter| {
            parameter.as_object_mut().unwrap().remove("default");
            parameter
        })
        .collect();
    ModuleDescriptor::parse(&json!({
        "id":"fixture.controls", "title":"Fixture controls", "effects":[],
        "actions":[{"id":"fixture-set","title":"Set fixture","notes":"One field patch",
            "patch":true,"parameters":parameters}],
        "queries":[{"id":"fixture-samples","title":"Sample curve","notes":"Module samples",
            "parameters":queries}],
        "controls":[{"kind":"group","label":"Fixture group","collapsed":false,"controls":[
            {"kind":"number","action":"fixture-set","parameter":"amount","label":"Amount","rail":"temperature"},
            {"kind":"number","action":"fixture-set","parameter":"count","label":"Count","style":"stepper"},
            {"kind":"number","action":"fixture-set","parameter":"coordinate","label":"Coordinate","style":"field"},
            {"kind":"toggle","action":"fixture-set","parameter":"enabled","label":"Enabled"},
            {"kind":"choice","action":"fixture-set","parameter":"mode","label":"Mode","style":"menu"},
            {"kind":"color","action":"fixture-set","parameter":"rgb","label":"Colour","style":"picker"},
            {"kind":"curve","action":"fixture-set","label":"Curve","sample_query":"fixture-samples",
                "channels":[{"parameter":"master","label":"Master"},{"parameter":"red","label":"Red"}],
                "background":"histogram"},
            {"kind":"action","action":"fixture-set","label":"Reset amount","style":"icon","icon":"reset","preset":{"amount":0.0}}
        ]}], "availability":{"kind":"available"}
    })).expect("the whole-vocabulary fixture is a valid descriptor")
}

/// A `layout: tabs` fixture with two top-level groups, each one slider over its own field of the
/// same patch action: the minimal shape the colour mixer declares, used to test tab selection
/// without depending on the mixer module being linked.
pub(crate) fn tabs_descriptor() -> ModuleDescriptor {
    ModuleDescriptor::parse(&json!({
        "id":"fixture.tabs", "title":"Fixture tabs", "effects":[], "layout":"tabs",
        "actions":[{"id":"fixture-set","title":"Set fixture","notes":"One field patch",
            "patch":true,"parameters":[
                {"name":"first","kind":"number","min":-10.0,"max":10.0,"default":0.0,
                    "required":false,"notes":"test"},
                {"name":"second","kind":"number","min":-10.0,"max":10.0,"default":0.0,
                    "required":false,"notes":"test"}
            ]}],
        "controls":[
            {"kind":"group","label":"First","collapsed":false,"controls":[
                {"kind":"number","action":"fixture-set","parameter":"first","label":"First"}
            ]},
            {"kind":"group","label":"Second","collapsed":false,"controls":[
                {"kind":"number","action":"fixture-set","parameter":"second","label":"Second"}
            ]}
        ], "availability":{"kind":"available"}
    }))
    .expect("a two-group layout: tabs fixture is a valid descriptor")
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
    let mut crop = vec![number(
        "angle",
        MIN_ANGLE,
        MAX_ANGLE,
        false,
        Some(json!(0.0)),
    )];
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
        number("angle", MIN_ANGLE, MAX_ANGLE, false, Some(json!(0.0))),
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
        }],
        actions: vec![
            ActionDescriptor {
                id: "crop".into(),
                title: "Crop".into(),
                notes: "test".into(),
                summary: Some("Crop {angle}°".into()),
                patch: false,
                parameters: crop,
            },
            ActionDescriptor {
                id: "crop-fit".into(),
                title: "Fit crop".into(),
                notes: "test".into(),
                summary: Some("Crop {aspect}".into()),
                patch: false,
                parameters: fit,
            },
            ActionDescriptor {
                id: "crop-reset".into(),
                title: "Reset crop".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: Vec::new(),
            },
        ],
        queries: Vec::new(),
        controls: vec![Control::Group {
            label: "Crop".into(),
            reset: None,
            collapsed: false,
            controls: vec![Control::Action {
                action: "crop-reset".into(),
                label: "Reset crop".into(),
                preset: Map::new(),
                style: Default::default(),
                icon: None,
            }],
        }],
        reset: Some(luxforge_core::ResetAction {
            action: "crop-reset".into(),
            preset: Map::new(),
        }),
        canvas: Some(CanvasInteraction::CropFrame {
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

/// The rows the owner's `recipe.describe` gives an entry, from the core's own built-in modules:
/// each layer's provider, summary and values, and the core's answer to whether it is neutral. The
/// desktop derives none of this from a payload, so its tests describe a stack as the owner does.
/// Each row's input stage is left unknown; [`described_at`] reports it for a test that reads it.
pub(crate) fn described(entry: &HistoryEntry) -> RecipeDescription {
    describe(entry, None)
}

/// [`described`] for an asset of `source` extents, with each row's input stage the core's own
/// stage fold reports ([`luxforge_core::ModuleRegistry::input_stages`]), as the owner does.
pub(crate) fn described_at(entry: &HistoryEntry, source: (u32, u32)) -> RecipeDescription {
    describe(entry, Some(source))
}

fn describe(entry: &HistoryEntry, source: Option<(u32, u32)>) -> RecipeDescription {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let stages = source.map_or_else(Vec::new, |(width, height)| {
        registry.input_stages(width, height, &entry.snapshot.recipe)
    });
    RecipeDescription {
        entry_id: entry.id.clone(),
        layers: entry
            .snapshot
            .recipe
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                let module = registry.effect(&layer.effect_id).map(|(module, _)| module);
                let read = |layer: &luxforge_core::Layer| {
                    module.and_then(|module| {
                        Some((
                            module
                                .describe_layer(
                                    &layer.effect_id,
                                    layer.effect_format,
                                    &layer.payload,
                                )
                                .ok()?,
                            module
                                .values(&layer.effect_id, layer.effect_format, &layer.payload)
                                .ok()?,
                        ))
                    })
                };
                let (summary, values) = read(layer).unwrap_or_default();
                luxforge_core::LayerDescription {
                    id: layer.id.clone(),
                    effect: layer.effect_id.clone(),
                    module: module.map(|module| module.descriptor().id.clone()),
                    title: module.map(|module| module.descriptor().title.clone()),
                    summary,
                    values,
                    available: module.is_some(),
                    mask: layer.mask.clone(),
                    artifacts: layer.artifacts.clone(),
                    neutral: registry.layer_neutral(layer),
                    input_stage: stages.get(index).map(|stage| luxforge_core::StageSize {
                        width: stage.width,
                        height: stage.height,
                    }),
                }
            })
            .collect(),
    }
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

/// The descriptors the desktop would fetch through `module.list` from the linked registry.
pub(crate) fn descriptors() -> Vec<ModuleDescriptor> {
    luxforge_core::ModuleRegistry::builtin()
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
