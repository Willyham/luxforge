//! A drag tick the GPU does not draw (`super::motion`), end to end against a real owner: a tick
//! that waits for its boundary holds the frame on screen and queues nothing, ending when the
//! surface draws it or rendering the reference frame once it has lasted half a second; a tick the
//! GPU cannot draw for a reason that lasts has the reference draw one whole frame at a time, the
//! newest tick's next; and a pan during a paused drag at 100% plans the drag's region again.
use super::{
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, deliver_until, surface_ready, zoomed},
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
    testing::{attach_log, events, finish, logged, real_photo, slide},
    *,
};
use std::time::Duration;

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

/// How many jobs went to the preview worker.
fn jobs(records: &[Value]) -> usize {
    events(records, "preview_job_requested").len()
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

/// A drag the GPU cannot draw for a reason that lasts — a Perspective drag over a stack with no
/// layer before the geometry, `boundary-stage` — has the reference draw one whole frame at a time:
/// the first tick's at once, and each later tick's waiting for it, the newest replacing the older,
/// so the frame that lands last is the newest revision's. The status bar names the layer.
#[test]
fn motion_a_lasting_reason_renders_one_reference_frame_at_a_time_the_newest_next() {
    let catalog = catalog("motion-reference");
    let (mut editor, _, _) = real_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(&mut editor);
    for value in [20.0, 30.0, 40.0] {
        let _ = slide(&mut editor, "set-perspective", "horizontal", value);
    }
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    assert_eq!(
        editor.gpu_plan_fallback().as_deref(),
        Some("boundary-stage")
    );
    assert!(editor.drag_frame_waiting(), "the newest tick waits");
    let first = logged(&mut editor, &log);
    assert_eq!(jobs(&first), 1, "one frame in flight");
    let ticks = events(&first, "gpu_preview_tick");
    assert_eq!(ticks.len(), 3);
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "boundary-stage")
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
        Some("Not on the GPU: Perspective")
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
    let super::gpu_preview::GpuAsk::Region(rect, magnification) = editor.gpu_ask() else {
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
