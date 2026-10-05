//! The settle hand-off, end to end against a real owner and preview worker: a GPU frame on screen
//! dissolves to the CPU frame of its content — the drafted settings settled on the CPU, or the
//! entry the draft committed — and never to older content; any input cancels a running dissolve,
//! and it ends after its 150 ms.
//!
//! No test draws: the surface's report of each frame is what a GPU tick would have drawn
//! (`GpuPreviews::surface`).
use super::{
    gpu_preview::SurfaceReport,
    message::{
        draft::DraftMessage, history::HistoryMessage, preview::PreviewMessage, view::ViewMessage,
    },
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
    *,
};
use luxforge_testbase::wait_until;
use luxforge_ui::photo_surface::{DISSOLVE_DURATION, GpuFallback as SurfaceFallback};

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-gpu-settle-{name}-{}.sqlite",
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

fn revision(editor: &Editor) -> u64 {
    editor
        .session
        .draft
        .as_ref()
        .expect("an open draft")
        .draft_revision
}

fn photo(editor: &Editor) -> Option<u64> {
    editor.surfaces().photo.map(luxforge_ui::Frame::version)
}

/// The surface's report after drawing the newest tick on the GPU, taken up by the next message as
/// the draw's wake would be.
fn drawn_on_the_gpu(editor: &mut Editor) {
    let version = editor.gpu.held_version().expect("a held boundary");
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: Some((version, revision(editor))),
        evaluated: None,
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
}

/// A Fit drag of exposure whose last ticks are drawn on the GPU: the boundary derived, the first
/// tick's CPU frame presented, the surface ready, and each tick of `values` drawn.
fn gpu_drag(editor: &mut Editor, values: &[f64]) {
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(editor, ACTION, FIELD, 0.1);
    assert!(editor.gpu.holds_boundary(), "derived at the tick");
    deliver_until(editor, "the first tick's frame", |editor| {
        !editor.presentation.queue.is_busy() && !editor.presentation.queue.ready()
    });
    let version = editor.gpu.held_version().expect("a held boundary");
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: None,
        evaluated: None,
    });
    for value in values {
        let _ = slide(editor, ACTION, FIELD, *value);
        assert!(
            editor.surfaces().gpu.is_some() && !editor.surfaces().gpu_hold,
            "the tick is drawn on the GPU"
        );
        drawn_on_the_gpu(editor);
    }
    assert!(
        editor.gpu_settle.dissolve().is_none(),
        "nothing settles yet"
    );
}

/// The release commits a stack the GPU does not draw at rest, and the committed entry's frame
/// replaces the GPU frame on screen through a dissolve from the drawn tick's revision to that
/// frame; it runs to its end, after which the next message lets it go.
#[test]
fn gpu_settle_a_commit_dissolves_from_the_gpu_frame_to_the_committed_frame() {
    let catalog = catalog("commit");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.rest_off = true;
    gpu_drag(&mut editor, &[0.2, 0.35]);
    let (drawn, boundary) = (revision(&editor), editor.gpu.held_version().unwrap());
    let behind = photo(&editor);
    let log = attach_log(&mut editor);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    // Until the committed frame arrives the GPU frame stays on screen.
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    deliver_until(&mut editor, "the committed frame", |editor| {
        editor.gpu_settle.dissolve().is_some()
    });
    let dissolve = editor.gpu_settle.dissolve().expect("a dissolve");
    assert_eq!(dissolve.from, drawn);
    assert_ne!(Some(dissolve.to), behind, "to a newer frame");
    assert_eq!(Some(dissolve.to), photo(&editor), "to the frame on screen");
    assert_eq!(editor.surfaces().dissolve, Some(dissolve));
    assert!(
        editor.surfaces().gpu.is_some() && editor.surfaces().gpu_hold,
        "the drag ended; its boundary stays resident, its plan held behind the CPU frame, and \
         the surface keeps its slot for the dissolve"
    );
    // Ended: the next message after its 150 ms lets it go.
    wait_until("the dissolve's duration", || {
        dissolve.started.elapsed() >= DISSOLVE_DURATION
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.gpu_settle.dissolve().is_none());
    assert!(editor.surfaces().dissolve.is_none());
    let records = logged(&mut editor, &log);
    let started = events(&records, "gpu_dissolve_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["case"], "committed");
    assert_eq!(started[0]["from"], json!(drawn));
    assert_eq!(started[0]["gpu_boundary"], json!(boundary));
    assert_eq!(events(&records, "gpu_dissolve_ended").len(), 1);
    assert_eq!(editor.gpu_settle.summary()["dissolves"], json!(1));
    finish(editor, catalog);
}

/// The release commits, and at rest the GPU draws the committed stack itself: its own view plan,
/// over the boundary the drag held, stands in for the CPU frame from the frame the drag lets go,
/// with no dissolve into the CPU frame behind it, the same programs over the same boundary as the
/// drag's last tick. A cancel puts the entry it was drawn over back the same way, its view plan
/// planned by its own committed job; a newer gesture takes the surface back for its own plan.
#[test]
fn gpu_settle_at_rest_the_gpu_draws_the_committed_stack_with_no_dissolve() {
    let catalog = catalog("at-rest");
    let (mut editor, _, _) = real_photo(&catalog);
    assert!(
        editor.gpu_rest_plan().is_some(),
        "the photograph's own view plan drawn at rest once it is open"
    );
    gpu_drag(&mut editor, &[0.2, 0.35]);
    assert!(
        editor.gpu_rest_plan().is_none(),
        "nothing at rest while a draft is open"
    );
    assert!(editor.surfaces().gpu_tag.is_some(), "the drag's own plan");
    let boundary = editor.gpu.held_version().unwrap();
    let log = attach_log(&mut editor);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the drag let go", |editor| {
        !editor.gpu.has_drag()
    });
    let surfaces = editor.surfaces();
    let (plan, _) = editor
        .gpu_rest_plan()
        .expect("the committed stack's view plan");
    assert!(
        std::ptr::eq(surfaces.gpu.unwrap(), plan) && !surfaces.gpu_hold,
        "drawn in place of the CPU frame"
    );
    assert_eq!(
        plan.boundary.version(),
        boundary,
        "over the boundary the drag held"
    );
    assert!(surfaces.gpu_tag.is_none(), "no gesture's revision");
    assert!(editor.gpu_settle.dissolve().is_none());
    assert_eq!(editor.displayed_picture(), "gpu");
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "gpu_dissolve_started").is_empty(),
        "{records:?}"
    );
    assert!(!events(&records, "gpu_at_rest").is_empty(), "{records:?}");
    // A cancel puts the entry back, its own view plan drawn the same way.
    gpu_drag(&mut editor, &[0.6]);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    deliver_until(&mut editor, "the drag let go", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
    let surfaces = editor.surfaces();
    assert!(
        editor.gpu_rest_plan().is_some() && surfaces.gpu.is_some() && !surfaces.gpu_hold,
        "the entry back on screen, drawn by the GPU"
    );
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "gpu_dissolve_started").is_empty(),
        "{records:?}"
    );
    finish(editor, catalog);
}

/// Compare draws both sides on the GPU at Fit: the GPU picture of the stack it began over is
/// retained as its After side, and the Before — the original entry — is drawn at rest by its own
/// committed job's view plan. Ending Compare hands the retained picture back at once, before the
/// stack's own job plans it again, so the photograph is the stack's GPU picture in the next frame.
#[test]
fn gpu_settle_compare_draws_both_sides_on_the_gpu_and_hands_after_back() {
    let catalog = catalog("compare");
    let (mut editor, asset, _) = real_photo(&catalog);
    gpu_drag(&mut editor, &[0.4]);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag() && !editor.presentation.queue.is_busy()
    });
    let (current, _) = editor
        .gpu_rest_plan()
        .expect("the current stack's view plan");
    let current = (current.boundary.version(), current.steps.len());
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::History(HistoryMessage::CompareToggle));
    assert!(
        editor.presentation.compare_after.is_some(),
        "{}",
        editor.status.text
    );
    let after = {
        let surfaces = editor.surfaces();
        let after = surfaces
            .compare_gpu
            .expect("the After side's retained view plan");
        (after.boundary.version(), after.steps.clone())
    };
    assert_eq!(
        (after.0, after.1.len()),
        current,
        "the GPU picture of the stack Compare began over"
    );
    // The Before's committed job, as Compare's own preview task plans it at the view's bounds.
    let refreshed = tasks::refresh(
        &editor.owner,
        editor.client,
        asset,
        tasks::Scope::Open,
        editor.proxy_bounds(),
    )
    .expect("the Before's job");
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: refreshed.job,
            session: refreshed.session,
        },
    )))));
    let (before, _) = editor.gpu_rest_plan().expect("the Before's view plan");
    assert_ne!(
        before.steps, after.1,
        "the original entry's own plan, not the current stack's"
    );
    let surfaces = editor.surfaces();
    assert!(
        surfaces.gpu.is_some() && !surfaces.gpu_hold && surfaces.compare_gpu.is_some(),
        "both sides drawn by the GPU"
    );
    // Ending Compare hands the retained picture back to the photograph at once.
    let _ = editor.update(Message::History(HistoryMessage::CompareExit));
    let (back, _) = editor
        .gpu_rest_plan()
        .expect("the stack's view plan, at once");
    assert_eq!((back.boundary.version(), back.steps.len()), current);
    assert!(editor.surfaces().compare_gpu.is_none());
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "gpu_compare_retained").len(),
        1,
        "{records:?}"
    );
    assert_eq!(
        events(&records, "gpu_compare_restored").len(),
        1,
        "{records:?}"
    );
    finish(editor, catalog);
}

/// A tick the surface cannot draw sends its job to the worker, and that CPU frame of a newer
/// revision replaces the GPU frame through a dissolve, the plan held behind it; the next tick,
/// drawn on the GPU, is an input that cancels it.
#[test]
fn gpu_settle_a_cpu_frame_of_the_draft_dissolves_and_the_next_tick_cancels_it() {
    let catalog = catalog("held");
    let (mut editor, _, _) = real_photo(&catalog);
    gpu_drag(&mut editor, &[0.2, 0.35]);
    let drawn = revision(&editor);
    let version = editor.gpu.held_version().unwrap();
    let log = attach_log(&mut editor);
    // The surface could not draw the next tick: it goes to the worker.
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(SurfaceFallback::Compiling),
        drawn: Some((version, drawn)),
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.5);
    let newer = revision(&editor);
    deliver_until(&mut editor, "the drafted frame", |editor| {
        editor.gpu_settle.dissolve().is_some()
    });
    let dissolve = editor.gpu_settle.dissolve().unwrap();
    assert_eq!(dissolve.from, drawn);
    assert_eq!(
        editor.presentation.displayed_draft_revision,
        Some(newer),
        "never to an older frame"
    );
    assert!(editor.surfaces().gpu_hold, "the plan is held behind it");
    assert_eq!(editor.surfaces().dissolve, Some(dissolve));
    // The next tick, drawn on the GPU, cancels it.
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: Some((version, drawn)),
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    assert!(editor.gpu_settle.dissolve().is_none());
    assert!(editor.surfaces().dissolve.is_none());
    assert!(editor.surfaces().gpu.is_some() && !editor.surfaces().gpu_hold);
    let records = logged(&mut editor, &log);
    let started = events(&records, "gpu_dissolve_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["case"], "held");
    let cancelled = events(&records, "gpu_dissolve_cancelled");
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0]["why"], "input");
    assert_eq!(editor.gpu_settle.summary()["cancelled"], json!(1));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A cancelled draft puts the entry it was drawn over back on screen with no dissolve: the CPU's
/// frame of it, where the GPU does not draw it at rest, which stood behind the GPU frame all along,
/// its pixels reused.
#[test]
fn gpu_settle_a_cancel_swaps_with_no_dissolve() {
    let catalog = catalog("cancel");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.rest_off = true;
    gpu_drag(&mut editor, &[0.2, 0.35]);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    deliver_until(&mut editor, "the frame read back", |editor| {
        !editor.gpu.has_drag()
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.gpu_settle.dissolve().is_none());
    let records = logged(&mut editor, &log);
    assert!(events(&records, "gpu_dissolve_started").is_empty());
    assert_eq!(
        events(&records, "preview_pixels_reused").len(),
        1,
        "{records:?}"
    );
    let surfaces = editor.surfaces();
    assert!(
        (surfaces.gpu.is_none() || surfaces.gpu_hold) && surfaces.photo.is_some(),
        "the CPU frame of the entry is the photograph"
    );
    finish(editor, catalog);
}

/// A clipping overlay turned on while the committed frame of a stack the GPU does not draw at rest
/// dissolves in is an input too: it cancels the dissolve, so the CPU frame's own overlay is drawn
/// at once rather than after it.
#[test]
fn gpu_settle_a_clipping_toggle_cancels_the_dissolve() {
    let catalog = catalog("view");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.rest_off = true;
    gpu_drag(&mut editor, &[0.2, 0.35]);
    let log = attach_log(&mut editor);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed frame", |editor| {
        editor.gpu_settle.dissolve().is_some()
    });
    editor.session.workspace.clip_highlights = true;
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.gpu_settle.dissolve().is_none());
    assert!(editor.surfaces().dissolve.is_none());
    let records = logged(&mut editor, &log);
    let cancelled = events(&records, "gpu_dissolve_cancelled");
    assert_eq!(cancelled.len(), 1, "{records:?}");
    assert_eq!(cancelled[0]["why"], "view");
    // The view change is recorded apart from the dissolve, as the scenario reads it.
    let views = events(&records, "gpu_settle_view");
    assert_eq!(views.len(), 1, "{records:?}");
    assert_eq!(views[0]["clipping"], json!([false, true]));
    editor.session.workspace.clip_highlights = false;
    finish(editor, catalog);
}

/// The photograph at 400%, where the window shows part of it, once its first frame is on screen:
/// the session's zoom on the owner, as the zoom field sets it, and as the desktop adopts it.
fn zoomed(editor: &mut Editor) {
    deliver_until(editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    luxforge_testkit::client::call(
        &editor.owner,
        editor.client,
        "view.set",
        json!({"zoom": {"mode": "percent", "value": 400.0}}),
    )
    .expect("the zoom");
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 400.0 };
}

/// The version of the CPU frame the percentage view draws the current content from: its whole
/// frame of that content, or else its region of it.
fn view_frame(editor: &Editor) -> Option<u64> {
    let surfaces = editor.surfaces();
    if surfaces.photo_content == Some(surfaces.current_content) {
        surfaces.photo.map(luxforge_ui::Frame::version)
    } else {
        surfaces
            .region
            .filter(|region| region.content_id == surfaces.current_content)
            .map(|region| region.frame.version())
    }
}

/// At a percentage zoom the release's committed frame of the visible region replaces the GPU
/// region frame through a dissolve, into the frame the view draws the committed content from; a
/// pan while it runs is a view input that cancels it.
#[test]
fn gpu_settle_at_a_percentage_zoom_a_commit_dissolves_into_the_views_frame_and_a_pan_cancels_it() {
    let catalog = catalog("percent-commit");
    let (mut editor, _, _) = real_photo(&catalog);
    zoomed(&mut editor);
    gpu_drag(&mut editor, &[0.2, 0.35]);
    assert!(
        editor
            .surfaces()
            .gpu
            .is_some_and(|plan| plan.region.is_some()),
        "a region plan"
    );
    let (drawn, boundary) = (revision(&editor), editor.gpu.held_version().unwrap());
    let behind = view_frame(&editor);
    let log = attach_log(&mut editor);
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed region", |editor| {
        editor.gpu_settle.dissolve().is_some()
    });
    let dissolve = editor.gpu_settle.dissolve().expect("a dissolve");
    assert_eq!(dissolve.from, drawn);
    assert_ne!(Some(dissolve.to), behind, "to a newer frame");
    assert_eq!(
        Some(dissolve.to),
        view_frame(&editor),
        "to the frame the view draws"
    );
    assert_eq!(editor.surfaces().dissolve, Some(dissolve));
    // A pan while it runs cancels it.
    let (x, y) = editor.view_state.local_pan;
    let _ = editor.update(Message::View(ViewMessage::Panned(x + 40.0, y + 40.0)));
    assert!(editor.gpu_settle.dissolve().is_none());
    assert!(editor.surfaces().dissolve.is_none());
    let records = logged(&mut editor, &log);
    let started = events(&records, "gpu_dissolve_started");
    assert_eq!(started.len(), 1, "{records:?}");
    assert_eq!(started[0]["case"], "committed");
    assert_eq!(started[0]["gpu_boundary"], json!(boundary));
    let cancelled = events(&records, "gpu_dissolve_cancelled");
    assert_eq!(cancelled.len(), 1, "{records:?}");
    assert_eq!(cancelled[0]["why"], "view");
    finish(editor, catalog);
}

/// Below 100% the view draws the displayed-size proxy of the whole stage, a whole frame as at Fit:
/// the release's committed proxy frame of a stack the GPU does not draw at rest replaces the drag's
/// GPU frame through a dissolve from the drawn tick's revision into that frame, and a pan while it
/// runs is a view input that cancels it.
#[test]
fn gpu_settle_below_100_percent_a_commit_dissolves_into_the_proxy_and_a_pan_cancels_it() {
    for value in [50.0, 33.0] {
        let catalog = catalog(&format!("below-{value}"));
        let (mut editor, _, _) = real_photo(&catalog);
        editor.gpu.rest_off = true;
        super::gpu_preview_tests::zoomed_out(&mut editor, value);
        gpu_drag(&mut editor, &[0.2, 0.35]);
        assert!(
            editor
                .surfaces()
                .gpu
                .is_some_and(|plan| plan.region.is_none()),
            "{value}%: a whole frame's plan"
        );
        let (drawn, boundary) = (revision(&editor), editor.gpu.held_version().unwrap());
        let behind = photo(&editor);
        let log = attach_log(&mut editor);
        let _ = let_go(&mut editor, ACTION, FIELD);
        assert!(run_commit(&mut editor));
        deliver_until(&mut editor, "the committed proxy frame", |editor| {
            editor.gpu_settle.dissolve().is_some()
        });
        let dissolve = editor.gpu_settle.dissolve().expect("a dissolve");
        assert_eq!(dissolve.from, drawn, "{value}%");
        assert_ne!(Some(dissolve.to), behind, "{value}%: to a newer frame");
        assert_eq!(
            Some(dissolve.to),
            photo(&editor),
            "{value}%: to the proxy the view draws"
        );
        assert!(
            editor.presentation.presented_reduced,
            "{value}%: the committed frame is the view's reduction"
        );
        assert_eq!(editor.surfaces().dissolve, Some(dissolve));
        // A pan while it runs cancels it.
        let (x, y) = editor.view_state.local_pan;
        let _ = editor.update(Message::View(ViewMessage::Panned(x + 40.0, y + 40.0)));
        assert!(editor.gpu_settle.dissolve().is_none(), "{value}%");
        let records = logged(&mut editor, &log);
        let started = events(&records, "gpu_dissolve_started");
        assert_eq!(started.len(), 1, "{records:?}");
        assert_eq!(started[0]["case"], "committed");
        assert_eq!(started[0]["gpu_boundary"], json!(boundary));
        let cancelled = events(&records, "gpu_dissolve_cancelled");
        assert_eq!(cancelled.len(), 1, "{records:?}");
        assert_eq!(cancelled[0]["why"], "view");
        finish(editor, catalog);
    }
}

/// At a percentage zoom a tick the surface cannot draw sends its region job to the worker, and
/// that region of the newer revision replaces the GPU region frame through a dissolve with the
/// plan held behind it; a change of zoom cancels it, and a dissolve that runs to its end is let go
/// by the next message.
#[test]
fn gpu_settle_at_a_percentage_zoom_the_drafts_region_dissolves_and_a_zoom_cancels_it() {
    let catalog = catalog("percent-held");
    let (mut editor, _, _) = real_photo(&catalog);
    zoomed(&mut editor);
    gpu_drag(&mut editor, &[0.2, 0.35]);
    let drawn = revision(&editor);
    let version = editor.gpu.held_version().unwrap();
    let held_tick = |editor: &mut Editor, value: f64| {
        editor.gpu.surface = Some(SurfaceReport {
            ready_boundary: None,
            fallback: Some(SurfaceFallback::Compiling),
            drawn: Some((version, drawn)),
            evaluated: None,
        });
        let _ = slide(editor, ACTION, FIELD, value);
        deliver_until(editor, "the drafted region", |editor| {
            editor.gpu_settle.dissolve().is_some()
        });
    };
    held_tick(&mut editor, 0.5);
    let dissolve = editor.gpu_settle.dissolve().unwrap();
    assert_eq!(dissolve.from, drawn);
    assert_eq!(Some(dissolve.to), view_frame(&editor));
    assert!(editor.surfaces().gpu_hold, "the plan is held behind it");
    // A change of zoom cancels it.
    let log = attach_log(&mut editor);
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 800.0 };
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.gpu_settle.dissolve().is_none());
    let records = logged(&mut editor, &log);
    let cancelled = events(&records, "gpu_dissolve_cancelled");
    assert_eq!(cancelled.len(), 1, "{records:?}");
    assert_eq!(cancelled[0]["why"], "view");
    let views = events(&records, "gpu_settle_view");
    assert_eq!(views.len(), 1, "{records:?}");
    assert_eq!(views[0]["zoom"], json!(800.0));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}
