//! Shared fixtures for the desktop tests. The descriptors are built here rather than taken from the
//! registry where a test proves the desktop knows no tool by name.
use crate::{
    Config,
    app::{
        Boot, Editor,
        draft::{CoreDraft, GestureId, Round},
        evidence::{Evidence, parse_script},
        gesture::{Gesture, Kind},
        message::{DraftMessage, Message, SyncMessage},
        tasks::{REQUEST_NUMBER, Refresh, RoundTrip},
    },
};
use lightwell_core::{
    AssetId, AssetRecord, ClientSession, Draft, DraftId, EditorState, EntryId, HistoryEntry,
    HistoryPage, HistoryRow, Lineage, LineageStep, PreviewJob, RecipeDescription, SourceImage,
};
use serde_json::{Value, json};

pub(crate) use crate::state::testing::{
    CROP_ASPECTS, Z6_AS_SHOT, Z6_CAM_XYZ, controls_descriptor, crop_descriptor, crop_layer,
    described, descriptors, entry, listed, raw_entry, raw_source,
};
use std::{collections::VecDeque, path::PathBuf, sync::atomic::Ordering};

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
        client: None,
        initial_import: None,
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
            entries: page.iter().map(HistoryRow::from).collect(),
            next_before_sequence: None,
        }),
        versions: Some(Vec::new()),
        lineage: Some(Lineage {
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
        }),
        recipe: RecipeDescription {
            entry_id: current.id.clone(),
            layers: Vec::new(),
        },
        current_recipe: None,
        masks: lightwell_core::mask::commands::MaskListing {
            entry_id: current.id.clone(),
            masks: Vec::new(),
        },
        original: None,
        job: PreviewJob {
            registry: std::sync::Arc::new(lightwell_core::ModuleRegistry::builtin()),
            context: lightwell_core::RenderContext::new(),
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
        request: None,
    }
}

/// The refresh the owner answers for a RAW photograph showing `current`: a RAW source, and recipe
/// rows the core's own RAW module described, values included.
pub(crate) fn raw_refresh(asset: &AssetId, current: &HistoryEntry) -> Refresh {
    use lightwell_core::ToolModule;
    let mut refresh = refresh_for(asset, current, vec![current.clone()], &[current], false);
    refresh.state.asset.source = raw_source();
    let module = lightwell_core::RawModule::new();
    refresh.recipe.layers = current
        .snapshot
        .recipe
        .layers
        .iter()
        .map(|layer| lightwell_core::LayerDescription {
            id: layer.id.clone(),
            effect: layer.effect_id.clone(),
            module: Some(module.descriptor().id.clone()),
            title: Some(module.descriptor().title.clone()),
            summary: module
                .describe_layer(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("a RAW summary"),
            values: module
                .values(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("RAW values"),
            available: true,
            mask: layer.mask.clone(),
            artifacts: layer.artifacts.clone(),
            neutral: module
                .is_neutral(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("RAW neutrality"),
        })
        .collect();
    refresh
}

/// An editor with the crop module discovered and one asset open at that revision, whose stack is
/// those layers.
pub(crate) fn opened(
    layers: Vec<lightwell_core::Layer>,
    revision: u64,
) -> (Editor, PathBuf, AssetId, EntryId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
        crop_descriptor(),
    ]))));
    let asset = AssetId::new();
    let mut current = entry(&asset, revision, None);
    for layer in layers {
        current.snapshot = current.snapshot.append(layer).expect("a valid stack");
    }
    let entry_id = current.id.clone();
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert!(editor.editable(), "{}", editor.status);
    (editor, catalog, asset, entry_id)
}

/// An evidence run with a script queued and one open frame already captured, for an editor built
/// any way a test likes. Nothing is captured here: the capture itself needs a real renderer.
pub(crate) fn scripted_evidence(steps: &str) -> Evidence {
    Evidence {
        dir: std::env::temp_dir().join(format!(
            "lightwell-script-{}-{}",
            std::process::id(),
            REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        )),
        queue: VecDeque::new(),
        opens: 1,
        script: parse_script(steps).expect("a valid script"),
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
        second_click: None,
        tools_scroll: None,
        capability_wait: None,
        wait_until: None,
        sync: crate::app::evidence::CaptureSync::default(),
    }
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
    let evidence = scripted_evidence(steps);
    let dir = evidence.dir.clone();
    editor.evidence = Some(evidence);
    editor.activity.requested = 1;
    dir
}

/// An editor with the registered modules discovered and one empty-stack asset open, which is what
/// a canvas pick needs: a declared pick action, a stack to locate in and a displayed entry. The
/// session is put into the point-pick module's own canvas mode, because a pick answers to the mode
/// that is on screen; the owner's copy is what a `workspace.set` round trip would have adopted.
pub(crate) fn picking() -> (Editor, PathBuf, EntryId) {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
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

/// Put an open slider gesture of this control in the editor's one slot directly, its `draft.begin`
/// on its way, as a test that is about something else needs one to be there.
pub(crate) fn hold_slider(editor: &mut Editor, action: &str, parameter: &str) {
    let asset = editor
        .state
        .as_ref()
        .expect("a photograph")
        .asset
        .id
        .clone();
    let gesture = editor.next_gesture();
    let (draft, _) = CoreDraft::open(gesture, 0, None);
    editor.gesture = Some(Gesture::Core(crate::app::gesture::CoreGesture {
        asset,
        draft,
        kind: Kind::Slider(crate::app::gesture::SliderGesture {
            action: action.into(),
            parameter: parameter.into(),
            label: parameter.into(),
            target: Default::default(),
            unpreviewed: false,
        }),
    }));
}

/// The core draft of the open gesture, or of the discarded one still closing.
pub(crate) fn core_draft(editor: &Editor) -> Option<&CoreDraft> {
    match editor.gesture.as_ref()? {
        Gesture::Core(gesture) => Some(&gesture.draft),
        Gesture::Closing { draft, .. } => Some(draft),
        Gesture::Crop(_) => None,
    }
}

/// The local identity of the open or closing core gesture, which its owner answers name.
pub(crate) fn gesture_of(editor: &Editor) -> GestureId {
    core_draft(editor).expect("a core gesture").gesture
}

/// Answer the open gesture's `draft.begin` with this draft, as its task would, without an owner.
pub(crate) fn answer_begin(editor: &mut Editor, draft: Draft) {
    let gesture = gesture_of(editor);
    let _ = editor.update(Message::Draft(DraftMessage::Begun {
        gesture,
        result: Ok(Box::new(draft)),
    }));
}

/// Answer the open gesture's `draft.commit`, as its task would.
pub(crate) fn answer_commit(editor: &mut Editor, result: Result<Option<Refresh>, String>) {
    let draft = core_draft(editor).expect("a core gesture");
    let (gesture, draft) = (
        draft.gesture,
        draft.draft_id.clone().expect("an open core draft"),
    );
    let _ = editor.update(Message::Draft(DraftMessage::Committed {
        gesture,
        draft,
        result: result.map(|refresh| refresh.map(Box::new)),
    }));
}

/// Answer the open gesture's `draft.reapply`, as its task would.
pub(crate) fn answer_reapply(editor: &mut Editor, result: Result<Draft, String>) {
    let draft = core_draft(editor).expect("a core gesture");
    let (gesture, draft) = (
        draft.gesture,
        draft.draft_id.clone().expect("an open core draft"),
    );
    let _ = editor.update(Message::Draft(DraftMessage::Reapplied {
        gesture,
        draft,
        result: result.map(Box::new),
    }));
}

/// Answer a closing gesture's `draft.cancel`, as its task would, with no frame read after it.
pub(crate) fn answer_cancel(editor: &mut Editor) {
    let draft = core_draft(editor)
        .and_then(|draft| draft.draft_id.clone())
        .expect("a closing core draft");
    let _ = editor.update(Message::Draft(DraftMessage::Cancelled {
        draft,
        cancelled: Ok(()),
        reseed: None,
    }));
}

/// Run the owner round trip the open or closing core gesture is waiting on, through the plain call
/// its task runs, and hand the answer back as the runtime does. Returns the round that was run.
///
/// A `draft.begin` that displaced an armed brush cancels that brush's draft first, in its own task;
/// here that is whatever draft the session still holds.
pub(crate) fn run_round(editor: &mut Editor) -> Option<Round> {
    let owner = editor.owner.clone();
    let client = editor.client;
    let draft = core_draft(editor)?.clone();
    let round = draft.in_flight()?;
    match round {
        Round::Begin => {
            let gesture = editor.core_gesture().expect("an open gesture");
            let asset = gesture.asset.clone();
            let (action, target) = match &gesture.kind {
                Kind::Slider(slider) => (slider.action.clone(), slider.target.clone()),
                Kind::Mask(mask) => (
                    mask.shape.method().expect("a method").to_owned(),
                    lightwell_core::mask::commands::MaskTarget {
                        mask: mask.shape.mask.clone(),
                        component: mask.shape.component.clone(),
                        ..Default::default()
                    },
                ),
            };
            if let Ok((session, _)) =
                crate::app::tasks::call(&owner, client, "session.state", json!({}))
                && let Some(held) = session["draft"]["draft_id"].as_str()
            {
                let _ = crate::app::tasks::call(
                    &owner,
                    client,
                    "draft.cancel",
                    json!({"draft_id": held}),
                );
            }
            let result = crate::app::tasks::call(
                &owner,
                client,
                "draft.begin",
                crate::app::tasks::draft_begin_params(asset, &action, target),
            )
            .and_then(|(value, _)| {
                serde_json::from_value::<Draft>(value).map_err(|e| e.to_string())
            });
            let _ = editor.update(Message::Draft(DraftMessage::Begun {
                gesture: draft.gesture,
                result: result.map(Box::new),
            }));
        }
        Round::Commit => {
            let gesture = editor.core_gesture().expect("an open gesture");
            let asset = gesture.asset.clone();
            let draft_id = draft.draft_id.clone().expect("an open core draft");
            let result = crate::app::tasks::draft_commit_now(
                &owner,
                client,
                &draft_id,
                asset,
                crate::app::tasks::mutation(draft.base_revision),
                None,
            );
            answer_commit(editor, result);
        }
        Round::Reapply => {
            let draft_id = draft.draft_id.clone().expect("an open core draft");
            let result = crate::app::tasks::draft_reapply_now(&owner, client, &draft_id);
            let _ = editor.update(Message::Draft(DraftMessage::Reapplied {
                gesture: draft.gesture,
                draft: draft_id,
                result: result.map(Box::new),
            }));
        }
        Round::Cancel => {
            let draft_id = draft.draft_id.clone().expect("a closing core draft");
            let reseed = matches!(editor.gesture, Some(Gesture::Closing { reseed: true, .. }))
                .then(|| editor.state.as_ref().map(|state| state.asset.id.clone()))
                .flatten()
                .map(|asset| (asset, editor.displayed_entry(), None));
            let (cancelled, reseed) =
                crate::app::tasks::draft_cancel_now(&owner, client, &draft_id, reseed);
            let _ = editor.update(Message::Draft(DraftMessage::Cancelled {
                draft: draft_id,
                cancelled,
                reseed,
            }));
        }
        Round::Set => return None,
    }
    Some(round)
}

/// An owner that accepts every `draft.set`: the next draft revision, the fields merged, and a
/// preview job for the current entry. Tests without a real photograph answer through it.
pub(crate) fn accepted_set(
    editor: &Editor,
    draft_id: &DraftId,
    fields: &Value,
) -> Result<(Draft, PreviewJob, RoundTrip), String> {
    let state = editor.state.as_ref().ok_or("no photograph is open")?;
    let gesture = editor.core_gesture().ok_or("no gesture is open")?;
    let action = match &gesture.kind {
        Kind::Slider(slider) => slider.action.clone(),
        Kind::Mask(mask) => mask.shape.method().unwrap_or_default().to_owned(),
    };
    let mut draft = editor
        .session
        .draft
        .clone()
        .filter(|held| &held.draft_id == draft_id)
        .unwrap_or_else(|| {
            let mut draft =
                Draft::new(&action, state.asset.id.clone(), gesture.draft.base_revision);
            draft.draft_id = draft_id.clone();
            draft
        });
    draft.draft_revision += 1;
    if let Some(fields) = fields.as_object() {
        draft.fields.extend(fields.clone());
    }
    let current = &state.current_entry;
    let job = refresh_for(&state.asset.id, current, Vec::new(), &[current], false).job;
    let now = std::time::Instant::now();
    Ok((
        draft,
        job,
        RoundTrip {
            queued: now,
            started: now,
            answered: now,
            planned: now,
        },
    ))
}
