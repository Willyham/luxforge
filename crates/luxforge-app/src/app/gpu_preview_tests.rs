//! A gesture drawn on the GPU, end to end against a real owner and preview worker: one boundary
//! job at draft begin and no preview job per tick once the boundary is held and the surface has
//! evaluated it; CPU frames until then and whenever the surface cannot draw; the boundary released
//! at commit, cancel and a key change; and an ineligible stack's reason.
use super::{
    gpu_preview::SurfaceReport,
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
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

/// An ineligible drag takes the CPU path and says why: at a percentage zoom no GPU preview is
/// planned at all.
#[test]
fn gpu_preview_an_ineligible_drag_names_its_reason() {
    let catalog = catalog("ineligible");
    let (mut editor, _, _) = real_photo(&catalog);
    // The session the owner answered a 100% zoom with.
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
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
