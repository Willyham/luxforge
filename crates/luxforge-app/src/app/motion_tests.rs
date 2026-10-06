//! A drag tick the GPU does not draw (`super::motion`), end to end against a real owner: a tick
//! that waits for its boundary holds the frame on screen and queues nothing, ending when the
//! surface draws it or rendering the reference frame once it has lasted half a second; a tick the
//! GPU cannot draw for a reason that lasts has the reference draw one whole frame at a time, the
//! newest tick's next; a pan during a paused drag at 100% plans the drag's region again; and a
//! session the GPU does not draw at all drags on the display-size proxy, which a session with a GPU
//! never asks for.
use super::{
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, deliver_until, surface_ready, zoomed},
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
    *,
};
use luxforge_ui::photo_surface::GpuStageState;
use std::time::Duration;

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

/// How many jobs went to the preview worker.
fn jobs(records: &[Value]) -> usize {
    events(records, "preview_job_requested").len()
}

/// How many of them asked for a drag's display-size proxy.
fn proxy_jobs(records: &[Value]) -> usize {
    events(records, "preview_job_requested")
        .iter()
        .filter(|job| job["interactive"] == true)
        .count()
}

/// The surface reports that its last frame drew the open draft's newest tick over the boundary
/// held for it.
fn surface_drew_newest(editor: &mut Editor) {
    let version = editor.gpu.held_version().expect("a held boundary");
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: Some((version, revision)),
        evaluated: None,
    });
}

/// A drag's first tick waits for the surface to evaluate its plan (`surface-pending`): it holds
/// the frame on screen, queues no whole frame, and ends when the surface draws it, which is the
/// gesture's frame.
#[test]
fn motion_a_held_tick_ends_when_the_surface_draws_it() {
    let catalog = catalog("motion-drawn");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    assert!(editor.drag_frame_waiting(), "the first tick holds");
    assert!(editor.motion_hold_pending(), "and is looked at again");
    surface_drew_newest(&mut editor);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.drag_frame_waiting(), "drawn on the GPU");
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "no whole frame is queued");
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["path"], "held");
    assert_eq!(ticks[0]["reason"], "surface-pending");
    let ended = events(&records, "gpu_preview_hold_ended");
    assert_eq!(ended.len(), 1, "{records:?}");
    assert_eq!(ended[0]["why"], "drawn");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A hold that lasts the half-second threshold, from the first tick of its run, renders the newest
/// held tick's reference frame: one job, of the newest revision, which reaches the screen.
#[test]
fn motion_a_hold_that_lasts_renders_the_newest_ticks_reference_frame() {
    let catalog = catalog("motion-lasted");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    assert!(editor.drag_frame_waiting());
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.drag_frame_waiting(), "short of the threshold");
    editor.backdate_hold(Duration::from_millis(600));
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.motion_hold_pending(), "the hold is over");
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 1, "one reference frame, the newest tick's");
    assert_eq!(
        proxy_jobs(&records),
        0,
        "a session with a GPU asks for no proxy"
    );
    let ended = events(&records, "gpu_preview_hold_ended");
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["why"], "lasted");
    let queued = events(&records, "gpu_preview_reference");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["draft_revision"], json!(revision));
    deliver_until(&mut editor, "the newest tick's frame", |editor| {
        editor.presentation.displayed_draft_revision == Some(revision)
    });
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A drag the GPU cannot draw for a reason that lasts — a 100% slot past the GPU-preview budget
/// even at its reduced stage, `budget-exceeded` — has the reference draw one whole frame at a time:
/// the first tick's at once, and each later tick's waiting for it, the newest replacing the older,
/// so the frame that lands last is the newest revision's. The status bar names the budget.
#[test]
fn motion_a_lasting_reason_renders_one_reference_frame_at_a_time_the_newest_next() {
    let catalog = catalog("motion-reference");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    zoomed(&mut editor);
    editor.gpu.budget = Some(1);
    let log = attach_log(&mut editor);
    for value in [0.1, 0.2, 0.3] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
    }
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    assert_eq!(
        editor.gpu_plan_fallback().as_deref(),
        Some("budget-exceeded")
    );
    assert!(editor.drag_frame_waiting(), "the newest tick waits");
    let first = logged(&mut editor, &log);
    assert_eq!(jobs(&first), 1, "one frame in flight");
    assert_eq!(proxy_jobs(&first), 0, "a whole frame, never a proxy");
    let ticks = events(&first, "gpu_preview_tick");
    assert_eq!(ticks.len(), 3);
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "budget-exceeded")
    );
    assert_eq!(
        ticks.iter().filter(|tick| tick["waits"] == true).count(),
        2,
        "the later ticks wait behind it"
    );
    let log = attach_log(&mut editor);
    deliver_until(&mut editor, "the newest tick's frame", |editor| {
        editor.presentation.displayed_draft_revision == Some(revision)
            && !editor.presentation.queue.is_busy()
    });
    let later = logged(&mut editor, &log);
    assert_eq!(
        jobs(&later),
        1,
        "the second tick's job was replaced by the third's"
    );
    assert!(!editor.drag_frame_waiting());
    editor.rederive();
    assert_eq!(
        editor
            .workspace
            .status
            .fallback
            .as_ref()
            .map(|notice| notice.phrase.as_str()),
        Some("GPU memory full")
    );
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// At 100% a pan during a paused drag past the region its last tick drew plans the view again as
/// the drag's next tick: the drag's GPU plan over the new region, held until the surface draws
/// it, with no whole frame queued; at Fit nothing is planned.
#[test]
fn motion_a_pan_during_a_paused_drag_plans_its_region_again() {
    let catalog = catalog("motion-pan");
    let (mut editor, asset, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let first = zoomed(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert!(editor.gpu_draws_view(first), "drawn on the GPU");
    let ticks = editor.gpu.ticks();
    // The drag pauses and the view pans to the photograph's far corner.
    let _ = editor.update(Message::View(ViewMessage::Panned(1400.0, 900.0)));
    let stage = editor.presentation.dimensions.unwrap();
    let second = editor.desired_view_for(stage).expect("a visible region");
    assert!(!super::preview::contains_region(first, second));
    assert!(
        editor.view_plan.in_flight,
        "the paused drag's view is planned"
    );
    // The owner's answer: the draft over the new region with its GPU plan.
    let super::gpu_preview::GpuAsk::Region(rect, magnification, _) = editor.gpu_ask() else {
        panic!("a region at 400%");
    };
    let draft = editor.session.draft.as_ref().unwrap().draft_id.clone();
    let job = super::tasks::ready_preview_job(
        &editor.owner,
        luxforge_core::PreviewRequest::new(editor.client, asset)
            .entry(editor.displayed_entry())
            .analyse()
            .draft(draft)
            .gpu_region(rect, magnification),
    )
    .expect("the view's job");
    let log = attach_log(&mut editor);
    let epoch = editor.view_plan.epoch;
    let _ = editor.update(Message::Preview(PreviewMessage::ViewLoaded {
        epoch,
        result: Ok(Box::new(job)),
    }));
    let records = logged(&mut editor, &log);
    assert_eq!(jobs(&records), 0, "no whole frame for a pan");
    assert_eq!(editor.gpu.ticks().1, ticks.1 + 1, "one more tick");
    assert_eq!(
        editor.gpu.summary()["drag"]["boundary"]["region"],
        json!([second.x0, second.y0, second.width, second.height])
    );
    surface_drew_newest(&mut editor);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.gpu_draws_view(second), "the new region on the GPU");
    assert!(!editor.drag_frame_waiting());
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// At 100% over the GPU's picture of the view, a mask's coverage is a grid of the region the GPU
/// draws, laid over that region, and keyed on it: a pan to another region asks for another grid.
#[test]
fn motion_mask_coverage_at_100_percent_is_a_grid_of_the_gpus_region() {
    let catalog = catalog("motion-coverage");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let first = zoomed(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    let region = editor.gpu_view_region().expect("the GPU draws the view");
    assert_eq!(region.rect, first);
    assert_eq!(region.stage, editor.presentation.dimensions.unwrap());
    assert_eq!(region.content, editor.presentation.presented_content);
    // Below 100% there is no region: the coverage is the whole stage's.
    editor.session.preview.view.zoom = luxforge_core::Zoom::Fit;
    assert!(editor.gpu_view_region().is_none());
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A photograph in a session the GPU does not draw at all, its photo surface smaller than the
/// 480 × 320 photograph so that Fit draws it smaller than it is: Fit's bounds.
fn without_a_gpu(editor: &mut Editor) -> luxforge_core::ProxyBounds {
    editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: true });
    editor.session.workspace.state_panel = false;
    editor.session.workspace.tools_panel = false;
    editor.view_state.window = (360.0, 300.0);
    let bounds = editor.fit_bounds().expect("Fit's bounds");
    assert!(bounds.width < 480 && bounds.height < 320, "{bounds:?}");
    bounds
}

/// Drag Basic's exposure through `values` and take up frames until the newest tick's is on
/// screen: the drag's records.
fn proxied_drag(editor: &mut Editor, values: &[f64]) -> Vec<Value> {
    let log = attach_log(editor);
    for value in values {
        let _ = slide(editor, ACTION, FIELD, *value);
        assert!(!editor.drag_frame_waiting(), "no tick holds or waits");
    }
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    deliver_until(editor, "the newest tick's proxy", |editor| {
        editor.presentation.displayed_draft_revision == Some(revision)
            && !editor.presentation.queue.is_busy()
    });
    logged(editor, &log)
}

/// Release the drag and take up the reference's frame of the committed stack.
fn release(editor: &mut Editor) {
    let _ = let_go(editor, ACTION, FIELD);
    assert!(run_commit(editor));
    deliver_until(editor, "the committed frame", |editor| {
        editor.presentation.displayed_draft_revision.is_none()
            && editor.presentation.presented_proxy.is_none()
            && !editor.presentation.queue.is_busy()
    });
}

/// In a session the GPU does not draw at all, a drag at Fit asks the reference for each tick's
/// display-size proxy, latest winning, and holds no tick: the frame on screen is the proxy at Fit's
/// bounds, labelled approximate. The release draws the reference's frame of the committed stack,
/// its exact frame reduced to the view.
#[test]
fn motion_a_session_without_a_gpu_drags_on_the_proxy_at_fit() {
    let catalog = catalog("motion-proxy-fit");
    let (mut editor, _, _) = real_photo(&catalog);
    let bounds = without_a_gpu(&mut editor);
    let records = proxied_drag(&mut editor, &[0.1, 0.2, 0.3]);
    assert_eq!(jobs(&records), 3, "one job a tick");
    assert_eq!(proxy_jobs(&records), 3, "each the tick's proxy");
    assert!(
        events(&records, "gpu_preview_tick")
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "no-adapter"),
        "{records:?}"
    );
    assert!(editor.presentation.presented_proxy.is_some());
    assert!(editor.presentation.presented_reduced);
    assert_eq!(editor.presentation.dimensions, Some((480, 320)));
    let (width, height) = editor
        .presentation
        .presenter
        .photo()
        .expect("a frame")
        .size();
    assert!(
        width <= bounds.width && height <= bounds.height,
        "the proxy is display-sized: {width} × {height} within {bounds:?}"
    );
    assert!(editor.surfaces().whole_frame());
    assert!(editor.activity.render.expect("a render time").approximate);
    let displayed = events(&records, "preview_displayed");
    assert_eq!(displayed.last().expect("a frame")["proxy"], true);
    release(&mut editor);
    assert!(!editor.activity.render.expect("a render time").approximate);
    finish(editor, catalog);
}

/// In a session the GPU does not draw at all, a drag at 400% asks for each tick's proxy at Fit's
/// bounds and plans no view of its own: the view draws the proxy whole, magnified to the stage's
/// box. The release draws the reference's exact frame of the committed stack, sharp.
#[test]
fn motion_a_session_without_a_gpu_drags_on_the_magnified_proxy_at_100_percent() {
    let catalog = catalog("motion-proxy-zoom");
    let (mut editor, _, _) = real_photo(&catalog);
    let bounds = without_a_gpu(&mut editor);
    zoomed(&mut editor);
    let records = proxied_drag(&mut editor, &[0.1, 0.2]);
    assert_eq!(jobs(&records), 2, "one job a tick, and none for the view");
    assert_eq!(proxy_jobs(&records), 2);
    assert!(!editor.view_plan.in_flight, "no view is planned");
    assert!(editor.presentation.presented_proxy.is_some());
    let (width, height) = editor
        .presentation
        .presenter
        .photo()
        .expect("a frame")
        .size();
    assert!(
        width <= bounds.width && height <= bounds.height,
        "Fit's proxy: {width} × {height} within {bounds:?}"
    );
    assert!(
        editor.surfaces().whole_frame(),
        "drawn whole, magnified to the stage's box"
    );
    release(&mut editor);
    assert_eq!(
        editor
            .presentation
            .presenter
            .photo()
            .expect("a frame")
            .size(),
        (480, 320),
        "the exact frame at rest"
    );
    assert!(!editor.presentation.presented_reduced);
    assert!(!editor.surfaces().whole_frame());
    finish(editor, catalog);
}
