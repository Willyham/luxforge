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
const PRESENCE: &str = "set-presence";
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

/// A Fit drag of Basic's exposure: its first tick takes the CPU path and its job carries the
/// boundary request; ticks before the boundary arrives take the CPU path, and one is rendered, by
/// the last job that asked, so no request is made while another is in flight; once it is held and
/// the surface has evaluated it, every tick is drawn on the GPU with no preview job, each GPU frame
/// tagged with its tick's draft revision; and the release commits and releases the boundary once
/// the committed frame is presented.
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
    assert_eq!((gpu, cpu), (0, 2));
    let records = logged(&mut editor, &log);
    assert_eq!(
        jobs(&records),
        2,
        "each tick before the boundary has its job"
    );
    let ticks = events(&records, "gpu_preview_tick");
    assert!(ticks.iter().all(|tick| tick["path"] == "cpu"));
    assert_eq!(ticks[0]["reason"], "boundary-pending");
    // The first tick asks for the boundary. The second asks again only if the first's job was
    // still waiting in the pending slot, where the second's replaces it so it never starts; which
    // it was depends on how soon the worker took the first job up. Either way one boundary is
    // rendered, by the last job that asked: had a job that asked earlier started, its boundary
    // would have been delivered first.
    let asked: Vec<&Value> = ticks
        .iter()
        .copied()
        .filter(|tick| tick["boundary_requested"] == true)
        .collect();
    assert_eq!(ticks[0]["boundary_requested"], true, "the first tick asks");
    assert_eq!(asked.len() as u64, requests);
    let held = events(&records, "gpu_boundary");
    assert_eq!(held.len(), 1, "one boundary rendered");
    assert_eq!(held[0]["held"], true);
    assert_eq!(
        held[0]["generation"],
        asked.last().unwrap()["generation"],
        "rendered by the last job that asked"
    );
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
    assert_eq!(editor.gpu.ticks(), (4, 2, requests), "and asks for none");
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

/// Commit `field` at `value` with a released slider, and wait for the committed frame.
fn commit(editor: &mut Editor, action: &str, field: &str, value: f64) {
    let _ = slide(editor, action, field, value);
    let _ = let_go(editor, action, field);
    assert!(run_commit(editor));
    deliver_until(editor, "the committed frame", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
}

/// A Presence drag, and a Basic drag under Presence, at Fit: each opens with one CPU tick whose
/// job asks for the boundary, then draws every tick on the GPU from a plan holding Presence's
/// spatial step, with no preview job. The Presence drag's plan reads Dehaze's light from the store
/// the committed frame filled; the Basic drag changes the input the light is estimated from, so
/// its plan takes the light on the GPU and says it is approximate.
#[test]
fn gpu_preview_presence_drags_draw_on_the_gpu_with_no_job_per_tick() {
    let catalog = catalog("presence");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    commit(&mut editor, PRESENCE, "dehaze", 40.0);
    gpu_drag(
        &mut editor,
        PRESENCE,
        "clarity",
        &[20.0, 30.0, 45.0, 60.0, 75.0],
        false,
    );
    gpu_drag(
        &mut editor,
        ACTION,
        FIELD,
        &[0.1, 0.2, 0.3, 0.45, 0.6],
        true,
    );
    finish(editor, catalog);
}

/// One drag drawn on the GPU through a plan holding Presence's spatial step, then committed: its
/// first tick's job asks for the boundary, and once the boundary is held and the surface has
/// evaluated it every later tick is drawn on the GPU with no preview job, the plan's light stored
/// or taken on the GPU as `approximate` says. Answers the last tick's plan.
fn gpu_drag(
    editor: &mut Editor,
    action: &str,
    field: &str,
    values: &[f64],
    approximate: bool,
) -> luxforge_ui::photo_surface::GpuPlan {
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(editor);
    let _ = slide(editor, action, field, values[0]);
    deliver_until(editor, "the boundary", |editor| editor.gpu.holds_boundary());
    let records = logged(editor, &log);
    assert_eq!(
        jobs(&records),
        1,
        "{field}: the first tick's job asks for the boundary"
    );
    surface_ready(editor);
    let log = attach_log(editor);
    for value in &values[1..] {
        let _ = slide(editor, action, field, *value);
        let revision = editor.session.draft.as_ref().unwrap().draft_revision;
        let surfaces = editor.surfaces();
        assert!(surfaces.gpu.is_some(), "{field}: the plan is drawn");
        assert_eq!(
            surfaces.gpu_tag,
            Some(revision),
            "{field}: tagged with its tick"
        );
    }
    let records = logged(editor, &log);
    assert_eq!(jobs(&records), 0, "{field}: no preview job per tick");
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks.len(), values.len() - 1, "{field}");
    assert!(ticks.iter().all(|tick| tick["path"] == "gpu"), "{field}");
    let (plan, _) = editor.gpu.surface_plan().expect("a plan");
    let plan = plan.clone();
    assert!(
        plan.steps
            .iter()
            .any(|step| matches!(step, luxforge_ui::photo_surface::GpuStep::Spatial(_))),
        "{field}: Presence is in the plan"
    );
    assert_eq!(
        editor.gpu.summary()["drag"]["approximate"],
        json!(approximate),
        "{field}: where the light comes from"
    );
    let _ = let_go(editor, action, field);
    assert!(run_commit(editor));
    deliver_until(editor, "the committed frame", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
    plan
}

/// While a clipping overlay is shown, which is derived from the CPU's frames, a drag is still drawn
/// on the GPU, its plan marking its own clipped pixels in the overlay's colours by the quantizer's
/// thresholds; the marks follow the overlay's classes, and its warm sequences carry them.
#[test]
fn gpu_preview_a_drag_with_clipping_shown_draws_its_own_marks() {
    use luxforge_ui::photo_surface::GpuStep;
    let catalog = catalog("clipping");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.session.workspace.clip_highlights = true;
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "drawn on the GPU");
    assert_eq!(editor.gpu_plan_fallback(), None);
    let marks = |editor: &Editor| match editor.surfaces().gpu.and_then(|plan| plan.steps.last()) {
        Some(GpuStep::Clipping(marks)) => Some(*marks),
        _ => None,
    };
    let shown = marks(&editor).expect("the plan's last step marks clipping");
    assert_eq!((shown.shadows, shown.highlights), (false, true));
    let palette = super::overlay::palette();
    assert_eq!(shown.palette, [palette[1], palette[2], palette[3]]);
    // Both classes, then none: the next tick's plan follows.
    editor.session.workspace.clip_shadows = true;
    let _ = slide(&mut editor, ACTION, FIELD, 0.35);
    let shown = marks(&editor).expect("marks");
    assert_eq!((shown.shadows, shown.highlights), (true, true));
    editor.session.workspace.clip_shadows = false;
    editor.session.workspace.clip_highlights = false;
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert!(marks(&editor).is_none(), "no overlay, no marks");
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

/// The compile cost of the spatial sequences the editor compiles around a Presence layer, on a
/// cold shader cache: a Presence drag's own first, then drags of the layers before it and a Detail
/// drag with Presence after it, in one process, as the editor's warm lists compile them, so a
/// sequence finds the pass pipelines an earlier one created. Every kernel and apply of the spatial
/// programs opens with a test of its words against this process's own constant, which no word
/// holds: the code the driver compiles is new to its shader cache in every process, where a comment
/// would not reach it at all. `LUXFORGE_COMPILE_CONSTANT` names the constant instead, so a second
/// run with an earlier run's constant measures the same sequences on a warm cache. A functional
/// measurement, not a timing gate: run it on purpose, with `--ignored --nocapture`, and record the
/// build profile, host and load beside its figures.
#[test]
#[ignore = "a measurement, run on purpose"]
fn gpu_preview_spatial_compile_cost_on_a_cold_cache() {
    use luxforge_core::{
        DETAIL_EFFECT, GpuPlanRequest, Layer, ModuleRegistry, PRESENCE_EFFECT, Recipe, Stage,
        gpu_plan,
    };
    use luxforge_ui::photo_surface::GpuStep;
    let test = "gpu_preview_spatial_compile_cost_on_a_cold_cache";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after the epoch")
        .as_nanos() as u32;
    let nonce = std::env::var("LUXFORGE_COMPILE_CONSTANT")
        .ok()
        .and_then(|constant| constant.parse::<u32>().ok())
        .unwrap_or((clock ^ std::process::id().rotate_left(16)) | 0x8000_0000);
    let cold = |source: &str| {
        source
            .replace(
                "words: u32, block: u32) {",
                &format!("words: u32, block: u32) {{\n    if words == {nonce}u {{ return; }}"),
            )
            .replace(
                "planes: u32) -> vec3<f32> {",
                &format!(
                    "planes: u32) -> vec3<f32> {{\n    if words == {nonce}u {{ return rgb; }}"
                ),
            )
    };
    let basic = || Layer::new(luxforge_core::BASIC_EFFECT, json!({"exposure": 0.5}));
    let curve = || {
        Layer::new(
            "luxforge.curve.tone",
            json!({"luminance": [[0.0, 0.0], [0.4, 0.5], [1.0, 1.0]]}),
        )
    };
    let mixer = || Layer::new(luxforge_core::MIXER_EFFECT, json!({"red-hue": 20.0}));
    let dehaze = || Layer::new(PRESENCE_EFFECT, json!({"dehaze": 25}));
    let presence = || {
        Layer::new(
            PRESENCE_EFFECT,
            json!({"texture": 40, "clarity": 30, "dehaze": 25}),
        )
    };
    let detail = || {
        Layer::new(
            DETAIL_EFFECT,
            json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}),
        )
    };
    let stage = Stage {
        width: 1600,
        height: 1067,
    };
    let full = Stage {
        width: 6000,
        height: 4000,
    };
    let cases: [(&str, Vec<Layer>, usize); 7] = [
        ("A Presence drag", vec![presence()], 0),
        (
            "Basic under Presence, Dehaze alone",
            vec![basic(), dehaze()],
            0,
        ),
        (
            "Basic under Presence, all three",
            vec![basic(), presence()],
            0,
        ),
        (
            "The curve under Presence, all three",
            vec![basic(), curve(), presence()],
            1,
        ),
        (
            "The mixer under Presence, all three",
            vec![basic(), curve(), mixer(), presence()],
            2,
        ),
        (
            "A Detail drag, Presence after it",
            vec![detail(), presence()],
            0,
        ),
        (
            "Basic under Detail and Presence",
            vec![basic(), detail(), presence()],
            0,
        ),
    ];
    eprintln!("{test}: adapter {}, constant {nonce}", qualifier.adapter());
    let mut total = std::time::Duration::ZERO;
    for (what, layers, drafted) in cases {
        let recipe = Recipe {
            format: luxforge_core::RECIPE_FORMAT,
            layers,
            ..Recipe::default()
        };
        let request = GpuPlanRequest::fit(drafted, stage, full).drafted(drafted);
        let plan = match gpu_plan(&registry, &recipe, request).unwrap() {
            luxforge_core::GpuAnswer::Plan(plan) => plan,
            luxforge_core::GpuAnswer::Fallback(reason) => panic!("{what}: {reason}"),
        };
        let mut steps = super::gpu_plan::plan_steps(&plan).expect("a sequence the surface runs");
        let mut passes = 0;
        for step in &mut steps {
            if let GpuStep::Spatial(spatial) = step {
                spatial.program.source = std::borrow::Cow::Owned(cold(&spatial.program.source));
                passes += spatial.passes.len();
            }
        }
        let before = qualifier.pass_pipelines_created();
        let time = qualifier.compile_time(&steps).expect("it compiles");
        total += time;
        eprintln!(
            "{test}: {what}: {} steps, {passes} passes, {} new pass pipelines, {:.1} ms",
            steps.len(),
            qualifier.pass_pipelines_created() - before,
            time.as_secs_f64() * 1000.0
        );
    }
    eprintln!(
        "{test}: every sequence: {} pass pipelines, {:.1} ms",
        qualifier.pass_pipelines_created(),
        total.as_secs_f64() * 1000.0
    );
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

/// At a percentage zoom a Presence drag is drawn in its GPU shape, every unit, while that slot
/// fits the budget; when it does not but the CPU's shape does, in the CPU's shape, the units its
/// values need, from the same held boundary; and when neither fits it takes the CPU path naming the
/// budget with the least it would take, the CPU shape's.
#[test]
fn gpu_preview_a_percentage_spatial_drag_draws_the_shape_the_budget_holds() {
    let catalog = catalog("shape");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    zoomed(&mut editor);
    let shape = |editor: &Editor| editor.gpu.summary()["drag"]["shape"].clone();
    let applies = |editor: &Editor| {
        editor.surfaces().gpu.and_then(|plan| {
            plan.steps.iter().find_map(|step| match step {
                luxforge_ui::photo_surface::GpuStep::Spatial(spatial) => {
                    Some(spatial.applies.len())
                }
                _ => None,
            })
        })
    };
    // Neither shape fits.
    editor.gpu.budget = Some(1);
    let _ = slide(&mut editor, PRESENCE, "clarity", 20.0);
    let (_, cpu) = editor.gpu.region_charge().expect("a region plan");
    let summary = editor.gpu.summary();
    assert_eq!(summary["drag"]["reason"], "budget-exceeded");
    assert_eq!(shape(&editor), "cpu");
    assert_eq!(summary["drag"]["over_budget"]["requested"], json!(cpu));
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
    // The budget holds the GPU shape.
    editor.gpu.budget = Some(u64::MAX);
    let _ = slide(&mut editor, PRESENCE, "clarity", 25.0);
    assert_eq!(shape(&editor), "gpu");
    let (_, gpu) = editor.gpu.region_charge().expect("a region plan");
    assert!(
        cpu < gpu,
        "the CPU shape takes {cpu} B, the GPU shape {gpu} B"
    );
    assert_eq!(editor.gpu.ticks().2, 1, "the boundary is asked for");
    deliver_until(&mut editor, "the region's boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let version = editor.gpu.held_version();
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, PRESENCE, "clarity", 30.0);
    assert_eq!(applies(&editor), Some(3), "Dehaze, Texture and Clarity");
    // Only the CPU's shape fits: Clarity alone, over the same boundary.
    editor.gpu.budget = Some(cpu);
    let _ = slide(&mut editor, PRESENCE, "clarity", 35.0);
    assert_eq!(shape(&editor), "cpu");
    assert_eq!(applies(&editor), Some(1), "Clarity alone");
    assert_eq!(editor.gpu.held_version(), version, "the same boundary");
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "no preview job per tick");
    let ticks = events(&records, "gpu_preview_tick");
    assert!(ticks.iter().all(|tick| tick["path"] == "gpu"), "{ticks:?}");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A Detail drag with Presence in the stack, at Fit: the plan chains Detail's operation and
/// Presence's from the Detail layer, and once the boundary is held every tick is drawn on the GPU
/// with no preview job.
#[test]
fn gpu_preview_a_detail_drag_under_presence_draws_on_the_gpu() {
    let catalog = catalog("detail-presence");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    commit(&mut editor, PRESENCE, "clarity", 30.0);
    commit(&mut editor, PRESENCE, "dehaze", 20.0);
    let plan = gpu_drag(
        &mut editor,
        "set-detail",
        "sharpening",
        &[20.0, 35.0, 50.0, 65.0],
        true,
    );
    let spatial = plan
        .steps
        .iter()
        .filter(|step| matches!(step, luxforge_ui::photo_surface::GpuStep::Spatial(_)))
        .count();
    assert_eq!(spatial, 2, "Detail's operation, then Presence's");
    finish(editor, catalog);
}

/// At 100% a Presence drag over a committed Dehaze reads the light the exact frames stored, so it
/// is drawn on the GPU over the visible region with no preview job per tick. A Basic drag under
/// that Presence changes the light's input, which the region alone cannot give, so it keeps the CPU
/// path and names `region-estimate`, asking for no boundary; with Dehaze back at neutral, a Basic
/// drag under Presence is drawn on the GPU too.
#[test]
fn gpu_preview_presence_drags_at_100_percent() {
    let catalog = catalog("presence-100");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    // The view the owner answers with is the session's: a commit's refresh answers it again, so
    // each gesture sets it, as the zoom control does.
    let at_100 = |editor: &mut Editor| {
        editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
        let stage = editor
            .presentation
            .dimensions
            .expect("the photograph's stage");
        editor.desired_view_for(stage).expect("a visible region")
    };
    at_100(&mut editor);
    commit(&mut editor, PRESENCE, "dehaze", 40.0);
    let wanted = at_100(&mut editor);
    let plan = gpu_drag(&mut editor, PRESENCE, "clarity", &[20.0, 30.0, 45.0], false);
    assert_eq!(
        plan.region.map(|region| region.rect),
        Some([wanted.x0, wanted.y0, wanted.x1(), wanted.y1()]),
        "the visible region"
    );
    // Under Presence with Dehaze: the CPU path, named, and no boundary asked for.
    at_100(&mut editor);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 2, "each tick has its region job");
    let ticks = events(&records, "gpu_preview_tick");
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "region-estimate"),
        "{ticks:?}"
    );
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    deliver_until(&mut editor, "the cancelled drag's frame", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
    // Dehaze at neutral: Presence holds no global estimate.
    at_100(&mut editor);
    commit(&mut editor, PRESENCE, "dehaze", 0.0);
    at_100(&mut editor);
    let plan = gpu_drag(&mut editor, ACTION, FIELD, &[0.1, 0.2, 0.3], false);
    assert!(plan.region.is_some(), "a region plan");
    finish(editor, catalog);
}

/// At a percentage zoom with Dehaze behind Detail, the CPU cannot cut the view's region: it draws
/// the whole frame's proxy while a drag moves and the whole exact frame once the view settles, which
/// stores the light. A Presence drag then reads that light, the drafted stack's own, and is drawn on
/// the GPU over the visible region with no preview job per tick, its boundary rendered after the
/// first tick's declined region job; and a Detail drag reads the light the stack it started from
/// stored, held for the drag, and is drawn on the GPU too, its plan approximate. (A store that holds
/// no light, which this photograph's exact Fit frames never leave, keeps the drag on the CPU path:
/// `behind_detail_a_region_plan_holds_dehazes_stored_light` in the core.)
#[test]
fn gpu_preview_dehaze_behind_detail_draws_its_region_on_the_gpu() {
    let catalog = catalog("dehaze-detail");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    let zoomed = |editor: &mut Editor| {
        editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 400.0 };
        let stage = editor
            .presentation
            .dimensions
            .expect("the photograph's stage");
        editor.desired_view_for(stage).expect("a visible region")
    };
    zoomed(&mut editor);
    commit(&mut editor, "set-detail", "sharpening", 40.0);
    zoomed(&mut editor);
    commit(&mut editor, PRESENCE, "dehaze", 40.0);
    let wanted = zoomed(&mut editor);
    let region = |plan: &luxforge_ui::photo_surface::GpuPlan| plan.region.map(|region| region.rect);
    let spatial = |plan: &luxforge_ui::photo_surface::GpuPlan| {
        plan.steps
            .iter()
            .filter(|step| matches!(step, luxforge_ui::photo_surface::GpuStep::Spatial(_)))
            .count()
    };
    let corners = Some([wanted.x0, wanted.y0, wanted.x1(), wanted.y1()]);
    let plan = gpu_drag(&mut editor, PRESENCE, "clarity", &[20.0, 30.0, 45.0], false);
    assert_eq!(region(&plan), corners, "the visible region");
    assert_eq!(spatial(&plan), 1, "Presence over Detail's output");
    zoomed(&mut editor);
    let plan = gpu_drag(
        &mut editor,
        "set-detail",
        "sharpening",
        &[50.0, 60.0, 70.0],
        true,
    );
    assert_eq!(region(&plan), corners, "the visible region");
    assert_eq!(spatial(&plan), 2, "Detail's operation, then Presence's");
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

/// Once the surface reports that its slot holds the boundary, the desktop lets the texels go and
/// hands the surface the resident boundary from then on, keeping no copy for the rest of the
/// gesture. A slot that lets it go — the surface names `boundary-released` — makes the next tick
/// ask for the boundary again, on the CPU path, and the texels it brings are held until the slot
/// holds them once more.
#[test]
fn gpu_preview_the_texels_are_let_go_once_the_slot_holds_them_and_asked_again_if_it_lets_go() {
    let catalog = catalog("resident");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    let texels_held =
        |editor: &Editor| editor.gpu.summary()["drag"]["boundary"]["texels_held"].clone();
    assert_eq!(
        texels_held(&editor),
        json!(true),
        "until the slot holds them"
    );
    let version = editor.gpu.held_version().unwrap();
    surface_ready(&mut editor);
    // The message the surface's draw wakes the desktop with lets them go, with no tick.
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(texels_held(&editor), json!(false));
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let plan = editor.surfaces().gpu.expect("the plan is drawn");
    assert!(!plan.boundary.holds_texels(), "the resident boundary");
    assert_eq!(plan.boundary.version(), version);
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "gpu_boundary_resident").len(), 1);
    assert_eq!(jobs(&records), 0, "the tick was drawn on the GPU");
    // The slot let it go.
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::BoundaryReleased),
        drawn: None,
    });
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    let released = events(&records, "gpu_boundary_released");
    assert_eq!(released.len(), 1, "{records:?}");
    assert_eq!(released[0]["why"], "slot-released");
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "cpu");
    assert_eq!(ticks[0]["reason"], "boundary-pending");
    assert_eq!(ticks[0]["boundary_requested"], true);
    assert_eq!(editor.gpu.ticks().2, 2, "asked again");
    deliver_until(&mut editor, "the boundary again", |editor| {
        editor.gpu.holds_boundary()
    });
    assert_eq!(texels_held(&editor), json!(true));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// While the surface is still uploading the boundary, a few chunks a frame, a tick takes the CPU
/// path naming `boundary-uploading`, its job going to the worker and no boundary asked for again,
/// and the desktop keeps the texels the next frames upload; once the surface holds the boundary the
/// message its draw wakes lets them go.
#[test]
fn gpu_preview_a_tick_during_the_upload_keeps_the_texels_and_names_it() {
    let catalog = catalog("uploading");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    let texels_held =
        |editor: &Editor| editor.gpu.summary()["drag"]["boundary"]["texels_held"].clone();
    let version = editor.gpu.held_version().unwrap();
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::BoundaryUploading {
            uploaded: 1,
            bytes: 2,
        }),
        drawn: None,
    });
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let records = logged(&mut editor, &log);
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "cpu");
    assert_eq!(ticks[0]["reason"], "boundary-uploading");
    assert_eq!(ticks[0]["boundary_requested"], false);
    assert_eq!(jobs(&records), 1, "the tick's job goes to the worker");
    assert_eq!(editor.gpu.ticks().2, 1, "asked once");
    assert_eq!(
        texels_held(&editor),
        json!(true),
        "the frames still upload them"
    );
    let plan = editor
        .surfaces()
        .gpu
        .expect("the plan is still handed over");
    assert!(plan.boundary.holds_texels());
    assert_eq!(plan.boundary.version(), version);
    surface_ready(&mut editor);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(texels_held(&editor), json!(false));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// While the slot a drag's plan replaces still retires, the surface refuses the new slot naming
/// the budget (`a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own`): each such
/// tick takes the CPU path naming `budget-exceeded`, its job going to the worker, and keeps the
/// boundary, its texels and the plan handed over, asking for nothing again; once the retirement
/// has ended and the surface holds the boundary, the next tick is drawn on the GPU. Nothing is
/// asked for, let go or allocated twice: the drag falls back for those ticks and does not thrash.
#[test]
fn gpu_preview_a_tick_the_budget_refuses_while_a_slot_retires_keeps_its_boundary() {
    let catalog = catalog("retiring");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    let texels_held =
        |editor: &Editor| editor.gpu.summary()["drag"]["boundary"]["texels_held"].clone();
    let version = editor.gpu.held_version().unwrap();
    let refused = SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::BudgetExceeded {
            requested: 600 << 20,
            in_use: 120 << 20,
            budget: 640 << 20,
        }),
        drawn: None,
    };
    for (tick, value) in [0.2, 0.3].into_iter().enumerate() {
        editor.gpu.surface = Some(refused);
        let log = attach_log(&mut editor);
        let _ = slide(&mut editor, ACTION, FIELD, value);
        let records = logged(&mut editor, &log);
        let ticks = events(&records, "gpu_preview_tick");
        let last = ticks.last().expect("a tick");
        assert_eq!(last["path"], "cpu", "tick {tick}");
        assert_eq!(last["reason"], "budget-exceeded", "tick {tick}");
        assert_eq!(last["boundary_requested"], false, "tick {tick}");
        assert_eq!(jobs(&records), 1, "tick {tick}: its job goes to the worker");
        assert!(
            events(&records, "gpu_boundary_released").is_empty(),
            "tick {tick}: the boundary is kept"
        );
        assert_eq!(texels_held(&editor), json!(true), "tick {tick}");
        let plan = editor
            .surfaces()
            .gpu
            .expect("the plan is still handed over");
        assert_eq!(plan.boundary.version(), version);
    }
    assert_eq!(editor.gpu.ticks().2, 1, "the boundary was asked for once");
    // The retirement ended and the next frame holds the slot.
    surface_ready(&mut editor);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    let records = logged(&mut editor, &log);
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks.last().expect("a tick")["path"], "gpu");
    assert_eq!(jobs(&records), 0, "the drawn tick sends no job");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}
