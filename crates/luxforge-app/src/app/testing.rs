//! Shared fixtures for the desktop tests. The descriptors are built here rather than taken from the
//! registry where a test proves the desktop knows no tool by name.
use crate::{
    Config,
    app::{
        Boot, Editor,
        draft::{CoreDraft, Round},
        evidence::{Evidence, parse_script},
        gesture::{CoreGesture, Kind},
        message::{Message, control::ControlMessage, draft::DraftMessage, sync::SyncMessage},
        tasks::{REQUEST_NUMBER, Refresh, RoundTrip},
    },
    state::histogram::Analysis,
};
use luxforge_core::{
    AssetId, AssetRecord, ClientSession, Draft, DraftId, EditorState, EntryId, Evaluation,
    HistoryEntry, HistoryPage, HistoryRow, Lineage, LineageStep, ModuleDescriptor, PreviewJob,
    RecipeDescription, SourceImage,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, path::PathBuf, sync::Arc, sync::atomic::Ordering};

pub(crate) use crate::state::testing::{
    CROP_ASPECTS, Z6_AS_SHOT, Z6_CAM_XYZ, controls_descriptor, crop_descriptor, crop_layer,
    described, described_at, descriptors, entry, listed, raw_entry, raw_source,
};

/// What a test preview job's evaluation is built from.
pub(crate) struct Parts {
    pub(crate) registry: Arc<luxforge_core::ModuleRegistry>,
    pub(crate) source: luxforge_core::PreviewSource,
    pub(crate) entry: HistoryEntry,
    pub(crate) recipe: luxforge_core::Recipe,
}

/// `job` evaluating what `change` makes of its parts. An evaluation is immutable and compiled
/// once, so a test that wants another stack, source, entry or registry builds another one. The job
/// keeps its draft and how it is presented; its identity is the new evaluation's.
pub(crate) fn rebuild(job: &mut PreviewJob, change: impl FnOnce(&mut Parts)) {
    let held = &job.evaluation;
    let mut parts = Parts {
        registry: held.registry().clone(),
        source: held.source().clone(),
        entry: held.entry().clone(),
        recipe: held.recipe().clone(),
    };
    change(&mut parts);
    let evaluation = Evaluation::new(
        parts.registry,
        held.context().clone(),
        parts.source,
        parts.entry,
        parts.recipe,
        held.draft().cloned(),
    );
    job.identity = evaluation.identity().expect("a test analysis identity");
    job.evaluation = evaluation;
}

pub(crate) fn boot() -> (Editor, PathBuf) {
    let (editor, _, catalog) = boot_with(Config::default());
    (editor, catalog)
}

/// An editor started from `config`, the startup task it asked the runtime for, and its catalog.
pub(crate) fn boot_with(config: Config) -> (Editor, iced::Task<Message>, PathBuf) {
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-desktop-{}-{}.sqlite",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    ));
    let (owner, join) = luxforge_core::OwnerHandle::start(&catalog).unwrap();
    let (editor, startup) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config,
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    (editor, startup, catalog)
}

/// An editor with every built-in discovered and one real photograph open, imported by a second
/// client of the same owner that stands in for an independent JSON client, and read back into the
/// editor as a command's completion does. Returns the photograph and that client.
pub(crate) fn real_photo(catalog: &std::path::Path) -> (Editor, AssetId, luxforge_core::ClientId) {
    real_photo_at(
        catalog,
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg"),
    )
}

/// The same stack as `stack` — the same entry, recipe and source identity, so the same analysis
/// identity — over a new allocation of its pixels, and a handle that says whether anything still
/// holds that allocation: what a test needs to see that the desktop kept no stack. A RAW
/// development's planes would hold the source worker's memory gate for as long.
pub(crate) fn fresh_stack(stack: &Evaluation) -> (Evaluation, std::sync::Weak<Vec<u8>>) {
    let luxforge_core::PreviewSource::Jpeg(image) = stack.source() else {
        panic!("a JPEG stack");
    };
    let pixels = Arc::new(image.rgba.as_ref().clone());
    let held = Arc::downgrade(&pixels);
    let source = luxforge_core::PreviewSource::Jpeg(SourceImage {
        rgba: pixels,
        ..image.clone()
    });
    let evaluation = Evaluation::new(
        stack.registry().clone(),
        stack.context().clone(),
        source,
        stack.entry().clone(),
        stack.recipe().clone(),
        None,
    );
    (evaluation, held)
}

/// Develop the photograph at `fixture` as `client` opens a file, on the owner's own blocking wait
/// (the one the desktop's open tasks use, never a poll of `job.read`), and ask for its
/// preparation: answers the photograph and its preparation's source job.
pub(crate) fn develop_and_prepare(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    fixture: &std::path::Path,
) -> (AssetId, String) {
    use crate::app::tasks::{developed_photograph, prepare_photograph, queue_import};
    let queued = queue_import(owner, client, fixture).expect("a Develop");
    let asset = developed_photograph(owner, client, queued.job_id()).expect("the Develop");
    let job = prepare_photograph(owner, client, &asset).expect("a preparation");
    (asset, job)
}

/// Open the photograph at `fixture` as `client`: develop it, wait for its source job on the owner's
/// own blocking wait and adopt it. Answers the adopted asset.
pub(crate) fn import_and_adopt(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    fixture: &std::path::Path,
) -> AssetId {
    use crate::app::tasks::{call, wait_source_job};
    let (asset, job) = develop_and_prepare(owner, client, fixture);
    wait_source_job(owner, client, &job).expect("source preparation");
    call(owner, client, "job.adopt", json!({"job_id": job})).expect("an adoption");
    asset
}

/// [`real_photo`] of the photograph at `fixture`. A RAW original is decoded and developed on the
/// owner's own source worker before the editor opens it.
pub(crate) fn real_photo_at(
    catalog: &std::path::Path,
    fixture: &std::path::Path,
) -> (Editor, AssetId, luxforge_core::ClientId) {
    let (owner, join) = luxforge_core::OwnerHandle::start(catalog).unwrap();
    let agent = owner.register();
    let asset = import_and_adopt(&owner, agent, fixture);
    let (mut editor, _) = Editor::new(Boot {
        owner: owner.clone(),
        join,
        live_server: None,
        config: Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let refreshed = crate::app::tasks::refresh(
        &owner,
        editor.client,
        asset.clone(),
        crate::app::tasks::Scope::Open,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    assert_eq!(
        editor.gesture_refusal(crate::app::gesture::Starting::Action),
        None,
        "{}",
        editor.status.text
    );
    (editor, asset, agent)
}

/// The loop timing's last derive as [`mark_no_derive`] leaves it: a value no derive reports.
const NO_DERIVE: f64 = 99_000.0;

/// Set the loop timing's last derive to a value neither a derive nor a skipped one reports, so a
/// later [`derive_ran`] says whether the next update derived.
pub(crate) fn mark_no_derive(editor: &Editor) {
    let mut timing = editor.log.loop_timing.get();
    timing.last_rederive_ms = NO_DERIVE;
    editor.log.loop_timing.set(timing);
}

/// The update after [`mark_no_derive`] ran a derive: a skipped one leaves the mark or the zero a
/// fast path reports.
pub(crate) fn derive_ran(editor: &Editor) -> bool {
    let last = editor.log.loop_timing.get().last_rederive_ms;
    last != NO_DERIVE && last != 0.0
}

/// The photograph's preview work is answered and nothing else is running, as it is a moment after a
/// photograph opens: the one state in which a message that changes nothing skips the update.
pub(crate) fn idle_workers(editor: &mut Editor) {
    luxforge_testbase::wait_until("the workers going idle", || {
        let _ = editor.update(Message::Preview(
            crate::app::message::preview::PreviewMessage::Poll,
        ));
        !editor.workers_busy()
    });
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
                source: luxforge_core::SourceKind::Jpeg,
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
            geometry: None,
            entry_id: current.id.clone(),
            layers: Vec::new(),
            output_stage: None,
            output_orientation: None,
        },
        current_recipe: None,
        masks: luxforge_core::mask::commands::MaskListing {
            entry_id: current.id.clone(),
            masks: Vec::new(),
        },
        original: None,
        // The histogram is a later task; this preview asks for no reduction.
        job: PreviewJob::new(Evaluation::new(
            Arc::new(luxforge_core::ModuleRegistry::developer()),
            luxforge_core::RenderContext::new(),
            luxforge_core::PreviewSource::Jpeg(SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "f".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            current.clone(),
            current.snapshot.recipe.clone(),
            None,
        ))
        .expect("a test preview job"),
        session: ClientSession::default(),
        request: None,
        skipped: Vec::new(),
        collapsed: None,
    }
}

/// The refresh the owner answers for a RAW photograph showing `current`: a RAW source, and recipe
/// rows the core's own RAW module described, values included.
pub(crate) fn raw_refresh(asset: &AssetId, current: &HistoryEntry) -> Refresh {
    use luxforge_core::ToolModule;
    let mut refresh = refresh_for(asset, current, vec![current.clone()], &[current], false);
    refresh.state.asset.source = raw_source();
    let module = luxforge_core::RawModule::new();
    refresh.recipe.layers = current
        .snapshot
        .recipe
        .layers
        .iter()
        .map(|layer| {
            let report = module
                .describe(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("a RAW layer describes itself");
            luxforge_core::LayerDescription {
                id: layer.id.clone(),
                effect: layer.effect_id.clone(),
                module: Some(module.descriptor().id.clone()),
                title: Some(module.descriptor().title.clone()),
                summary: report.summary,
                values: report.values,
                available: true,
                mask: layer.mask.clone(),
                artifacts: layer.artifacts.clone(),
                neutral: report.neutral,
                input_stage: None,
                input_orientation: None,
            }
        })
        .collect();
    refresh
}

/// The source extents the rows of [`opened`] describe, so a crop draft opened there has a stage of
/// this size with no geometry ahead of it.
pub(crate) const CROP_SOURCE: (u32, u32) = (480, 320);

/// An editor with the crop module discovered and one asset open at that revision, whose stack is
/// those layers, described as the owner describes them for a [`CROP_SOURCE`] photograph.
pub(crate) fn opened(
    layers: Vec<luxforge_core::Layer>,
    revision: u64,
) -> (Editor, PathBuf, AssetId, EntryId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
        crop_descriptor(),
    ]))));
    // The owner does not hold this photograph, so its draft requests are answered by the stand-in.
    stand_in(&mut editor);
    let asset = AssetId::new();
    let mut current = entry(&asset, revision, None);
    for layer in layers {
        current.snapshot = current.snapshot.append(layer).expect("a valid stack");
    }
    let entry_id = current.id.clone();
    let mut refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    refresh.recipe = described_at(&current, CROP_SOURCE);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert_eq!(
        editor.gesture_refusal(crate::app::gesture::Starting::Action),
        None,
        "{}",
        editor.status.text
    );
    (editor, catalog, asset, entry_id)
}

/// An evidence run with a script queued and one open frame already captured, for an editor built
/// any way a test likes. Nothing is captured here: the capture itself needs a real renderer.
pub(crate) fn scripted_evidence(steps: &str) -> Evidence {
    Evidence {
        dir: std::env::temp_dir().join(format!(
            "luxforge-script-{}-{}",
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
        view_idle: None,
        idle: None,
        allow_unready_capture: false,
        capture_overlay: false,
        saving: false,
        had_errors: false,
        paced_slider: None,
        paced_stroke: None,
        second_click: None,
        tools_scroll: None,
        capability_wait: None,
        wait_until: None,
        warm_wait: None,
        agent: None,
        agent_wait: None,
        long_work_wait: None,
        loupe_arrows: None,
        grid_scroll: None,
        sync: crate::app::evidence::CaptureSync::default(),
        recorded: Default::default(),
        gpu_identity: None,
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
            luxforge_core::CanvasInteraction::SampleApply { query, action, .. } => {
                Some((module.id.clone(), query.clone(), action.clone()))
            }
            _ => None,
        })
        .expect("a declared sample-apply canvas")
}

/// Attach a real diagnostics log so the evidence records a pick writes can be read back. Its path is
/// the test base's scratch path, which clears whatever an earlier process with the same id left
/// there, so a reused process id never finds the log already present.
pub(crate) fn attach_log(editor: &mut Editor) -> PathBuf {
    let path = luxforge_testbase::paths::temp_path("pick.jsonl");
    editor.log.diagnostics =
        Some(crate::diagnostics::Diagnostics::start(&path).expect("a fresh log"));
    path
}

/// Close the attached log and return the records the harness would read.
pub(crate) fn logged(editor: &mut Editor, path: &PathBuf) -> Vec<Value> {
    assert!(
        editor
            .log
            .diagnostics
            .take()
            .expect("an attached log")
            .finish_within(luxforge_testbase::HANG),
        "the log flushed"
    );
    let text = std::fs::read_to_string(path).expect("the log file");
    std::fs::remove_file(path).expect("the log is removed");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("a JSON record"))
        .collect()
}

pub(crate) fn evidence(editor: &Editor) -> &Evidence {
    editor.evidence.as_ref().expect("an evidence run")
}

/// Move a slider to `value` exactly as the widget does: the rail fraction that sends that value,
/// through `ControlMessage::Fraction`, converted by the one path evidence scripts use
/// ([`crate::app::evidence::rail_fractions`]). A value the rail cannot send fails the test.
pub(crate) fn slide(
    editor: &mut Editor,
    action: &str,
    parameter: &str,
    value: f64,
) -> iced::Task<Message> {
    let fraction =
        crate::app::evidence::rail_fractions(&editor.modules, action, parameter, &[value])
            .unwrap_or_else(|reason| panic!("{reason}"))[0];
    editor.update(Message::Control(ControlMessage::Fraction {
        action: action.into(),
        parameter: parameter.into(),
        fraction,
    }))
}

/// Let go of a slider, as the widget does when the pointer is released.
pub(crate) fn let_go(editor: &mut Editor, action: &str, parameter: &str) -> iced::Task<Message> {
    editor.update(Message::Control(ControlMessage::Released {
        action: action.into(),
        parameter: parameter.into(),
    }))
}

/// A test's stand-in for the owner's draft requests, for a photograph the owner does not hold.
/// Every `draft.begin`, `draft.set` and `draft.reapply` is accepted, except that each queued
/// refusal answers one request of its kind in turn; every `draft.cancel` is accepted and reads no
/// frame back. The driver asks it exactly where it would ask the owner, synchronously.
#[derive(Default)]
pub(crate) struct StandIn {
    pub(crate) begins: VecDeque<String>,
    pub(crate) sets: VecDeque<String>,
    pub(crate) reapplies: VecDeque<String>,
}

impl StandIn {
    /// A fresh draft of `action` on the revision the desktop holds, as `draft.begin` answers.
    pub(crate) fn begin(
        &mut self,
        state: Option<&EditorState>,
        asset: AssetId,
        action: &str,
    ) -> Result<Draft, String> {
        if let Some(refusal) = self.begins.pop_front() {
            return Err(refusal);
        }
        Ok(Draft::new(
            action,
            asset,
            state.map_or(0, |state| state.revision),
        ))
    }

    /// The open gesture's draft on the revision the desktop holds, no longer conflicted, as
    /// `draft.reapply` answers.
    pub(crate) fn reapply(
        &mut self,
        state: Option<&EditorState>,
        open: Option<&CoreGesture>,
        draft_id: &DraftId,
    ) -> Result<Draft, String> {
        if let Some(refusal) = self.reapplies.pop_front() {
            return Err(refusal);
        }
        let (Some(state), Some(open)) =
            (state, open.filter(|open| &open.draft.draft_id == draft_id))
        else {
            return Err("not-found: this client holds no such draft".into());
        };
        let mut rebased = Draft::new(
            &open.kind.action().unwrap_or_default(),
            open.asset.clone(),
            state.revision,
        );
        rebased.draft_id = draft_id.clone();
        rebased.draft_revision = open.draft.draft_revision;
        Ok(rebased)
    }
}

/// Answer this editor's draft requests with the [`StandIn`] from now on, and return it so a test
/// can queue refusals.
pub(crate) fn stand_in(editor: &mut Editor) -> &mut StandIn {
    editor.stand_in.get_or_insert_with(StandIn::default)
}

/// Put an open slider gesture of this control in the editor's one slot directly, its `draft.begin`
/// answered on the revision the desktop holds, as a test that is about something else needs one to
/// be there.
pub(crate) fn hold_slider(editor: &mut Editor, action: &str, parameter: &str) {
    let state = editor.document.state.as_ref().expect("a photograph");
    let (asset, revision) = (state.asset.id.clone(), state.revision);
    let gesture = editor.next_gesture();
    let (draft, _) = CoreDraft::open(gesture, Draft::new(action, asset.clone(), revision), None);
    editor.gesture = Some(Box::new(crate::app::gesture::CoreGesture {
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

/// The core draft of the open gesture.
pub(crate) fn core_draft(editor: &Editor) -> Option<&CoreDraft> {
    editor.core_gesture().map(|gesture| &gesture.draft)
}

/// Answer the open gesture's `draft.commit`, as its task would.
pub(crate) fn answer_commit(editor: &mut Editor, result: Result<Option<Refresh>, String>) {
    let draft = core_draft(editor).expect("a core gesture");
    let (gesture, draft) = (draft.gesture, draft.draft_id.clone());
    let _ = editor.update(Message::Draft(DraftMessage::Committed {
        gesture,
        draft,
        result: result.map(|refresh| refresh.map(Box::new)),
    }));
}

/// Run the open gesture's `draft.commit`, the one draft round trip that is an owner task, through
/// the plain call its task runs, and hand the answer back as the runtime does. Returns whether a
/// commit was in flight.
pub(crate) fn run_commit(editor: &mut Editor) -> bool {
    let Some(draft) = core_draft(editor).cloned() else {
        return false;
    };
    if draft.in_flight() != Some(Round::Commit) {
        return false;
    }
    let asset = editor
        .core_gesture()
        .expect("an open gesture")
        .asset
        .clone();
    // The display bounds the desktop's own commit sends, so the committed stack's job is planned
    // as it is in the editor.
    let result = crate::app::tasks::draft_commit_now(
        &editor.owner,
        editor.client,
        &draft.draft_id,
        asset,
        crate::app::tasks::mutation(draft.base_revision),
        editor.proxy_bounds(),
    );
    answer_commit(editor, result);
    true
}

/// An owner that accepts every `draft.set`: the next draft revision, the fields merged, and a
/// preview job for the current entry when the gesture previews its fields. Tests without a real
/// photograph answer through it.
pub(crate) fn accepted_set(
    editor: &Editor,
    draft_id: &DraftId,
    fields: &Value,
) -> Result<(Draft, Option<PreviewJob>, RoundTrip), String> {
    let state = editor
        .document
        .state
        .as_ref()
        .ok_or("no photograph is open")?;
    let gesture = editor.core_gesture().ok_or("no gesture is open")?;
    let action = gesture.kind.action().unwrap_or_default();
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
    let job = gesture
        .kind
        .previews()
        .then(|| refresh_for(&state.asset.id, current, Vec::new(), &[current], false).job);
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

/// The first patch action any registered module declares, and its first field: the tests below
/// drive that control, so no module or parameter is named here either.
pub(crate) fn patch_control(editor: &Editor) -> (String, String) {
    // The first patch a JPEG shows: the RAW development's `set-raw` is registered earlier but
    // applies only to a RAW photo, and draws no controls of its own.
    let action = editor
        .modules
        .iter()
        .filter(|module| module.applies_to(luxforge_core::SourceTag::Jpeg))
        .flat_map(|module| module.actions.iter())
        .find(|action| action.patch)
        .expect("a built-in declares a field patch");
    (
        action.id.clone(),
        action
            .parameters
            .first()
            .expect("the patch declares a field")
            .name
            .clone(),
    )
}

/// An editor with every registered module discovered, one asset open and a diagnostics log
/// attached, so the requests a gesture sends can be counted from the records the harness reads.
pub(crate) fn drafting() -> (Editor, PathBuf, PathBuf, AssetId, String, String) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    // The owner does not hold this photograph, so its draft requests are answered by the stand-in.
    stand_in(&mut editor);
    let asset = AssetId::new();
    let current = entry(&asset, 4, None);
    let mut refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    refresh.recipe = described_at(&current, CROP_SOURCE);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    let log = attach_log(&mut editor);
    let (action, parameter) = patch_control(&editor);
    (editor, catalog, log, asset, action, parameter)
}

/// Take the started crop gesture's input stage as shown: the draft a crop test drives, its
/// `draft.begin` answered at the start and every `draft.set` answered by the [`StandIn`]. The frame
/// itself opened with the start.
pub(crate) fn open_crop(editor: &mut Editor) {
    editor.crop_stage_shown();
}

/// Put an open crop gesture with this frame and this stage in the editor's one slot directly, its
/// `draft.begin` answered, as a test that is about something else needs one there.
pub(crate) fn hold_crop(
    editor: &mut Editor,
    frame: crate::crop_draft::CropDraft,
    stage: crate::app::crop::StageView,
) {
    let asset = editor
        .document
        .state
        .as_ref()
        .expect("a photograph")
        .asset
        .id
        .clone();
    let gesture = editor.next_gesture();
    let (draft, _) = CoreDraft::open(gesture, Draft::new("crop", asset.clone(), 0), None);
    editor.gesture = Some(Box::new(crate::app::gesture::CoreGesture {
        asset,
        draft,
        kind: Kind::Crop(crate::app::crop::CropGesture {
            action: "crop".into(),
            frame,
            stage,
            frames: Default::default(),
            generation: None,
        }),
    }));
}

/// The details of every record the diagnostics log holds under `event`, in order.
pub(crate) fn events<'a>(records: &'a [Value], event: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["event"] == json!(event))
        .map(|record| &record["detail"])
        .collect()
}

/// Opens an asset with `modules` registered, so a slider or an action control has something
/// real to submit against.
pub(crate) fn opened_with_modules(
    modules: Vec<ModuleDescriptor>,
    revision: u64,
) -> (Editor, PathBuf) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(modules))));
    // The owner does not hold this photograph, so its draft requests are answered by the stand-in.
    stand_in(&mut editor);
    let asset = AssetId::new();
    let current = entry(&asset, revision, None);
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert_eq!(
        editor.gesture_refusal(crate::app::gesture::Starting::Action),
        None,
        "{}",
        editor.status.text
    );
    (editor, catalog)
}

/// Every sliders in one control tree, however deeply a module nests its groups.
pub(crate) fn flatten(
    control: &crate::state::tools::ControlModel,
) -> Vec<&crate::state::tools::SliderControl> {
    crate::state::control_tree::walk(std::slice::from_ref(control))
        .filter_map(|control| match control {
            crate::state::tools::ControlModel::Slider(slider) => Some(slider),
            _ => None,
        })
        .collect()
}

// -- The histogram inspector and its clipping toggles ----------------------------------------
/// One analysed frame as the preview worker would hand it over: a report reduced from exactly
/// these pixels, the identity the owner stores it under, and the raster kept beside it.
pub(crate) fn analysed(
    editor: &Editor,
    generation: u64,
    pixels: &[[u8; 4]],
    width: u32,
    height: u32,
) -> (Analysis, Arc<luxforge_core::Raster>) {
    let rgba: Vec<u8> = pixels.iter().flatten().copied().collect();
    let report =
        luxforge_core::analysis::reduce(&rgba, width, height, &luxforge_core::Cancel::never())
            .expect("a reduction");
    let entry_id = editor.displayed_entry().expect("a displayed entry");
    let state = editor.document.state.as_ref().expect("an open asset");
    let identity = luxforge_core::analysis::AnalysisIdentity {
        asset_id: state.asset.id.clone(),
        source_fingerprint: state.asset.fingerprint.clone(),
        entry_id,
        snapshot_id: state.current_entry.snapshot.id.clone(),
        recipe_hash: "hash".into(),
        draft: None,
        width,
        height,
        domain: luxforge_core::analysis::AnalysisDomain,
    };
    let raster = Arc::new(luxforge_core::Raster {
        width,
        height,
        rgba: rgba.into(),
        source_fingerprint: state.asset.fingerprint.clone(),
        snapshot_id: state.current_entry.snapshot.id.clone(),
    });
    (
        Analysis {
            generation,
            identity,
            report,
        },
        raster,
    )
}

/// The same frame as `analysed`, stamped with the draft it was planned from: exactly the
/// identity the core computes for a drafted preview job and for `analysis.request
/// {target: draft}`, so a report adopted here is a cache hit for that request.
pub(crate) fn drafted(
    editor: &Editor,
    generation: u64,
    draft_id: &luxforge_core::DraftId,
    draft_revision: u64,
    pixels: &[[u8; 4]],
    width: u32,
    height: u32,
) -> (Analysis, Arc<luxforge_core::Raster>) {
    let (mut analysis, raster) = analysed(editor, generation, pixels, width, height);
    analysis.identity.draft = Some(luxforge_core::DraftStamp {
        draft_id: draft_id.clone(),
        draft_revision,
    });
    (analysis, raster)
}

/// An exact frame of `generation` retained as the preview worker's exact phase leaves it: exact,
/// rendered in `render_ms`, and of no content serial yet.
pub(crate) fn exact(
    generation: u64,
    raster: Arc<luxforge_core::Raster>,
    render_ms: f64,
) -> super::preview::ExactFrame {
    super::preview::ExactFrame {
        generation,
        raster,
        approximate_white_balance: false,
        render_ms,
        content: None,
    }
}

/// An analysed frame waiting to be adopted with its pixels, as an exact phase leaves it.
pub(crate) fn incoming(
    analysis: Analysis,
    raster: Arc<luxforge_core::Raster>,
) -> Option<(Analysis, super::preview::ExactFrame)> {
    let frame = exact(analysis.generation, raster, 0.0);
    Some((analysis, frame))
}

/// A job reader's stream with the runtime that owns its timer, to follow it in a test. The stream
/// makes its interval on its first poll, in whichever runtime polls it, so every poll of one
/// stream goes through the one runtime.
pub(crate) struct Followed<M> {
    runtime: tokio::runtime::Runtime,
    stream: iced::futures::stream::BoxStream<'static, M>,
}

impl<M> Followed<M> {
    pub(crate) fn new(stream: iced::futures::stream::BoxStream<'static, M>) -> Self {
        Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a one-thread runtime with a clock"),
            stream,
        }
    }

    /// What the reader sends next, waiting for it as long as the hang bound allows, and `None`
    /// once the stream has ended. Reads that send nothing are made meanwhile, which is what a
    /// test counts them for.
    pub(crate) fn next(&mut self) -> Option<M> {
        use iced::futures::StreamExt;
        let stream = &mut self.stream;
        self.runtime
            .block_on(async { tokio::time::timeout(luxforge_testbase::HANG, stream.next()).await })
            .expect("the reader sent or ended within the hang bound")
    }
}
