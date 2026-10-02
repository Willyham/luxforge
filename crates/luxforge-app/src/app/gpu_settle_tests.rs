//! The settle hand-off, end to end against a real owner and preview worker: a GPU frame on screen
//! dissolves to the CPU frame of its content — the drafted settings settled on the CPU, or the
//! entry the draft committed — and never to older content; any input cancels a running dissolve,
//! and it ends after its 150 ms.
//!
//! No test draws: the surface's report of each frame is what a GPU tick would have drawn
//! (`GpuPreviews::surface`).
use super::{
    gpu_preview::SurfaceReport,
    message::{draft::DraftMessage, preview::PreviewMessage, view::ViewMessage},
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
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
}

/// A Fit drag of exposure whose last ticks are drawn on the GPU: the boundary asked for and held,
/// the surface ready, and each tick of `values` drawn.
fn gpu_drag(editor: &mut Editor, values: &[f64]) {
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(editor, ACTION, FIELD, 0.1);
    deliver_until(editor, "the boundary", |editor| editor.gpu.holds_boundary());
    let version = editor.gpu.held_version().expect("a held boundary");
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(version),
        fallback: None,
        drawn: None,
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

/// The release commits, and the committed entry's frame replaces the GPU frame on screen through
/// a dissolve from the drawn tick's revision to that frame; it runs to its end, after which the
/// next message lets it go.
#[test]
fn gpu_settle_a_commit_dissolves_from_the_gpu_frame_to_the_committed_frame() {
    let catalog = catalog("commit");
    let (mut editor, _, _) = real_photo(&catalog);
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
        editor.surfaces().gpu.is_none(),
        "the drag let go; the surface keeps its slot for the dissolve"
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

/// A cancelled draft puts the entry it was drawn over back on screen: older content, which
/// replaces the GPU frame with no dissolve.
#[test]
fn gpu_settle_a_cancel_swaps_with_no_dissolve() {
    let catalog = catalog("cancel");
    let (mut editor, _, _) = real_photo(&catalog);
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
    let swapped = events(&records, "gpu_settle_swapped");
    assert_eq!(swapped.len(), 1, "{records:?}");
    assert_eq!(swapped[0]["why"], "older content");
    finish(editor, catalog);
}

/// A clipping overlay turned on while the committed frame dissolves in is an input too: it cancels
/// the dissolve, so the CPU frame's own overlay is drawn at once rather than after it.
#[test]
fn gpu_settle_a_clipping_toggle_cancels_the_dissolve() {
    let catalog = catalog("view");
    let (mut editor, _, _) = real_photo(&catalog);
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
