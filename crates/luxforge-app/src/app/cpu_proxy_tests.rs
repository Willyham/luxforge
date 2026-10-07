//! The CPU proxy (`super::cpu_proxy`), end to end against a real owner: a session the GPU does not
//! draw at all drags on the display-size proxy, at Fit and magnified at 100%, and the release
//! draws the reference's frame of the committed stack.
use super::{
    gpu_preview_tests::{catalog, deliver_until, zoomed},
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
    *,
};
use luxforge_gpu::GpuStageState;

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
fn a_session_without_a_gpu_drags_on_the_proxy_at_fit() {
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
fn a_session_without_a_gpu_drags_on_the_magnified_proxy_at_100_percent() {
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
