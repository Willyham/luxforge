//! What the canvas shows when the preview of the displayed state fails.
//!
//! A commit whose render fails leaves history and the recipe naming the edit, so the picture from
//! before the edit must not stay on screen as though it were the edit's result — which is how a
//! RAW crop came to read "Crop 7°" in history over an uncropped photograph when its render was
//! refused. These tests drive the real preview queue: the failing job is a JPEG source whose
//! declared size is over the 512 MiB frame limit, so the worker answers `resource-limit` exactly as
//! a refused render does.
//!
//! Where a test is about the order of two jobs on the one worker — a job still running when the
//! next is requested, a job still waiting in the pending slot — the job it needs held carries a
//! [`Hold`]'s gate, so that order is decided by the test and never by how fast this machine renders
//! while it is loaded. Every wait fails at once, naming what it waited for, when nothing more can
//! arrive: the queue is idle, or its job is held at a gate this test has not opened.
use super::{
    Editor, ProxyFrame,
    crop::PendingStage,
    evidence::Settle,
    message::{CropMessage, Message, PreviewMessage, SyncMessage},
    tasks::SyncResult,
    testing::{
        attach_log, core_draft, crop_layer, entry, finish, hold_crop, logged, open_crop, opened,
        refresh_for,
    },
};
use crate::state::{canvas::PhotoView, histogram::HistogramStatus};
use lightwell_core::{
    ActionInput, ActionPlan, AssetId, Availability, BASIC_EFFECT, ColorOperation, CropPayload,
    CropStage, EFFECT_FORMAT, EffectDescriptor, EffectStage, EntryId, Error, ErrorKind,
    HistoryEntry, Layer, LayerId, ModuleDescriptor, ModuleRegistry, POINTER_MODE, PointwiseColor,
    PreviewJob, PreviewSource, Processing, SourceImage, Stage, StageContext, ToolModule, Zoom,
};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak},
    time::{Duration, Instant},
};

/// The effect a [`Hold`] puts ahead of the stack of the job it holds.
const HELD_EFFECT: &str = "test.held.effect";

/// A gate a render waits at, row by row, while it is shut. It is a pointwise colour unit that
/// leaves every pixel as it found it, so a job carrying it renders the frame it would render
/// without it; all it changes is *when* that render can finish.
struct Gate {
    state: Mutex<GateState>,
    opened: Condvar,
}

struct GateState {
    shut: bool,
    /// Renders waiting here now.
    waiting: usize,
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a render is waiting here, which nothing but the test that shut the gate can end.
    fn holding(&self) -> bool {
        let state = self.lock();
        state.shut && state.waiting > 0
    }
}

impl PointwiseColor for Gate {
    fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {
        let mut state = self.lock();
        while state.shut {
            state.waiting += 1;
            state = self
                .opened
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.waiting -= 1;
        }
    }
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "held render".into()
    }
}

/// The module that compiles [`HELD_EFFECT`] to one [`Gate`].
struct HeldModule {
    descriptor: ModuleDescriptor,
    gate: Arc<Gate>,
}

impl ToolModule for HeldModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: Map::new(),
        })
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Err(Error::validation("a held render has no action"))
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok("held render".into())
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        let unit: Arc<dyn PointwiseColor> = self.gate.clone();
        Ok(Processing::Color(ColorOperation::new(vec![unit])))
    }
}

thread_local! {
    /// The gates this test has shut, so a wait can tell that its job is held at one. Each test runs
    /// on its own thread, so no test sees another's.
    static GATES: RefCell<Vec<Weak<Gate>>> = const { RefCell::new(Vec::new()) };
}

/// A test's hold on one preview job: the job waits at a shut gate on its first row until the test
/// opens it. Dropping the hold opens the gate, so a failing test never leaves a worker waiting.
///
/// A held job renders [`small`]: below the renderer's one-megapixel parallel threshold, so it runs
/// chunk by chunk on the preview worker itself and holds no Rayon thread, and taller than one
/// 16-row chunk, so a job superseded while it is held reads its token at its next chunk and answers
/// cancelled whenever its gate is opened.
struct Hold(Arc<Gate>);

impl Hold {
    fn shut() -> Self {
        let gate = Arc::new(Gate {
            state: Mutex::new(GateState {
                shut: true,
                waiting: 0,
            }),
            opened: Condvar::new(),
        });
        GATES.with(|gates| gates.borrow_mut().push(Arc::downgrade(&gate)));
        Self(gate)
    }

    /// Hold `job`: one gate layer ahead of its stack — inside a truncated job's prefix — rendered
    /// by the built-in modules and this gate's.
    fn hold(&self, job: &mut PreviewJob) {
        job.recipe.layers.insert(
            0,
            Layer {
                id: LayerId::new(),
                effect_id: HELD_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
                mask: None,
                artifacts: Vec::new(),
            },
        );
        job.layer_count = job.layer_count.map(|count| count + 1);
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(HeldModule {
                descriptor: ModuleDescriptor {
                    id: "test.held".into(),
                    title: "Held".into(),
                    effects: vec![EffectDescriptor {
                        id: HELD_EFFECT.into(),
                        format: EFFECT_FORMAT,
                        stage: EffectStage::Color,
                        order: 0,
                        artifacts: false,
                        single: false,
                        maskable: false,
                    }],
                    availability: Availability::Available,
                    ..ModuleDescriptor::default()
                },
                gate: self.0.clone(),
            }))
            .expect("a valid holding module");
        job.registry = Arc::new(registry);
    }

    fn open(&self) {
        self.0.lock().shut = false;
        self.0.opened.notify_all();
    }

    /// Wait until the held job is inside its render, so a newer request finds it running and
    /// unable to finish, rather than about to answer cancelled before its first row.
    fn reached(&self, editor: &Editor, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.0.holding() {
            assert!(
                editor.preview_queue.is_busy() && !editor.preview_queue.ready(),
                "{what} can never reach its gate: the job ended first: {}",
                editor.status
            );
            assert!(
                Instant::now() < deadline,
                "{what} never reached its gate: {}",
                editor.status
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.open();
    }
}

/// Deliver worker results with `poll` until `done` holds, failing at once when nothing more can
/// arrive: the queue has nothing running, waiting or ready, or its running job is held at a gate
/// this test has not opened and nothing is ready. The deadline is only a backstop for a render that
/// is still progressing: these tests assert what is shown, never how fast.
fn wait_for(
    editor: &mut Editor,
    what: &str,
    done: impl Fn(&Editor) -> bool,
    poll: impl Fn(&mut Editor),
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if done(editor) {
            return;
        }
        poll(editor);
        if done(editor) {
            return;
        }
        let queue = &editor.preview_queue;
        assert!(
            queue.is_busy(),
            "{what} can never happen: the preview queue has nothing running, waiting or ready: {}",
            editor.status
        );
        let held = GATES.with(|gates| {
            gates
                .borrow()
                .iter()
                .filter_map(Weak::upgrade)
                .any(|gate| gate.holding())
        });
        assert!(
            !held || queue.ready(),
            "{what} can never happen: the running preview job is held at a gate this test has not \
             opened: {}",
            editor.status
        );
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {}",
            editor.status
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Deliver worker results through `update` until `done` holds; see [`wait_for`].
fn poll_until(editor: &mut Editor, what: &str, done: impl Fn(&Editor) -> bool) {
    wait_for(editor, what, done, |editor| {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    });
}

/// A committed entry that straightens and crops the one before it.
fn cropped(asset: &AssetId, parent: &HistoryEntry, sequence: u64) -> HistoryEntry {
    let mut next = entry(asset, sequence, Some(&parent.id));
    next.label = "Crop 7°".into();
    next.action_id = "crop".into();
    next.snapshot = parent
        .snapshot
        .append(crop_layer(CropPayload {
            angle: 7.0,
            x: 0.2,
            y: 0.2,
            width: 0.6,
            height: 0.6,
        }))
        .expect("a valid stack");
    next
}

/// The refresh a commit of `current` brings back, whose preview job renders `source`.
fn committed(
    asset: &AssetId,
    current: &HistoryEntry,
    older: &[&HistoryEntry],
    source: SourceImage,
) -> super::tasks::Refresh {
    let mut page = vec![current.clone()];
    page.extend(older.iter().map(|entry| (*entry).clone()));
    let mut lineage = vec![current];
    lineage.extend(older.iter().copied());
    let mut refresh = refresh_for(asset, current, page, &lineage, false);
    refresh.job.source = PreviewSource::Jpeg(source);
    refresh
}

/// A small photograph the crop above covers at 7°.
fn small() -> SourceImage {
    SourceImage {
        width: 64,
        height: 48,
        rgba: [10, 20, 30, 255].repeat(64 * 48).into(),
        fingerprint: "f".into(),
        orientation: 1,
    }
}

/// A source the renderer refuses before allocating anything: its declared frame is 2.2 GiB.
fn over_the_frame_limit() -> SourceImage {
    SourceImage {
        width: 30_000,
        height: 20_000,
        rgba: vec![0, 0, 0, 255].into(),
        fingerprint: "f".into(),
        orientation: 1,
    }
}

fn opened_and_shown() -> (Editor, PathBuf, AssetId, HistoryEntry) {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 4);
    poll_until(&mut editor, "the opened entry's frame", |editor| {
        editor.presented_entry.as_ref() == Some(&entry_id)
    });
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    (editor, catalog, asset, current)
}

fn events<'a>(records: &'a [Value], name: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["event"] == json!(name))
        .map(|record| &record["detail"])
        .collect()
}

/// A crop that committed but whose render failed: the uncropped picture is withdrawn rather than
/// left under history's "Crop 7°", the canvas and the histogram say why, a zoom hands nothing over
/// in its place, and the next frame that renders puts a picture back.
#[test]
fn a_commit_whose_render_fails_withdraws_the_earlier_picture_instead_of_presenting_it() {
    let (mut editor, catalog, asset, original) = opened_and_shown();
    let log = attach_log(&mut editor);
    let version = editor.presenter.photo_version();
    assert!(
        editor.presenter.photo().is_some(),
        "the opened frame is on screen"
    );

    let crop = cropped(&asset, &original, 5);
    let refresh = committed(&asset, &crop, &[&original], over_the_frame_limit());
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert_eq!(
        editor.state.as_ref().expect("open").current_entry.id,
        crop.id,
        "history names the crop"
    );
    poll_until(&mut editor, "the crop's failure", |editor| {
        editor.render_error.is_some()
    });

    assert_eq!(
        editor.render_error.as_ref().map(|(kind, _)| *kind),
        Some(ErrorKind::ResourceLimit)
    );
    assert!(
        editor.presenter.photo().is_none(),
        "the uncropped picture is still on the surface under the crop's entry"
    );
    assert_eq!(editor.presented_entry, None);
    assert_eq!(
        editor.stack_summary()["displayed"],
        Value::Null,
        "the evidence names a displayed entry although nothing is shown"
    );
    assert!(editor.raster.is_none() && editor.proxy_frame.is_none() && editor.analysis.is_none());
    assert!(
        editor.status.starts_with("resource-limit: "),
        "{}",
        editor.status
    );
    let canvas = &editor.workspace.canvas;
    match &canvas.photo {
        PhotoView::Empty(line) => assert!(
            line.starts_with("Preview unavailable: ") && line.contains("512 MiB"),
            "{line}"
        ),
        other => panic!("the canvas still draws a photograph: {other:?}"),
    }
    assert!(
        canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Rendering limit"),
        "{:?}",
        canvas.notices
    );
    assert_eq!(
        editor.workspace.histogram.status,
        HistogramStatus::Unavailable
    );

    // A zoom either way hands nothing retained over in the picture's place and asks for no render.
    let generation = editor.preview_generation;
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    editor.session.preview.view.zoom = Zoom::Fit;
    let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert!(
        editor.presenter.photo().is_none(),
        "a zoom put a stale picture back"
    );
    assert_eq!(
        editor.presenter.photo_version(),
        version,
        "nothing was handed over"
    );
    assert_eq!(
        editor.preview_generation, generation,
        "nothing was asked for"
    );
    assert!(editor.render_error.is_some(), "the failure is still shown");

    // The next state that renders is shown, and the failure with it is over.
    let mut next = entry(&asset, 6, Some(&crop.id));
    next.snapshot = crop.snapshot.clone();
    let refresh = committed(&asset, &next, &[&crop, &original], small());
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    poll_until(&mut editor, "the next frame", |editor| {
        editor.presenter.photo().is_some()
    });
    assert_eq!(editor.presented_entry.as_ref(), Some(&next.id));
    assert_eq!(editor.render_error, None);
    assert_eq!(editor.workspace.canvas.photo, PhotoView::Plain);

    let records = logged(&mut editor, &log);
    let failed = events(&records, "preview_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["entry_id"], json!(crop.id));
    assert_eq!(failed[0]["error_code"], json!("resource-limit"));
    let withdrawn = events(&records, "preview_withdrawn");
    assert_eq!(withdrawn.len(), 1, "{withdrawn:?}");
    assert_eq!(withdrawn[0]["target_entry"], json!(crop.id));
    assert_eq!(withdrawn[0]["withdrawn_entry"], json!(original.id));
    assert_eq!(withdrawn[0]["error_code"], json!("resource-limit"));
    let displayed: Vec<&Value> = events(&records, "preview_displayed");
    assert!(
        displayed
            .iter()
            .all(|detail| detail["entry_id"] != json!(crop.id)),
        "a frame was presented as the crop's: {displayed:?}"
    );
    finish(editor, catalog);
}

/// The full-resolution phase of the state on screen failed after its display proxy was shown: the
/// proxy is that state's own picture, so it stays, and the failure is still named.
#[test]
fn a_failed_exact_phase_keeps_the_proxy_of_the_same_state() {
    let (mut editor, catalog, _, current) = opened_and_shown();
    let presented = editor.presented_generation;
    let error = Error::resource_limit("linear output exceeds 512 MiB");
    editor.preview_failed(presented, false, &current.id, None, &error);
    editor.rederive();
    assert!(
        editor.presenter.photo().is_some(),
        "the target's own proxy was withdrawn"
    );
    assert_eq!(editor.presented_entry.as_ref(), Some(&current.id));
    assert_eq!(editor.workspace.canvas.photo, PhotoView::Plain);
    assert!(
        editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Rendering limit")
    );

    // A drafted revision of the same entry is another picture: its failure withdraws the frame.
    editor.preview_failed(presented + 1, false, &current.id, Some(3), &error);
    assert!(
        editor.presenter.photo().is_none(),
        "a frame of another revision stayed"
    );
    finish(editor, catalog);
}

/// A zoom while a newer entry is rendering hands over the picture on screen under that picture's
/// own entry, and leaves the entry the desktop is waiting for as the one it asked for.
#[test]
fn a_zoom_hands_over_the_retained_picture_under_its_own_entry() {
    let (mut editor, catalog, asset, current) = opened_and_shown();
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    let raster = |code: u8| {
        Arc::new(lightwell_core::Raster {
            width: 2,
            height: 2,
            rgba: vec![code; 16].into(),
            source_fingerprint: "f".into(),
            snapshot_id: current.snapshot.id.clone(),
        })
    };
    let generation = editor.presented_generation;
    editor.presented_proxy = true;
    editor.proxy_frame = Some(ProxyFrame {
        generation,
        raster: raster(1),
        dimensions: (1200, 900),
        built: false,
        approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        render_ms: 5.0,
    });
    editor.raster = Some((generation, raster(2)));
    // The next entry is committed and asked for; its frame has not arrived.
    let next: EntryId = cropped(&asset, &current, 5).id;
    editor.display_entry = Some(next.clone());
    let log = attach_log(&mut editor);

    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    assert!(
        !editor.presented_proxy,
        "the retained exact raster is on screen"
    );
    assert_eq!(editor.presented_entry.as_ref(), Some(&current.id));
    assert_eq!(
        editor.display_entry.as_ref(),
        Some(&next),
        "the hand-over moved the requested entry back"
    );
    let records = logged(&mut editor, &log);
    let displayed = events(&records, "preview_displayed");
    assert_eq!(displayed.len(), 1, "{displayed:?}");
    assert_eq!(displayed[0]["entry_id"], json!(current.id));
    assert_eq!(displayed[0]["snapshot_id"], json!(current.snapshot.id));
    assert_eq!(displayed[0]["reason"], json!("zoom"));
    finish(editor, catalog);
}

/// The crop layer's input stage could not be rendered: a start that was waiting ends in the pointer
/// mode with the reason in the status bar, while the photograph — the current state — stays; a
/// reapply keeps the draft it was rebasing.
#[test]
fn a_draft_whose_input_stage_fails_ends_explicitly_and_keeps_the_photograph() {
    let (mut editor, catalog, _, _) = opened_and_shown();
    let error = Error::resource_limit("linear output exceeds 512 MiB");
    let starting = PendingStage {
        layer: None,
        layer_index: 0,
        ahead: lightwell_core::Orientation::NEUTRAL,
        payload: None,
        base_revision: 4,
        reapply: false,
        queued: Vec::new(),
    };
    hold_crop(&mut editor, None, Some(starting));
    editor.draft_generation = Some(99);
    editor.draft_preview_failed(&error);
    assert!(editor.crop_pending().is_none() && editor.draft_generation.is_none());
    assert!(editor.crop().is_none());
    assert_eq!(editor.mode_sync.as_deref(), Some(POINTER_MODE));
    assert!(
        editor
            .status
            .starts_with("The crop's input stage could not be rendered: resource-limit"),
        "{}",
        editor.status
    );
    assert_eq!(
        editor.render_error, None,
        "the photograph's own state did not fail"
    );
    assert!(editor.presenter.photo().is_some());

    let stage = CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    };
    let rebasing = PendingStage {
        layer: None,
        layer_index: 0,
        ahead: lightwell_core::Orientation::NEUTRAL,
        payload: None,
        base_revision: 5,
        reapply: true,
        queued: Vec::new(),
    };
    hold_crop(
        &mut editor,
        Some(crate::crop_draft::CropDraft::neutral(stage, 0)),
        Some(rebasing),
    );
    editor.draft_generation = Some(100);
    editor.draft_preview_failed(&error);
    assert!(
        editor.crop().is_some(),
        "a failed reapply discarded the draft"
    );
    assert!(editor.crop_pending().is_none() && editor.draft_generation.is_none());
    finish(editor, catalog);
}

/// A scripted step waiting for the newest preview's pixels ends on that preview's failure, so the
/// evidence captures the failure instead of waiting out its deadline; an older job's failure does
/// not end it.
#[test]
fn a_scripted_step_waiting_for_a_preview_ends_on_its_failure() {
    let steps = json!([{"wait": {"ms": 1}}]).to_string();
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(&steps);
    let error = Error::resource_limit("linear output exceeds 512 MiB");
    let entry = EntryId::new();
    editor.preview_generation = 9;
    if let Some(evidence) = editor.evidence.as_mut() {
        evidence.awaiting = Some(Settle::Preview);
        evidence.capture_pending = false;
    }
    editor.preview_failed(8, false, &entry, None, &error);
    let evidence = crate::app::testing::evidence(&editor);
    assert_eq!(
        evidence.awaiting,
        Some(Settle::Preview),
        "an older job's failure ended the step"
    );
    editor.preview_failed(9, false, &entry, None, &error);
    let evidence = crate::app::testing::evidence(&editor);
    assert!(evidence.awaiting.is_none() && evidence.capture_pending);
    finish(editor, catalog);
}

/// A layer ahead of the crop, so the crop's input stage is a real prefix of the stack with a colour
/// pass of its own.
fn basic() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 0.5, "contrast": 20.0}),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The job `crop_preview_task` hands back for the starting draft: the current stack truncated to
/// the layers before the crop, over `source`.
fn draft_job(editor: &Editor, source: SourceImage) -> PreviewJob {
    let state = editor.state.as_ref().expect("an open asset");
    let current = &state.current_entry;
    let pending = editor.crop_pending().expect("a starting draft");
    let mut job = refresh_for(&state.asset.id, current, Vec::new(), &[current], false).job;
    job.source = PreviewSource::Jpeg(source);
    job.layer_count = Some(pending.layer_index);
    job
}

/// Another client commits: the event sync reads the state back and requests the new entry's frame,
/// held at `hold`'s gate when there is one.
fn committed_elsewhere(
    editor: &mut Editor,
    asset: &AssetId,
    sequence: u64,
    hold: Option<&Hold>,
) -> HistoryEntry {
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    let mut next = entry(asset, sequence, Some(&current.id));
    next.snapshot = current.snapshot.clone();
    let mut refresh = committed(asset, &next, &[&current], small());
    if let Some(hold) = hold {
        hold.hold(&mut refresh.job);
    }
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
        refresh,
    )))));
    next
}

/// [`poll_until`] through `dispatch`, so the mode a draft's end asks the session for is still
/// there to read afterwards rather than folded into a task this test never runs.
fn dispatch_polls_until(editor: &mut Editor, what: &str, done: impl Fn(&Editor) -> bool) {
    wait_for(editor, what, done, |editor| {
        let _ = editor.dispatch(Message::Preview(PreviewMessage::Poll));
    });
}

/// A starting draft's input stage is still rendering when another client's commit requests the
/// new entry's frame, which stops the draft's job: the job's cancelled outcome is recorded under
/// the draft's own generation, and the draft ends explicitly, back in the pointer mode with the
/// reason in the status bar, instead of waiting for pixels that will never come. The new entry's
/// frame is then shown as usual, and its own status replaces the reason.
///
/// Both jobs are held: the draft's until the commit has superseded it, so it cannot finish first
/// and open the draft; the new entry's until the draft has ended, so its frame cannot follow the
/// draft's end in the same poll and replace the reason before it is read.
#[test]
fn a_starting_draft_whose_input_stage_a_newer_request_cancels_ends_explicitly() {
    let (mut editor, catalog, asset, _) = opened(vec![basic()], 4);
    poll_until(&mut editor, "the opened frame", |editor| {
        editor.presented_generation > 0 && !editor.preview_queue.is_busy()
    });
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let (stage, frame) = (Hold::shut(), Hold::shut());
    let mut job = draft_job(&editor, small());
    stage.hold(&mut job);
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    let draft = editor
        .draft_generation
        .expect("the draft's job was requested");
    assert_eq!(editor.status, "Rendering the crop's input stage…");
    assert_eq!(
        editor.preview_queue.pending_generation(),
        None,
        "the draft's job is running"
    );
    stage.reached(&editor, "the draft's input stage");

    let next = committed_elsewhere(&mut editor, &asset, 5, Some(&frame));
    let newer = editor.preview_generation;
    assert!(newer > draft);
    assert_eq!(
        editor.preview_queue.pending_generation(),
        Some(newer),
        "the new entry's job waits behind the draft's"
    );
    stage.open();
    dispatch_polls_until(&mut editor, "the draft's end", |editor| {
        editor.crop_pending().is_none()
    });
    assert_eq!(editor.draft_generation, None);
    assert!(editor.crop().is_none() && editor.presenter.stage().is_none());
    assert_eq!(
        editor.mode_sync.as_deref(),
        Some(POINTER_MODE),
        "the session was not asked to leave the crop mode"
    );
    assert_eq!(
        editor.status,
        "The crop's input stage was superseded by a newer preview: start the crop again"
    );

    frame.open();
    dispatch_polls_until(&mut editor, "the new entry's frame", |editor| {
        editor.presented_generation == newer && !editor.preview_queue.is_busy()
    });
    assert!(
        editor.presenter.stage().is_none(),
        "nothing of the draft was shown"
    );
    assert!(editor.crop_pending().is_none() && editor.presenter.stage().is_none());
    assert_eq!(editor.presented_entry.as_ref(), Some(&next.id));
    assert_eq!(editor.render_error, None);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "preview_exact_cancelled"),
        vec![&json!({"generation": draft, "draft": true})],
        "the cancelled phase is recorded as the draft's"
    );
    assert_eq!(
        events(&records, "crop_draft_failed"),
        vec![&json!({
            "reapply": false,
            "error_code": "cancelled",
            "detail": "superseded by a newer preview",
            "generation": draft,
        })]
    );
    finish(editor, catalog);
}

/// A reapply's input stage waits in the pending slot behind the frame of one commit from another
/// client when a second commit's frame replaces it there, so it never starts: the reapply ends as
/// it is replaced, and keeps the conflicted draft it was rebasing for another reapply. The status
/// bar goes straight on to the second commit's frame, which is what the photograph is waiting for;
/// the draft's own notice still says it changed elsewhere.
///
/// The first commit's job is held inside its render until the second commit has replaced the
/// reapply's, so the reapply's job is still waiting behind it however slowly this test runs.
#[test]
fn a_reapply_whose_input_stage_a_newer_request_replaces_keeps_the_conflicted_draft() {
    let (mut editor, catalog, asset, _) = opened(vec![basic()], 4);
    poll_until(&mut editor, "the opened frame", |editor| {
        editor.presented_generation > 0 && !editor.preview_queue.is_busy()
    });
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(
        &mut editor,
        CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        },
    );
    // The first commit makes the draft conflicted; its frame is held inside its render.
    let first = Hold::shut();
    committed_elsewhere(&mut editor, &asset, 5, Some(&first));
    first.reached(&editor, "the first commit's frame");
    assert!(core_draft(&editor).expect("the draft is kept").conflicted);
    let _ = editor.update(Message::Crop(CropMessage::Reapply));
    assert!(editor.crop_pending().expect("a pending rebase").reapply);
    let log = attach_log(&mut editor);
    let job = draft_job(&editor, small());
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    let reapply = editor
        .draft_generation
        .expect("the reapply's job was requested");
    assert_eq!(
        editor.preview_queue.pending_generation(),
        Some(reapply),
        "the reapply's job waits behind the first commit's frame"
    );

    let next = committed_elsewhere(&mut editor, &asset, 6, None);
    assert!(
        editor.crop_pending().is_none() && editor.draft_generation.is_none(),
        "the replaced reapply is still waiting"
    );
    assert!(editor.crop().is_some(), "the reapply kept its draft");
    let draft = core_draft(&editor).expect("the reapply kept its draft");
    assert!(draft.conflicted, "the kept draft is still conflicted");
    assert_eq!(draft.base_revision, 4);

    first.open();
    poll_until(&mut editor, "the second commit's frame", |editor| {
        editor.presented_entry.as_ref() == Some(&next.id) && !editor.preview_queue.is_busy()
    });
    assert!(
        editor.presenter.stage().is_none(),
        "the replaced job delivered a stage"
    );
    assert!(core_draft(&editor).expect("the draft is kept").conflicted);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "crop_draft_failed"),
        vec![&json!({
            "reapply": true,
            "error_code": "cancelled",
            "detail": "superseded by a newer preview",
            "generation": reapply,
        })]
    );
    assert!(
        events(&records, "preview_exact_cancelled")
            .iter()
            .all(|detail| detail["generation"] != json!(reapply)),
        "a job that never started was cancelled: {records:?}"
    );
    finish(editor, catalog);
}

/// The stack changed while the owner planned the draft's job: the job it answers truncates an entry
/// that is not the current one the desktop holds, so its input stage is stale and the draft ends
/// explicitly as a cancelled one does, without a render — decided from the answer itself, with no
/// currency request of its own.
#[test]
fn a_draft_whose_job_the_owner_finds_superseded_ends_explicitly() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let mut job = draft_job(&editor, small());
    job.entry = entry(&asset, 5, Some(&job.entry.id));
    let _ = editor.dispatch(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    assert_eq!(
        editor.draft_generation, None,
        "the stale stage was not requested"
    );
    assert!(editor.crop_pending().is_none() && editor.crop().is_none());
    assert_eq!(editor.mode_sync.as_deref(), Some(POINTER_MODE));
    assert!(
        editor.status.ends_with("start the crop again"),
        "{}",
        editor.status
    );
    let records = logged(&mut editor, &log);
    let failed = events(&records, "crop_draft_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["generation"], Value::Null);
    finish(editor, catalog);
}

/// A draft's input stage opens the draft in the very update that takes it up: it becomes the
/// presenter's stage from the render's own buffer, with no upload and no message to wait for, and
/// the photograph stays held under it, its version untouched, for when the draft ends.
#[test]
fn a_draft_opens_on_its_input_stage_in_the_update_that_takes_it_up() {
    let (mut editor, catalog, _, _) = opened(vec![basic()], 4);
    poll_until(&mut editor, "the opened frame", |editor| {
        editor.presented_generation > 0 && !editor.preview_queue.is_busy()
    });
    let photo = editor.presenter.photo_version();
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let job = draft_job(&editor, small());
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    let draft = editor.draft_generation.expect("the draft's job");
    // Nothing is taken up until the stage is ready, so one `Poll` takes it up.
    wait_for(
        &mut editor,
        "the draft's input stage",
        |editor| editor.preview_queue.ready(),
        |_| {},
    );
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let open = editor.crop().expect("the draft opened in the same update");
    assert_eq!((open.stage.width, open.stage.height), (64, 48));
    assert_eq!(
        editor.presenter.stage().map(lightwell_ui::Frame::size),
        Some((64, 48))
    );
    assert!(editor.drafting());
    assert_eq!(
        editor.presenter.photo_version(),
        photo,
        "the stage is not a photograph"
    );
    assert!(editor.presenter.photo().is_some());
    assert_ne!(editor.presented_generation, draft);
    let _ = editor.update(Message::Crop(CropMessage::Cancel));
    assert!(editor.presenter.stage().is_none(), "the stage was kept");
    finish(editor, catalog);
}
