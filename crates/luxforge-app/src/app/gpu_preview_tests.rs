//! A gesture drawn on the GPU, end to end against a real owner and preview worker: one boundary
//! job at draft begin and no preview job per tick once the boundary is held and the surface has
//! evaluated it; CPU frames until then and whenever the surface cannot draw; the boundary released
//! at commit, cancel and a key change; and an ineligible stack's reason.
use super::{
    gpu_preview::SurfaceReport,
    message::{draft::DraftMessage, preview::PreviewMessage, sync::SyncMessage, view::ViewMessage},
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
    *,
};
use luxforge_testbase::wait_until;
use luxforge_ui::photo_surface::GpuFallback as SurfaceFallback;

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-gpu-preview-{name}-{}.sqlite",
        std::process::id()
    ))
}

/// Take up the preview worker's results, as the worker's wake does, until `done`.
fn deliver_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&Editor) -> bool) {
    wait_until(what, || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        done(editor)
    });
}

/// The surface reports, as it does once its pipeline is ready and its slot holds the boundary,
/// that it evaluated the held boundary's plan with no fallback.
fn surface_ready(editor: &mut Editor) {
    let version = editor.gpu.held_version().expect("a held boundary");
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: None,
    });
}

/// How many jobs went to the preview worker.
fn jobs(records: &[Value]) -> usize {
    events(records, "preview_job_requested").len()
}

/// A Fit drag of Basic's exposure: its first tick takes the CPU path and its job carries the one
/// boundary request; ticks before the boundary arrives take the CPU path and ask for it no more;
/// once it is held and the surface has evaluated it, every tick is drawn on the GPU with no preview
/// job, each GPU frame tagged with its tick's draft revision; and the release commits and
/// releases the boundary once the committed frame is presented.
#[test]
fn gpu_preview_a_fit_drag_makes_one_boundary_job_and_no_job_per_tick() {
    let catalog = catalog("drag");
    let (mut editor, _, _) = real_photo(&catalog);
    // The surface has not drawn anything yet.
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.15);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    let (gpu, cpu, requests) = editor.gpu.ticks();
    assert_eq!((gpu, cpu, requests), (0, 2, 1), "one boundary request");
    let records = logged(&mut editor, &log);
    assert_eq!(
        jobs(&records),
        2,
        "each tick before the boundary has its job"
    );
    let held = events(&records, "gpu_boundary");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["held"], true);
    let ticks = events(&records, "gpu_preview_tick");
    assert!(ticks.iter().all(|tick| tick["path"] == "cpu"));
    assert_eq!(ticks[0]["reason"], "boundary-pending");
    // The boundary is drawn at once, under the newest tick's revision; a tick waits for the
    // surface's report that it evaluated it.
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    assert_eq!(editor.surfaces().gpu_tag, Some(revision));
    surface_ready(&mut editor);
    let log = attach_log(&mut editor);
    for value in [0.2, 0.3, 0.45, 0.6] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        let revision = editor.session.draft.as_ref().unwrap().draft_revision;
        let surfaces = editor.surfaces();
        assert!(surfaces.gpu.is_some(), "the plan is drawn");
        assert_eq!(surfaces.gpu_tag, Some(revision), "tagged with its tick");
        assert!(!surfaces.gpu_hold);
    }
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "no preview job per tick");
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks.len(), 4);
    assert!(ticks.iter().all(|tick| tick["path"] == "gpu"));
    assert_eq!(editor.gpu.ticks(), (4, 2, 1));
    // The release commits; the boundary is held until the committed frame replaces the drafted
    // one, then released.
    let log = attach_log(&mut editor);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    assert!(
        editor.gpu.has_drag(),
        "held until the committed frame is presented"
    );
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag()
    });
    let records = logged(&mut editor, &log);
    let released = events(&records, "gpu_boundary_released");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0]["why"], "draft-ended");
    assert!(editor.surfaces().gpu.is_none());
    finish(editor, catalog);
}

/// A tick the surface cannot draw — its sequence still compiling, or any other fallback — takes
/// the CPU path with that reason, and its job goes to the worker as before.
#[test]
fn gpu_preview_a_tick_the_surface_cannot_draw_takes_the_cpu_path() {
    let catalog = catalog("fallback");
    let (mut editor, _, _) = real_photo(&catalog);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    for (fallback, value) in [
        (SurfaceFallback::Compiling, 0.25),
        (SurfaceFallback::DeviceLost, 0.35),
        // A plan the surface finds over the GPU-preview budget once its boundary is held.
        (
            SurfaceFallback::BudgetExceeded {
                requested: 2,
                in_use: 0,
                budget: 1,
            },
            0.45,
        ),
    ] {
        editor.gpu.surface = Some(SurfaceReport {
            ready_boundary: None,
            fallback: Some(fallback),
            drawn: None,
        });
        let log = attach_log(&mut editor);
        let _ = slide(&mut editor, ACTION, FIELD, value);
        let records = logged(&mut editor, &log);
        assert_eq!(jobs(&records), 1, "{fallback:?}");
        let ticks = events(&records, "gpu_preview_tick");
        assert_eq!(ticks[0]["path"], "cpu");
        assert_eq!(ticks[0]["reason"], fallback.as_str());
        // The plan is still handed over, so the surface draws it once it can.
        assert!(editor.surfaces().gpu.is_some());
    }
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// Cancel releases the boundary once the frame read back after the cancel is presented, and a
/// changed key — a smaller window gives the drag a proxy, and so another boundary — releases the
/// held one and asks for the new one.
#[test]
fn gpu_preview_the_boundary_is_released_at_cancel_and_on_a_key_change() {
    let catalog = catalog("release");
    let (mut editor, _, _) = real_photo(&catalog);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let first = editor.gpu.held_version();
    // A window too small for the photograph at its own size: the Fit frame becomes a proxy.
    let _ = editor.update(Message::View(ViewMessage::Resized(900.0, 600.0)));
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    let released = events(&records, "gpu_boundary_released");
    assert_eq!(released.len(), 1, "the old boundary goes");
    assert_eq!(released[0]["why"], "key-changed");
    assert_eq!(jobs(&records), 1, "the tick asks for the new one");
    assert_eq!(editor.gpu.ticks().2, 2);
    deliver_until(&mut editor, "the new boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    assert_ne!(editor.gpu.held_version(), first);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    deliver_until(&mut editor, "the frame read back", |editor| {
        !editor.gpu.has_drag()
    });
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "gpu_boundary_released")[0]["why"],
        "draft-ended"
    );
    finish(editor, catalog);
}

/// An ineligible drag takes the CPU path and says why: at a percentage zoom below 100% no GPU
/// preview is planned at all.
#[test]
fn gpu_preview_an_ineligible_drag_names_its_reason() {
    let catalog = catalog("ineligible");
    let (mut editor, _, _) = real_photo(&catalog);
    // The session the owner answered a 50% zoom with.
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 50.0 };
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 1);
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "cpu");
    assert_eq!(ticks[0]["reason"], "not-fit");
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A tick drawn on the GPU puts the gesture's newest frame on screen as a CPU frame would: an
/// evidence step waiting for the drained slider's frame settles on it, and the capture then waits
/// for the surface's draw of that tick.
#[test]
fn gpu_preview_a_gpu_tick_settles_the_slider_step_waiting_for_it() {
    let catalog = catalog("settle");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    editor.evidence = Some(crate::app::testing::scripted_evidence("[]"));
    editor.await_step(evidence::Settle::SliderDraft);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "the tick was drawn on the GPU");
    assert_eq!(editor.gpu.ticks().0, 1);
    assert!(
        editor.evidence.as_ref().unwrap().awaiting.is_none(),
        "the step settled on the GPU tick"
    );
    let settled = events(&records, "script_step_settled");
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0]["waited_for"], "slider_draft");
    editor.evidence = None;
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A first tick that leaves the photograph's pixels as they are still sends its job to the
/// worker, which renders the boundary it asks for: pixels reused from the frame on screen would
/// answer the frame and drop the request, and the drag would never leave the CPU path.
#[test]
fn gpu_preview_a_tick_that_changes_no_pixel_still_asks_for_its_boundary() {
    let catalog = catalog("unchanged");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
    // A new drag whose first value is the one committed: the same pixels.
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 1, "the job went to the worker");
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    assert_eq!(editor.gpu.ticks().2, 1, "asked once");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// While a clipping overlay is shown, which is derived from the CPU's frames, a drag takes the CPU
/// path and says why, asking for no boundary.
#[test]
fn gpu_preview_a_drag_with_clipping_shown_keeps_the_cpu_path() {
    let catalog = catalog("clipping");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.session.workspace.clip_highlights = true;
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 2);
    let ticks = events(&records, "gpu_preview_tick");
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "clipping-shown")
    );
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
    assert_eq!(editor.gpu_plan_fallback(), Some("clipping-shown".into()));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// Another client's commit during a GPU-drawn drag conflicts the draft, and the photograph is then
/// the new entry's CPU frame: the plans the drag drew were planned over the entry before, so none
/// is handed to the surface until a tick plans over the current entry, as Reapply's does.
#[test]
fn gpu_preview_a_commit_elsewhere_withdraws_the_drags_plan_until_it_plans_again() {
    use luxforge_testkit::client::{call, mutation, request_id};
    let catalog = catalog("elsewhere");
    let (mut editor, asset, client) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.5);
    assert!(
        editor.surfaces().gpu.is_some(),
        "the drag is drawn on the GPU"
    );

    // Another client turns the photograph, which this desktop's poll reads back.
    let agent = editor.owner.register();
    let revision = editor.document.state.as_ref().unwrap().revision;
    call(
        &editor.owner,
        agent,
        "edit.transform",
        json!({
            "asset_id": asset,
            "transform": "rotate-right",
            "mutation": mutation(revision, &request_id("agent"), "agent"),
        }),
    )
    .expect("the agent's commit");
    let own = editor.sync.own_requests.iter().cloned().collect::<Vec<_>>();
    let polled = tasks::sync_now(
        &editor.owner,
        client,
        (asset.clone(), revision),
        editor.sync.sequence,
        &own,
        editor.proxy_bounds(),
    )
    .expect("the poll answers");
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    assert!(
        editor
            .core_gesture()
            .expect("the draft is kept")
            .draft
            .conflicted,
        "the commit conflicts the draft"
    );
    assert!(
        editor.surfaces().gpu.is_none(),
        "a plan made over the entry before is not drawn over the new one"
    );

    // Reapply plans over the new entry, and its tick is drawn on the GPU again.
    let _ = editor.update(Message::Draft(DraftMessage::Reapply));
    let draft = &editor.core_gesture().expect("the rebased draft").draft;
    assert!(!draft.conflicted);
    deliver_until(&mut editor, "the reapplied tick's plan", |editor| {
        editor.surfaces().gpu.is_some() || !editor.presentation.queue.is_busy()
    });
    if !editor.gpu.holds_boundary() {
        deliver_until(&mut editor, "the boundary", |editor| {
            editor.gpu.holds_boundary()
        });
        surface_ready(&mut editor);
        let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    }
    let surfaces = editor.surfaces();
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    assert!(surfaces.gpu.is_some(), "the rebased draft is drawn again");
    assert_eq!(surfaces.gpu_tag, Some(revision));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// The compile cost of each program sequence a Fit drag of the colour modules draws, measured on
/// this host's adapter through the stage's own compile (`naga` checks, backend translation and the
/// driver's pipeline). A functional measurement for the design's choice of one pipeline per
/// sequence, not a timing gate: run it on purpose, with `--ignored --nocapture`, and record the
/// build profile and host beside its figures. Each sequence is compiled first in a new process's
/// order, then again, which the driver's own shader cache may serve.
#[test]
#[ignore = "a measurement, run on purpose"]
fn gpu_preview_compile_cost_per_sequence() {
    use luxforge_core::{GpuPlanRequest, Layer, ModuleRegistry, Recipe, Stage, gpu_plan};
    let test = "gpu_preview_compile_cost_per_sequence";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let basic = || Layer::new(luxforge_core::BASIC_EFFECT, json!({"exposure": 0.5}));
    let curve = || {
        Layer::new(
            "luxforge.curve.tone",
            json!({"luminance": [[0.0, 0.0], [0.4, 0.5], [1.0, 1.0]]}),
        )
    };
    let mixer = || Layer::new(luxforge_core::MIXER_EFFECT, json!({"red-hue": 20.0}));
    let vignette = || Layer::new(luxforge_core::VIGNETTE_EFFECT, json!({"amount": -30.0}));
    let stage = Stage {
        width: 1600,
        height: 1067,
    };
    let full = Stage {
        width: 6000,
        height: 4000,
    };
    let cases: [(&str, Vec<Layer>, usize); 6] = [
        ("Basic", vec![basic()], 0),
        ("Tone curve", vec![basic(), curve()], 1),
        ("Mixer", vec![basic(), curve(), mixer()], 2),
        ("Vignette", vec![basic(), vignette()], 1),
        (
            "Basic under a curve and a mixer",
            vec![basic(), curve(), mixer()],
            0,
        ),
        (
            "Basic under every colour module",
            vec![basic(), curve(), mixer(), vignette()],
            0,
        ),
    ];
    eprintln!("{test}: adapter {}", qualifier.adapter());
    for (what, layers, boundary) in cases {
        let recipe = Recipe {
            format: luxforge_core::RECIPE_FORMAT,
            layers,
            ..Recipe::default()
        };
        let request = GpuPlanRequest::fit(boundary, stage, full).drafted(boundary);
        let plan = match gpu_plan(&registry, &recipe, request).unwrap() {
            luxforge_core::GpuAnswer::Plan(plan) => plan,
            luxforge_core::GpuAnswer::Fallback(reason) => panic!("{what}: {reason}"),
        };
        let steps = super::gpu_plan::plan_steps(&plan).expect("a sequence the surface runs");
        let first = qualifier.compile_time(&steps).expect("it compiles");
        let again = qualifier.compile_time(&steps).expect("it compiles");
        eprintln!(
            "{test}: {what}: {} steps, first {:.2} ms, again {:.2} ms",
            steps.len(),
            first.as_secs_f64() * 1000.0,
            again.as_secs_f64() * 1000.0
        );
    }
}

/// The visible region of the 480 × 320 photograph at 400%, which the window shows only part of.
fn zoomed(editor: &mut Editor) -> luxforge_core::Region {
    deliver_until(editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 400.0 };
    let stage = editor
        .presentation
        .dimensions
        .expect("the photograph's stage");
    let wanted = editor.desired_view_for(stage).expect("a visible region");
    assert!(
        wanted.width < stage.0 && wanted.height < stage.1,
        "the view shows part of the photograph: {wanted:?} of {stage:?}"
    );
    wanted
}

/// The held boundary's region, as the drag's evidence names it.
fn held_region(editor: &Editor) -> Value {
    editor.gpu.summary()["drag"]["boundary"]["region"].clone()
}

fn corners(rect: luxforge_core::Region) -> Value {
    json!([rect.x0, rect.y0, rect.width, rect.height])
}

/// A drag at a percentage zoom of 100% or more is drawn over the visible region at full scale: its
/// first tick takes the CPU path and its region job carries the one boundary request, for that
/// region; once the boundary is held and the surface has evaluated it, every tick is drawn on the
/// GPU with no preview job — no tick's, and no region job for the view, which the GPU frame holds —
/// and the plan handed to the surface draws that region of the stage.
#[test]
fn gpu_preview_a_percentage_drag_draws_its_region_with_no_job_per_tick() {
    let catalog = catalog("region");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let wanted = zoomed(&mut editor);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 1);
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "cpu");
    assert_eq!(ticks[0]["reason"], "boundary-pending");
    assert_eq!(ticks[0]["boundary_requested"], true);
    deliver_until(&mut editor, "the region's boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    assert_eq!(held_region(&editor), corners(wanted));
    assert_eq!(editor.gpu.summary()["drag"]["zoom"], 400.0);
    surface_ready(&mut editor);
    let log = attach_log(&mut editor);
    for value in [0.2, 0.3, 0.45] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        let revision = editor.session.draft.as_ref().unwrap().draft_revision;
        let surfaces = editor.surfaces();
        let plan = surfaces.gpu.expect("the region's plan is drawn");
        let region = plan.region.expect("a region plan");
        assert_eq!(
            region.rect,
            [wanted.x0, wanted.y0, wanted.x1(), wanted.y1()]
        );
        assert_eq!(region.stage, editor.presentation.dimensions.unwrap());
        assert_eq!(surfaces.gpu_tag, Some(revision), "tagged with its tick");
        assert!(!surfaces.gpu_hold);
        assert!(
            editor.gpu_draws_view(wanted),
            "the GPU frame holds the view"
        );
        // The tick's view motion plans no region job: the GPU frame answers the view.
        assert!(!editor.view_plan.in_flight, "no region job for the view");
        assert!(!editor.view_plan.dirty);
        assert!(
            editor.view_plan.quiet_since.is_some(),
            "the quiet policy runs"
        );
    }
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "no preview job per tick");
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks.len(), 3);
    assert!(ticks.iter().all(|tick| tick["path"] == "gpu"));
    assert_eq!(editor.gpu.ticks(), (3, 1, 1));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A pan during a GPU-drawn drag at a percentage zoom to where the held region does not reach
/// withdraws the plan at once, so the surface draws the CPU's frames and never the GPU frame of one
/// region beside them; the next tick asks for the new region's boundary and takes the CPU path,
/// handing the surface no plan until that boundary is held, and then draws the new region on the
/// GPU.
#[test]
fn gpu_preview_a_pan_past_the_held_region_draws_the_cpu_frame_until_the_new_boundary() {
    let catalog = catalog("pan");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let first = zoomed(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the region's boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert!(editor.surfaces().gpu.is_some(), "drawn on the GPU");
    let version = editor.gpu.held_version();
    // Pan to the photograph's far corner, which the held region does not reach.
    let _ = editor.update(Message::View(ViewMessage::Panned(1400.0, 900.0)));
    let stage = editor.presentation.dimensions.unwrap();
    let second = editor.desired_view_for(stage).expect("a visible region");
    assert!(!super::preview::contains_region(first, second));
    assert!(
        editor.surfaces().gpu.is_none(),
        "the plan is withdrawn the moment the view leaves its region"
    );
    assert!(!editor.gpu_draws_view(second));
    // The next tick plans the new region: the old boundary goes and the new one is asked for.
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    let released = events(&records, "gpu_boundary_released");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0]["why"], "key-changed");
    assert_eq!(released[0]["version"], json!(version));
    assert_eq!(
        jobs(&records),
        1,
        "the tick's region job carries the request"
    );
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "cpu");
    assert_eq!(ticks[0]["reason"], "boundary-pending");
    assert!(
        editor.surfaces().gpu.is_none(),
        "no plan until the new boundary"
    );
    deliver_until(&mut editor, "the new region's boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    assert_eq!(held_region(&editor), corners(second));
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    let plan = editor.surfaces().gpu.expect("the new region's plan");
    assert_eq!(
        plan.region.map(|region| region.rect),
        Some([second.x0, second.y0, second.x1(), second.y1()])
    );
    assert_eq!(editor.gpu.ticks().2, 2, "one boundary request a region");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A region whose boundary and frame alone would pass the GPU-preview budget asks for no boundary:
/// every tick takes the CPU path, naming the budget, and the drag's evidence carries the bytes and
/// the budget they pass.
#[test]
fn gpu_preview_a_region_over_the_budget_keeps_the_cpu_path_and_names_it() {
    let catalog = catalog("budget");
    let (mut editor, _, _) = real_photo(&catalog);
    let wanted = zoomed(&mut editor);
    // A Basic layer's boundary is the region itself, with no tail and no plane: on a JPEG eight
    // bytes a texel, and four an output pixel.
    let needed = u64::from(wanted.width) * u64::from(wanted.height) * 12;
    editor.gpu.budget = Some(needed - 1);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 2, "each tick has its region job");
    let ticks = events(&records, "gpu_preview_tick");
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "budget-exceeded"),
        "{ticks:?}"
    );
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
    assert_eq!(
        editor.gpu.summary()["drag"]["over_budget"],
        json!({"requested": needed, "budget": needed - 1})
    );
    assert_eq!(editor.gpu_plan_fallback(), Some("budget-exceeded".into()));
    assert!(editor.surfaces().gpu.is_none());
    // Within the budget the same region is drawn on the GPU.
    editor.gpu.budget = Some(needed);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    assert_eq!(editor.gpu.ticks().2, 1, "the boundary is asked for");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// Every family of the qualification corpus at 100%, in the largest window the owner's display
/// holds: the worker's exact visible region against the GPU frame of the region plan over the
/// region's own boundary, each held to its recipe's class, and a slot over the GPU-preview budget
/// a gap naming it.
#[test]
#[ignore = "the GPU preview corpus at 100%: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_preview_corpus_at_100_percent() {
    super::gpu_qualification::corpus_at_percent(
        "gpu_preview_corpus_at_100_percent",
        &[
            "basic",
            "tone-curve",
            "mixer",
            "vignette",
            "colour-stack",
            "mask-linear",
            "mask-radial",
            "mask-brush",
            "mask-luminance-range",
            "mask-colour-range",
            "mask-composed",
            "crop",
            "lens-perspective",
            "presence",
            "detail",
        ],
        100.0,
    );
}
