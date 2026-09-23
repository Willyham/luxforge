//! Shared fixtures for the desktop tests. The descriptors are built here rather than taken from the
//! registry where a test proves the desktop knows no tool by name.
use crate::{
    Config,
    app::{
        Boot, Editor,
        evidence::{Evidence, parse_script},
        message::Message,
        tasks::{REQUEST_NUMBER, Refresh},
    },
};
use lightwell_core::{
    ActionDescriptor, AssetId, AssetRecord, Availability, CanvasInteraction, ClientSession,
    Control, CropPayload, EditorState, EffectStage, EntryId, HistoryEntry, HistoryPage, LayerId,
    Lineage, LineageStep, MAX_ANGLE, MIN_ANGLE, ModuleDescriptor, ParameterDescriptor,
    ParameterKind, PreviewJob, RecipeDescription, Snapshot, SourceImage,
};
use serde_json::{Map, Value, json};
use std::{collections::VecDeque, path::PathBuf, sync::atomic::Ordering};

pub(crate) const CROP_EFFECT: &str = "lightwell.geometry.crop";
pub(crate) const CROP_ASPECTS: [&str; 7] =
    ["free", "original", "1:1", "3:2", "4:3", "16:9", "custom"];

pub(crate) fn boot() -> (Editor, PathBuf) {
    let catalog = std::env::temp_dir().join(format!(
        "lightwell-desktop-{}-{}.sqlite",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    ));
    let (owner, join) = lightwell_core::OwnerHandle::start(&catalog).unwrap();
    let (editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: Config::default(),
        window: (1440.0, 900.0),
    });
    (editor, catalog)
}

pub(crate) fn finish(mut editor: Editor, catalog: PathBuf) {
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    std::fs::remove_file(catalog).unwrap();
}

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
        id: "lightwell.crop".into(),
        title: "Crop".into(),
        hint: Some("Frame, ratio and angle".into()),
        effects: vec![lightwell_core::EffectDescriptor {
            id: CROP_EFFECT.into(),
            format: 1,
            stage: EffectStage::Geometry,
            order: 0,
            maskable: false,
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
        reset: Some(lightwell_core::ResetAction {
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
        }),
        developer: false,
        collapsed: false,
        layout: lightwell_core::ModuleLayout::Stacked,
        availability: Availability::Available,
    }
}

/// One layer of the crop module's effect carrying that payload.
pub(crate) fn crop_layer(payload: CropPayload) -> lightwell_core::Layer {
    lightwell_core::Layer {
        id: LayerId::new(),
        effect_id: CROP_EFFECT.into(),
        effect_format: 1,
        payload: serde_json::to_value(payload).expect("a serializable payload"),
        mask: None,
    }
}

pub(crate) fn refresh_for(
    asset: &AssetId,
    current: &HistoryEntry,
    page: Vec<HistoryEntry>,
    lineage: &[&HistoryEntry],
    truncated: bool,
) -> Refresh {
    Refresh {
        state: EditorState {
            asset: AssetRecord {
                id: asset.clone(),
                source_root: PathBuf::new(),
                locator: PathBuf::new(),
                fingerprint: "f".into(),
                file_identity: "i".into(),
                byte_len: 0,
                width: 1,
                height: 1,
                source: lightwell_core::SourceKind::Jpeg,
            },
            revision: current.sequence,
            current_entry: current.clone(),
            redo: Vec::new(),
        },
        history: (!page.is_empty()).then_some(HistoryPage {
            entries: page,
            next_before_sequence: None,
        }),
        versions: Vec::new(),
        lineage: Lineage {
            steps: lineage
                .iter()
                .map(|entry| LineageStep {
                    entry_id: entry.id.clone(),
                    sequence: entry.sequence,
                    action_id: entry.action_id.clone(),
                    undo_parent: entry.undo_parent.clone(),
                })
                .collect(),
            next_entry_id: truncated.then(EntryId::new),
        },
        recipe: RecipeDescription {
            entry_id: current.id.clone(),
            layers: Vec::new(),
        },
        masks: lightwell_core::mask::commands::MaskListing {
            entry_id: current.id.clone(),
            masks: Vec::new(),
        },
        original: None,
        job: PreviewJob {
            registry: std::sync::Arc::new(lightwell_core::ModuleRegistry::builtin()),
            source: lightwell_core::PreviewSource::Jpeg(SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "f".into(),
                orientation: 1,
            }),
            recipe: current.snapshot.recipe.clone(),
            entry: current.clone(),
            layer_count: None,
            draft_revision: None,
            // The histogram is a later task; this preview asks for no reduction.
            identity: lightwell_core::analysis::AnalysisIdentity::of(
                &current.asset_id,
                "f",
                current,
                &current.snapshot.recipe,
                None,
                Some((1, 1)),
            )
            .expect("a test analysis identity"),
            analyse: false,
            proxy: None,
            mask_overlay: None,
        },
        session: ClientSession::default(),
        sequence: 7,
    }
}

/// An editor with the crop module discovered and one asset open at that revision, whose stack is
/// those layers.
pub(crate) fn opened(
    layers: Vec<lightwell_core::Layer>,
    revision: u64,
) -> (Editor, PathBuf, AssetId, EntryId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::ModulesLoaded(Ok(vec![crop_descriptor()])));
    let asset = AssetId::new();
    let mut current = entry(&asset, revision, None);
    for layer in layers {
        current.snapshot = current.snapshot.append(layer).expect("a valid stack");
    }
    let entry_id = current.id.clone();
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
    assert!(editor.editable(), "{}", editor.status);
    (editor, catalog, asset, entry_id)
}

/// An editor with an evidence run attached and a script queued, so steps can be driven without a
/// window. Nothing is captured here: the capture itself needs a real renderer.
pub(crate) fn scripted(steps: &str) -> (Editor, PathBuf, AssetId, PathBuf) {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    let dir = attach_script(&mut editor, steps);
    (editor, catalog, asset, dir)
}

/// Attach an evidence run with this script to an editor that is already open, for the suites that
/// build their own. Returns the run's directory.
pub(crate) fn attach_script(editor: &mut Editor, steps: &str) -> PathBuf {
    let script = parse_script(steps).expect("a valid script");
    let dir = std::env::temp_dir().join(format!(
        "lightwell-script-{}-{}",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    ));
    editor.evidence = Some(Evidence {
        dir: dir.clone(),
        queue: VecDeque::new(),
        opens: 1,
        script,
        step: 0,
        awaiting: None,
        current: None,
        steps: Vec::new(),
        frames: Vec::new(),
        capture_pending: false,
        capture_overlay: false,
        saving: false,
        had_errors: false,
        paced_slider: None,
        paced_stroke: None,
        tools_scroll: None,
    });
    editor.activity.requested = 1;
    dir
}

/// An editor with the registered modules discovered and one empty-stack asset open, which is what
/// a canvas pick needs: a declared pick action, a stack to locate in and a displayed entry. The
/// session is put into the point-pick module's own canvas mode, because a pick answers to the mode
/// that is on screen; the owner's copy is what a `workspace.set` round trip would have adopted.
pub(crate) fn picking() -> (Editor, PathBuf, EntryId) {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
    assert!(editor.modules_ready);
    assert_eq!(editor.displayed_entry(), Some(entry_id.clone()));
    editor.session.workspace.mode = pick_mode(&editor);
    (editor, catalog, entry_id)
}

/// The module whose canvas declares a plain point pick.
pub(crate) fn pick_mode(editor: &Editor) -> String {
    let (action, _, _) = crate::state::tools::point_pick(&editor.modules).expect("a canvas pick");
    editor
        .modules
        .iter()
        .find(|module| module.action(action).is_some())
        .expect("the module declaring the pick action")
        .id
        .clone()
}

/// The names the pick action declares for its coordinate fields.
pub(crate) fn pick_fields(editor: &Editor) -> (String, String, String) {
    let (action, x, y) = crate::state::tools::point_pick(&editor.modules).expect("a canvas pick");
    (action.to_owned(), x.to_owned(), y.to_owned())
}

/// The module whose canvas declares a sample-apply pick, with the query and action it names.
pub(crate) fn sample_mode(editor: &Editor) -> (String, String, String) {
    editor
        .modules
        .iter()
        .find_map(|module| match module.canvas.as_ref()? {
            lightwell_core::CanvasInteraction::SampleApply { query, action, .. } => {
                Some((module.id.clone(), query.clone(), action.clone()))
            }
            _ => None,
        })
        .expect("a declared sample-apply canvas")
}

/// Attach a real diagnostics log so the evidence records a pick writes can be read back.
pub(crate) fn attach_log(editor: &mut Editor) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-pick-{}-{}.jsonl",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    ));
    editor.diagnostics = Some(crate::diagnostics::Diagnostics::start(&path).expect("a fresh log"));
    path
}

/// Close the attached log and return the records the harness would read.
pub(crate) fn logged(editor: &mut Editor, path: &PathBuf) -> Vec<Value> {
    assert!(
        editor.diagnostics.take().expect("an attached log").finish(),
        "the log flushed"
    );
    let text = std::fs::read_to_string(path).expect("the log file");
    std::fs::remove_file(path).expect("the log is removed");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("a JSON record"))
        .collect()
}

/// The `canvas_pick` details the diagnostics log holds, in order.
pub(crate) fn pick_events(records: &[Value]) -> Vec<&Value> {
    records
        .iter()
        .filter(|record| record["event"] == json!("canvas_pick"))
        .map(|record| &record["detail"])
        .collect()
}

pub(crate) fn evidence(editor: &Editor) -> &Evidence {
    editor.evidence.as_ref().expect("an evidence run")
}

/// The descriptors the desktop would fetch through `module.list` from the linked registry.
pub(crate) fn descriptors() -> Vec<ModuleDescriptor> {
    lightwell_core::ModuleRegistry::builtin()
        .descriptors()
        .into_iter()
        .cloned()
        .collect()
}

/// One `preset.list` row: a Lightwell preset when `report` is `None`, an imported one otherwise,
/// holding one Basic exposure field.
pub(crate) fn listed(
    name: &str,
    group: &str,
    report: Option<lightwell_core::ReportCounts>,
) -> lightwell_core::PresetSummary {
    lightwell_core::PresetSummary {
        id: lightwell_core::PresetId::new(),
        name: name.into(),
        group: group.into(),
        settings: json!({"set-basic": {"exposure": 0.5}})
            .as_object()
            .cloned()
            .expect("an object"),
        origin: match report {
            Some(_) => lightwell_core::PresetOrigin::LightroomXmp {
                file_name: Some("look.xmp".into()),
                uuid: None,
                process_version: None,
                preset_type: None,
            },
            None => lightwell_core::PresetOrigin::Lightwell {},
        },
        report,
        actor: "test".into(),
        created_ms: 0,
        updated_ms: 0,
        unavailable: Vec::new(),
    }
}
