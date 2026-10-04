//! The status bar's notice of a gesture drawn on the CPU path while the GPU preview is on
//! (`docs/design/gpu-preview.md`, "Labels and overlays during motion"), end to end against a real
//! owner: what `workspace.status` and the snapshot say of each class of reason, held against the
//! reason the evidence records in the same frame; reasons that pass, and the preference turned off,
//! say nothing; `compiling` waits for half a second and is decided by a tick, never by the clock;
//! and the notice clears at the next GPU tick and when the gesture's settle ends.
use super::{
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, commit, deliver_until, surface_ready, zoomed},
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
    testing::{finish, let_go, real_photo, run_commit, slide},
    *,
};
use luxforge_ui::photo_surface::GpuFallback as SurfaceFallback;
use std::time::Duration;

const ACTION: &str = "set-basic";
const PRESENCE: &str = "set-presence";
const FIELD: &str = "exposure";

const MEMORY: (&str, &str) = (
    "GPU memory full",
    "This many layers at this zoom need more than the GPU preview holds, so the preview is drawn \
     on the CPU, which is slower. Fewer masked Presence or Detail layers, or Fit, draw on the GPU.",
);
const ZOOM: (&str, &str) = (
    "GPU preview at Fit and 100%+",
    "The GPU preview draws at Fit and at 100% or more; at this zoom the preview is drawn on the \
     CPU.",
);
const STACK: (&str, &str) = (
    "Not on the GPU: Perspective",
    "The GPU preview cannot draw this layer yet, so this drag is drawn on the CPU.",
);
const DEHAZE: (&str, &str) = (
    "Dehaze on the CPU",
    "Dehaze needs the haze estimate a settled frame stores for this view; until then this drag is \
     drawn on the CPU.",
);
const COMPILING: (&str, &str) = (
    "Preparing GPU preview",
    "The GPU preview is compiling its programs for this stack; drags are drawn on the CPU until \
     it is ready.",
);

/// What the notice says, read three ways that must agree — the model the bar draws, the snapshot's
/// status bar and its surface's evidence — and held against `reason`, the reason the same snapshot
/// records as `plan_fallback`. `None` for the reason saying nothing.
fn assert_says(editor: &Editor, reason: &str, said: Option<(&str, &str)>) {
    let snapshot = editor.snapshot();
    let gpu = &snapshot["surface"]["gpu"];
    assert_eq!(
        gpu["plan_fallback"],
        json!({ "reason": reason }),
        "the reason recorded"
    );
    let expected = said.map_or(
        Value::Null,
        |(phrase, tooltip)| json!({"phrase": phrase, "tooltip": tooltip}),
    );
    assert_eq!(gpu["fallback_notice"], expected, "{reason}: the evidence");
    assert_eq!(
        snapshot["status_bar"]["fallback"], expected,
        "{reason}: beside the render slot"
    );
    assert_eq!(
        editor
            .workspace
            .status
            .fallback
            .as_ref()
            .map(|notice| (notice.phrase.as_str(), notice.tooltip.as_str())),
        said,
        "{reason}: the model the bar draws"
    );
}

/// The notice reads the open drag's latest tick, so each case's ticks are asserted as they come.
fn latest_reason(editor: &Editor) -> Value {
    editor.gpu.summary()["drag"]["reason"].clone()
}

/// A slot over a test budget is drawn on the CPU naming `budget-exceeded`, which the notice says
/// as the GPU memory being full, for every tick of the gesture; the next tick drawn on the GPU
/// clears it.
#[test]
fn gpu_preview_the_notice_says_a_slot_over_the_budget_is_memory() {
    let catalog = catalog("notice-memory");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    zoomed(&mut editor);
    editor.gpu.budget = Some(1);
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_eq!(latest_reason(&editor), json!("budget-exceeded"));
        assert_says(&editor, "budget-exceeded", Some(MEMORY));
    }
    // Within the budget the boundary is asked for, which passes: nothing is said while it comes.
    editor.gpu.budget = Some(u64::MAX);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_says(&editor, "boundary-pending", None);
    deliver_until(&mut editor, "the region's boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    // The next tick drawn on the GPU says nothing of a gesture that is no longer slow.
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert_eq!(editor.gpu.ticks().0, 1, "a tick on the GPU");
    assert_eq!(editor.gpu_plan_fallback(), None);
    assert_eq!(editor.workspace.status.fallback, None);
    assert_eq!(editor.snapshot()["status_bar"]["fallback"], Value::Null);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A percentage zoom below 100% plans no GPU preview and names `not-fit`: the notice says the
/// zoom, from the first tick, and keeps saying it through the release until the frame that replaces
/// the drafted one is presented, which is when the drag, and the gesture's settle, end.
#[test]
fn gpu_preview_the_notice_says_a_zoom_below_100_and_clears_when_the_settle_ends() {
    let catalog = catalog("notice-zoom");
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert_eq!(editor.gpu_plan_fallback(), None, "no gesture, no notice");
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 50.0 };
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_says(&editor, "not-fit", Some(ZOOM));
    }
    // The release commits on the CPU; the drag stays until the committed frame is presented, and
    // the notice stays with it.
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    assert!(editor.gpu.has_drag(), "settling");
    assert_says(&editor, "not-fit", Some(ZOOM));
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag()
    });
    assert_eq!(editor.gpu_plan_fallback(), None);
    assert_eq!(
        editor.workspace.status.fallback, None,
        "the settle ended with the drag"
    );
    assert_eq!(editor.snapshot()["status_bar"]["fallback"], Value::Null);
    assert_eq!(
        editor.snapshot()["surface"]["gpu"]["fallback_notice"],
        Value::Null
    );
    finish(editor, catalog);
}

/// A first tick that waits for its boundary names `boundary-pending`, which passes within a tick
/// or two: nothing is said, though the evidence records the reason.
#[test]
fn gpu_preview_the_notice_says_nothing_while_a_boundary_comes() {
    let catalog = catalog("notice-pending");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    assert_eq!(editor.gpu.ticks(), (0, 1, 1), "a CPU tick asking");
    assert_says(&editor, "boundary-pending", None);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert_says(&editor, "boundary-pending", None);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    // The boundary held but not yet evaluated by the surface is passing too.
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_says(&editor, "surface-pending", None);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// With the preference off the desktop hands no plan and the evidence names `preference-off`: the
/// person chose the CPU path, so nothing is said, mid-gesture or between gestures.
#[test]
fn gpu_preview_the_notice_says_nothing_with_the_preference_off() {
    let catalog = catalog("notice-off");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let mut session = editor.session.clone();
    session.revision += 1;
    session.workspace.gpu_preview = false;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    assert_says(&editor, "preference-off", None);
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_says(&editor, "preference-off", None);
    }
    // At a zoom that would say its own reason, the preference still says nothing.
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 50.0 };
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_says(&editor, "preference-off", None);
    // Turned back on, the next tick says the zoom again.
    let mut session = editor.session.clone();
    session.revision += 1;
    session.workspace.gpu_preview = true;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert_says(&editor, "not-fit", Some(ZOOM));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A layer the plan cannot hold names itself: a Perspective drag changes a geometry layer, which
/// no plan from its input can hold, so the reason is `boundary-stage` and the notice names the
/// layer by the label the recipe list gives it.
#[test]
fn gpu_preview_the_notice_names_the_layer_the_stack_cannot_draw() {
    let catalog = catalog("notice-stack");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    for value in [20.0, 40.0] {
        let _ = slide(&mut editor, "set-perspective", "horizontal", value);
        assert_eq!(latest_reason(&editor), json!("boundary-stage"));
        assert_says(&editor, "boundary-stage", Some(STACK));
    }
    // Another gesture over the same stack names its own reason, and the layer goes with the last.
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 50.0 };
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    assert_says(&editor, "not-fit", Some(ZOOM));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// At 100% a Basic drag under a committed Dehaze would take the light from the visible region
/// alone, which the exact frame reads whole: the plan answers `region-estimate`, and the notice
/// says Dehaze is on the CPU, naming no layer.
#[test]
fn gpu_preview_the_notice_says_dehaze_when_its_estimate_keeps_a_drag_on_the_cpu() {
    let catalog = catalog("notice-dehaze");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    let at_100 = |editor: &mut Editor| {
        editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
    };
    at_100(&mut editor);
    commit(&mut editor, PRESENCE, "dehaze", 40.0);
    at_100(&mut editor);
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_eq!(latest_reason(&editor), json!("region-estimate"));
        assert_says(&editor, "region-estimate", Some(DEHAZE));
    }
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// `compiling` says its phrase only from a tick at least half a second after the drag first saw it,
/// and the tick decides it: a drag that holds still keeps what it last said, whatever time passes
/// and whatever message arrives. A tick on the GPU clears it, and a later compile starts its half
/// second again.
#[test]
fn gpu_preview_the_notice_says_compiling_once_it_has_lasted_half_a_second() {
    let catalog = catalog("notice-compiling");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    let version = editor.gpu.held_version().expect("a held boundary");
    let compiling = |editor: &mut Editor| {
        editor.gpu.surface = Some(SurfaceReport {
            ready_boundary: Some(version),
            fallback: Some(SurfaceFallback::Compiling),
            drawn: None,
            evaluated: None,
        });
    };
    // The first tick that finds the sequence compiling is its first moment: nothing is said.
    compiling(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert_says(&editor, "compiling", None);
    // Time passing says nothing by itself: no tick, no timer, and an unrelated message change
    // nothing.
    editor.gpu.backdate_compiling(Duration::from_millis(600));
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_says(&editor, "compiling", None);
    // A tick after half a second says it, and the figure the evidence records stays with it.
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_says(&editor, "compiling", Some(COMPILING));
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_says(&editor, "compiling", Some(COMPILING));
    // Ready: the tick is drawn on the GPU and the notice goes.
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert_eq!(editor.gpu_plan_fallback(), None);
    assert_eq!(editor.workspace.status.fallback, None);
    // Compiling again begins a new run: the half second starts over.
    compiling(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.5);
    assert_says(&editor, "compiling", None);
    // A tick that finds another reason ends the run too.
    editor.gpu.backdate_compiling(Duration::from_millis(600));
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: Some(SurfaceFallback::BoundaryUploading {
            uploaded: 1,
            bytes: 2,
        }),
        drawn: None,
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    assert_says(&editor, "boundary-uploading", None);
    compiling(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.7);
    assert_says(&editor, "compiling", None);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}
