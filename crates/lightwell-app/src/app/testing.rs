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
    });
    (editor, catalog)
}

pub(crate) fn finish(mut editor: Editor, catalog: PathBuf) {
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    std::fs::remove_file(catalog).unwrap();
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
        }],
        actions: vec![
            ActionDescriptor {
                id: "crop".into(),
                title: "Crop".into(),
                notes: "test".into(),
                summary: Some("Crop {angle}°".into()),
                parameters: crop,
            },
            ActionDescriptor {
                id: "crop-fit".into(),
                title: "Fit crop".into(),
                notes: "test".into(),
                summary: Some("Crop {aspect}".into()),
                parameters: fit,
            },
            ActionDescriptor {
                id: "crop-reset".into(),
                title: "Reset crop".into(),
                notes: "test".into(),
                summary: None,
                parameters: Vec::new(),
            },
        ],
        controls: vec![Control::Group {
            label: "Crop".into(),
            reset: None,
            controls: vec![Control::Action {
                action: "crop-reset".into(),
                label: "Reset crop".into(),
                preset: Map::new(),
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
        original: None,
        job: PreviewJob {
            registry: std::sync::Arc::new(lightwell_core::ModuleRegistry::builtin()),
            source: SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "f".into(),
                orientation: 1,
            },
            entry: current.clone(),
            layer_count: None,
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
    let script = parse_script(steps).expect("a valid script");
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
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
        saving: false,
        had_errors: false,
    });
    editor.activity.requested = 1;
    (editor, catalog, asset, dir)
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
