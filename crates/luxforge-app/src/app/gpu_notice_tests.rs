//! The status bar's notice of a gesture drawn on the CPU path while the GPU preview is on, and of
//! the reference renderer drawing every frame (`docs/design/gpu-preview.md`, "Labels and overlays
//! during motion"), end to end against a real owner: what `workspace.status` and the snapshot say of
//! each class of reason, held against the reason the evidence records in the same frame; reasons
//! that pass, and the preference turned off, say nothing; `compiling` waits for half a second and
//! is decided by a tick, never by the clock; the notice clears at the next GPU tick and when the
//! gesture's settle ends; and the reference renderer's notice is the session's, said at rest.
use super::{
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, deliver_until, surface_ready, zoomed, zoomed_out},
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
    testing::{finish, let_go, real_photo, run_commit, slide},
    *,
};
use luxforge_ui::photo_surface::GpuFallback as SurfaceFallback;
use std::time::Duration;

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

const MEMORY: (&str, &str) = (
    "GPU memory full",
    "This many layers at this zoom need more than the GPU preview holds, so the preview is drawn \
     on the CPU, which is slower. Fewer masked Presence or Detail layers, or Fit, draw on the GPU.",
);
const STACK: (&str, &str) = (
    "Not on the GPU: Pixel",
    "The GPU preview cannot draw this layer yet, so this drag is drawn on the CPU.",
);
const REFERENCE: (&str, &str) = (
    "Reference renderer",
    "This graphics device cannot run the GPU renderer, or this launch turned it off, so every \
     frame is drawn by the reference renderer on the CPU, which is slower.",
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
    // Within the budget the boundary is derived, and the plan waits for the surface, which
    // passes: nothing is said while it does.
    editor.gpu.budget = Some(u64::MAX);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert!(editor.gpu.holds_boundary());
    assert_says(&editor, "surface-pending", None);
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

/// A reason that lasts is said from the first tick that names it, and kept through the release
/// until the frame that replaces the drafted one is presented, which is when the drag, and the
/// gesture's settle, end: here a Fit drag whose slot, at the exact stage the photograph is drawn
/// at, the surface finds over the budget.
#[test]
fn gpu_preview_the_notice_says_a_lasting_reason_and_clears_when_the_settle_ends() {
    let catalog = catalog("notice-settle");
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert_eq!(editor.gpu_plan_fallback(), None, "no gesture, no notice");
    // The drag starts from the boundary the photograph's job derived, and the surface finds its
    // slot over the budget.
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::BudgetExceeded {
            requested: 2,
            in_use: 0,
            budget: 1,
        }),
        drawn: None,
        evaluated: None,
    });
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_says(&editor, "budget-exceeded", Some(MEMORY));
    }
    // The release commits on the CPU; the drag stays until the committed frame is presented, and
    // the notice stays with it.
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    assert!(editor.gpu.has_drag(), "settling");
    assert_says(&editor, "budget-exceeded", Some(MEMORY));
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

/// A first tick whose plan the surface has not evaluated yet names `surface-pending`, and one while
/// the surface still uploads the source names `source-uploading`; both pass within a frame or two,
/// so nothing is said, though the evidence records the reason.
#[test]
fn gpu_preview_the_notice_says_nothing_while_the_source_or_the_plan_comes() {
    let catalog = catalog("notice-pending");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    assert_eq!(editor.gpu.ticks(), (0, 1, 0), "a CPU tick");
    assert_says(&editor, "surface-pending", None);
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::SourceUploading {
            uploaded: 1,
            bytes: 2,
        }),
        drawn: None,
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert_says(&editor, "source-uploading", None);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// Below 100% a drag is drawn on the GPU from the displayed-size proxy as at Fit, so the zoom has
/// no reason of its own: the first tick's `surface-pending` passes, the ticks after it are drawn on
/// the GPU, and nothing is said through the release until the settle ends.
#[test]
fn gpu_preview_the_notice_says_nothing_of_a_zoom_below_100() {
    for value in [50.0, 33.0] {
        let catalog = catalog(&format!("notice-below-{value}"));
        let (mut editor, _, _) = real_photo(&catalog);
        editor.gpu.surface = Some(SurfaceReport::default());
        zoomed_out(&mut editor, value);
        let _ = slide(&mut editor, ACTION, FIELD, 0.1);
        assert_says(&editor, "surface-pending", None);
        surface_ready(&mut editor);
        for tick in [0.2, 0.3] {
            let _ = slide(&mut editor, ACTION, FIELD, tick);
            assert_eq!(latest_reason(&editor), Value::Null, "{value}%: on the GPU");
            assert_eq!(editor.gpu_plan_fallback(), None, "{value}%");
            assert_eq!(editor.workspace.status.fallback, None, "{value}%");
            let snapshot = editor.snapshot();
            assert_eq!(snapshot["status_bar"]["fallback"], Value::Null, "{value}%");
            assert_eq!(
                snapshot["surface"]["gpu"]["fallback_notice"],
                Value::Null,
                "{value}%"
            );
        }
        let _ = let_go(&mut editor, ACTION, FIELD);
        assert!(run_commit(&mut editor));
        assert_eq!(editor.workspace.status.fallback, None, "{value}%: settling");
        deliver_until(&mut editor, "the committed frame", |editor| {
            !editor.gpu.has_drag()
        });
        assert_eq!(editor.workspace.status.fallback, None, "{value}%");
        finish(editor, catalog);
    }
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
    // Where the drag would say its own reason, a slot over the budget, the preference still says
    // nothing.
    editor.gpu.budget = Some(1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_says(&editor, "preference-off", None);
    // Turned back on, the next tick says the budget.
    let mut session = editor.session.clone();
    session.revision += 1;
    session.workspace.gpu_preview = true;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert_says(&editor, "budget-exceeded", Some(MEMORY));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A Perspective drag on a photograph whose stack holds no content layer is planned from the
/// source, its geometry the plan's tail, where it once named `boundary-stage`: every tick derives
/// the boundary its output reads from the source and hands the surface its plan, saying nothing
/// while the surface evaluates it.
#[test]
fn gpu_preview_a_geometry_drag_on_a_fresh_photo_is_planned_for_the_gpu() {
    let catalog = catalog("notice-geometry");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    for value in [20.0, 40.0] {
        let _ = slide(&mut editor, "set-perspective", "horizontal", value);
        assert!(
            editor.gpu.holds_boundary(),
            "{value}: the source's boundary, derived"
        );
        let (plan, _) = editor.gpu.planned().expect("a plan");
        assert!(plan.content.is_empty(), "{value}: no content layer");
        assert!(
            plan.geometry.projective().is_some(),
            "{value}: the perspective is the tail"
        );
        assert!(
            editor.gpu.surface_plan().is_some(),
            "{value}: handed to the surface"
        );
        assert_says(&editor, "surface-pending", None);
    }
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A layer the plan cannot hold names itself: over a developer's pixel replacement, which addresses
/// content pixels no program replaces, a Basic drag's reason is `pixel-stage`, and the notice names
/// the layer by the label the recipe list gives it.
#[test]
fn gpu_preview_the_notice_names_the_layer_the_stack_cannot_draw() {
    let catalog = catalog("notice-stack");
    let _ = std::fs::remove_file(&catalog);
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &catalog,
        std::sync::Arc::new(luxforge_core::ModuleRegistry::developer()),
        luxforge_core::HostConfig::unconfigured(),
    )
    .unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let (mut editor, asset, other) =
        crate::app::testing::real_photo_on(owner.clone(), join, &fixture, crate::Config::default());
    {
        use luxforge_testkit::client::{call, mutation, request_id, revision};
        let id = json!(asset.as_str());
        call(
            &owner,
            other,
            "edit.set-pixel",
            json!({"asset_id": id, "x": 3, "y": 2, "rgb": [9, 9, 9],
                   "mutation": mutation(revision(&owner, other, &id).unwrap(),
                                        &request_id("notice"), "agent")}),
        )
        .expect("a pixel replacement");
    }
    let refreshed = crate::app::tasks::refresh(
        &owner,
        editor.client,
        asset,
        crate::app::tasks::Scope::Open,
        editor.drawn(),
    )
    .unwrap();
    let _ = editor.update(Message::Sync(super::message::sync::SyncMessage::Refreshed(
        Ok(Box::new(refreshed)),
    )));
    editor.gpu.surface = Some(SurfaceReport::default());
    for value in [0.2, 0.4] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_eq!(latest_reason(&editor), json!("pixel-stage"));
        assert_says(&editor, "pixel-stage", Some(STACK));
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
        fallback: Some(SurfaceFallback::SourceUploading {
            uploaded: 1,
            bytes: 2,
        }),
        drawn: None,
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    assert_says(&editor, "source-uploading", None);
    compiling(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.7);
    assert_says(&editor, "compiling", None);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// The reference renderer's notice is the session's. Before the surface has checked its stage
/// (`surface-pending`) and once it draws on the GPU nothing is said; once the desktop reports a
/// lost device, the bar says the reference renderer at rest, before any gesture, through a drag
/// whose every tick names `device-lost`, and still once the release has settled, beside the reason
/// the evidence records.
#[test]
fn gpu_preview_the_notice_says_the_reference_renderer_from_the_session_at_rest_and_in_a_drag() {
    use super::renderer_tests::report;
    use luxforge_core::{Renderer, RendererReason};
    use luxforge_ui::photo_surface::GpuStageState;
    let catalog = catalog("notice-reference");
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert_eq!(
        editor.session.renderer,
        Renderer::reference(RendererReason::SurfacePending)
    );
    assert_eq!(
        editor.workspace.status.fallback, None,
        "the stage is not checked yet"
    );
    editor.renderer.stage = Some(GpuStageState::Available);
    report(&mut editor);
    assert_eq!(editor.session.renderer, Renderer::gpu());
    assert_eq!(
        editor.workspace.status.fallback, None,
        "the GPU says nothing"
    );

    editor.renderer.stage = Some(GpuStageState::DeviceLost);
    report(&mut editor);
    assert_says(&editor, "device-lost", Some(REFERENCE));
    for value in [0.1, 0.2] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_eq!(latest_reason(&editor), json!("device-lost"));
        assert_says(&editor, "device-lost", Some(REFERENCE));
    }
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag()
    });
    assert_says(&editor, "device-lost", Some(REFERENCE));
    finish(editor, catalog);
}

const REST_COMPILING: (&str, &str) = (
    "Preparing GPU render",
    "The GPU renderer is compiling its programs for this photo, so the reference renderer draws \
     it on the CPU until they are ready.",
);

/// The photograph just opened is the reference renderer's frame while the GPU's picture at rest
/// compiles its programs, and the status bar says so at once, with no gesture open and no reason
/// of a tick's; the GPU frame that replaces it, once they are compiled, takes the notice away.
#[test]
fn gpu_preview_the_picture_at_rest_is_labelled_the_reference_while_its_programs_compile() {
    let catalog = catalog("notice-rest-compiling");
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert!(
        editor.gpu_rest_plan().is_some(),
        "the picture at rest is planned"
    );
    // The surface's first frame of the picture at rest finds its programs compiling, and draws the
    // reference frame.
    editor.gpu.surface = Some(SurfaceReport {
        fallback: Some(SurfaceFallback::Compiling),
        ..SurfaceReport::default()
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let said = json!({"phrase": REST_COMPILING.0, "tooltip": REST_COMPILING.1});
    let snapshot = editor.snapshot();
    let gpu = &snapshot["surface"]["gpu"];
    assert_eq!(gpu["rest_compiling"], json!(true));
    assert_eq!(gpu["plan_fallback"], Value::Null, "no gesture's reason");
    assert_eq!(gpu["fallback_notice"], said, "the evidence");
    assert_eq!(
        snapshot["status_bar"]["fallback"], said,
        "beside the render slot"
    );
    assert!(
        !editor.workspace.status.render.starts_with("GPU"),
        "the render slot names the reference frame: {}",
        editor.workspace.status.render
    );
    // Its programs compiled, the surface draws the GPU's picture at rest: nothing more is said.
    editor.gpu.surface = Some(SurfaceReport {
        drawn: Some((1, 0)),
        ..SurfaceReport::default()
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["surface"]["gpu"]["rest_compiling"], json!(false));
    assert_eq!(snapshot["status_bar"]["fallback"], Value::Null);
    assert_eq!(editor.workspace.status.fallback, None);
    // A gesture's own `compiling` is the gesture's, said after half a second, never this notice.
    editor.gpu.surface = Some(SurfaceReport {
        fallback: Some(SurfaceFallback::Compiling),
        ..SurfaceReport::default()
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    assert!(!editor.gpu_rest_compiling(), "a drag is open");
    assert_eq!(
        editor.workspace.status.fallback, None,
        "not half a second yet"
    );
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A warm-up the compile thread runs is listed on the owner's activity board while it runs, which
/// the Performance section and `activity.list` read, and ends when the warm-up does; each ended
/// warm-up is recorded once as the `gpu_warm_up` event with its figures, the launch's first
/// marked so.
#[test]
fn gpu_preview_a_warm_up_is_listed_while_it_runs_and_recorded_once_it_ends() {
    use super::gpu_warm::{WARM_UP_KIND, WARM_UP_LABEL};
    use luxforge_ui::photo_surface::WarmUpFigures;
    let catalog = catalog("warm-up");
    let (mut editor, _, _) = real_photo(&catalog);
    let log = super::testing::attach_log(&mut editor);
    let listed = |editor: &Editor| -> Vec<Value> {
        let (answer, _) =
            super::tasks::call(&editor.owner, editor.client, "activity.list", json!({}))
                .expect("activity.list");
        answer["active"]
            .as_array()
            .expect("active")
            .iter()
            .filter(|entry| entry["kind"] == json!(WARM_UP_KIND))
            .map(|entry| entry["label"].clone())
            .collect()
    };
    let running = WarmUpFigures {
        period: 1,
        version: 3,
        sequences: 7,
        open_sequences: 2,
        open_us: None,
        us: None,
    };
    editor.gpu.warm_up.figures = Some(running);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(
        listed(&editor),
        [json!(WARM_UP_LABEL)],
        "listed while it runs"
    );
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(listed(&editor).len(), 1, "listed once");
    assert_eq!(
        editor.snapshot()["surface"]["gpu"]["warm_up"]["running"],
        json!(true)
    );
    let ended = WarmUpFigures {
        open_us: Some(1_200_000),
        us: Some(4_500_000),
        ..running
    };
    editor.gpu.warm_up.figures = Some(ended);
    for _ in 0..2 {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    }
    assert!(listed(&editor).is_empty(), "ended with the warm-up");
    let records = super::testing::logged(&mut editor, &log);
    let recorded = super::testing::events(&records, "gpu_warm_up");
    assert_eq!(recorded.len(), 1, "recorded once");
    assert_eq!(
        *recorded[0],
        json!({"period": 1, "version": 3, "sequences": 7, "open_sequences": 2,
            "open_ms": 1200.0, "ms": 4500.0, "running": false, "first": true})
    );
    finish(editor, catalog);
}
