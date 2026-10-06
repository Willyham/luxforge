//! The GPU's histogram and clipping counts against a real owner and preview worker
//! (`docs/design/gpu-first.md`, stage 2): a committed stack the GPU draws at rest is presented with
//! no CPU render and its tiles' counts become its report, plotted and in the owner's store; counts
//! that fail send the content to the reference renderer; a gesture's ticks plot the counts of the
//! frame each drew, as updating; and before the surface has checked its stage the reference renders
//! as before. The surface is no test's: what it reports is stood in for.
use super::*;
use crate::app::{
    Message,
    gpu_preview_tests::{catalog, deliver_until, surface_ready},
    message::preview::PreviewMessage,
    tasks::call,
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
};
use luxforge_ui::photo_surface::{
    GpuStageState, SurfaceCounts, TickCounts,
    gpu_preview::histogram::{Counts, HistogramError},
};

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

/// Counts of a `width × height` frame whose every pixel is the one code `code`, as the surface
/// reads them back.
fn uniform(code: u8, (width, height): (u32, u32)) -> Counts {
    let pixels = u64::from(width) * u64::from(height);
    let mut bins = [0; 256];
    bins[usize::from(code)] = pixels;
    let at = |end: u8| if code == end { pixels } else { 0 };
    Counts {
        r: bins,
        g: bins,
        b: bins,
        r0: at(0),
        g0: at(0),
        b0: at(0),
        r255: at(255),
        g255: at(255),
        b255: at(255),
        any_shadow: at(0),
        any_highlight: at(255),
        all_shadow: at(0),
        all_highlight: at(255),
        both: 0,
        pixels,
    }
}

/// An editor over the real photograph, its open's reference frame presented, its surface's GPU
/// stage checked and able.
fn opened_on_the_gpu(name: &str) -> (Editor, std::path::PathBuf) {
    let catalog = catalog(name);
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the open's reference frame", |editor| {
        editor.presentation.presenter.photo().is_some() && editor.presentation.analysis.is_some()
    });
    assert_eq!(
        editor
            .presentation
            .shown_analysis()
            .map(|analysis| analysis.source),
        Some(AnalysisSource::Reference),
        "an open is the reference's: no frame of the photograph was on screen to draw over"
    );
    editor.renderer.stage = Some(GpuStageState::Available);
    (editor, catalog)
}

/// Commit an exposure drag, and the committed stack's job with it.
fn commit(editor: &mut Editor, value: f64) {
    let _ = slide(editor, ACTION, FIELD, value);
    let _ = let_go(editor, ACTION, FIELD);
    assert!(run_commit(editor));
}

/// A zoom from Fit to 100% over a commit the GPU presented, with no CPU frame of it, keeps drawing
/// the GPU's whole-frame plan of that stack while the view's region is planned: the frame under it
/// is the open's, an earlier stack's, which must never stand in for the committed one.
#[test]
fn a_zoom_to_100_over_a_stack_the_gpu_presented_keeps_its_plan_until_the_region() {
    let (mut editor, catalog) = opened_on_the_gpu("zoom-presented");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.gpu_presented == Some(editor.presentation.presented_content)
            && editor.gpu_rest_plan().is_some()
    });
    let (fit, _) = editor.gpu_rest_plan().expect("the Fit view plan");
    assert!(fit.region.is_none(), "a whole frame's plan at Fit");
    let boundary = fit.boundary.version();
    // The session's answer to the zoom, as the desktop takes it up.
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&luxforge_core::Zoom::Fit);
    let surfaces = editor.surfaces();
    let (shown, _) = editor
        .gpu_rest_plan()
        .expect("the committed stack's plan is still the picture");
    assert!(
        std::ptr::eq(surfaces.gpu.expect("a plan drawn"), shown),
        "the GPU's plan is drawn in place of the earlier stack's frame"
    );
    if shown.region.is_none() {
        assert_eq!(shown.boundary.version(), boundary, "the Fit plan, kept");
    }
    finish(editor, catalog);
}

/// A commit the GPU draws at rest is presented with no preview job, so no exact frame is rendered
/// or reduced; its tiles are handed for their counts, which, read back, are its report: plotted as
/// the GPU's, current, and in the owner's store under the committed stack's identity, so an API
/// client's `analysis.request` for it is answered at once, with exactly them.
#[test]
fn a_commit_the_gpu_draws_renders_no_cpu_frame_and_its_tiles_counts_are_its_report() {
    let (mut editor, catalog) = opened_on_the_gpu("counts");
    let log = attach_log(&mut editor);
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.gpu.counts.is_some()
    });
    let records = logged(&mut editor, &log);
    let displayed = events(&records, "preview_displayed");
    let last = displayed.last().expect("the commit presented");
    assert_eq!(last["path"], "gpu", "{last}");
    let target = editor.gpu.counts.clone().expect("its counts wanted");
    let content = editor.presentation.content_serial;
    assert_eq!(editor.presentation.gpu_presented, Some(content));
    assert_eq!(target.content, content);
    assert!(
        events(&records, "preview_job_requested")
            .iter()
            .all(|job| job["generation"].as_u64() < Some(target.generation)),
        "no job after the commit's: {records:?}"
    );
    deliver_until(&mut editor, "the queue idle", |editor| {
        !editor.presentation.queue.is_busy()
    });
    assert!(
        editor.presentation.analysis_updating(),
        "its counts are to come"
    );
    assert!(editor.gpu_counts_pending());
    let handed = editor.surfaces();
    let tiles = handed
        .gpu_rest
        .or(handed.gpu_counts)
        .expect("its tiles handed");
    assert!(target.versions.contains(&tiles.version));
    // The surface reads its tiles' counts back.
    let size = (target.identity.width, target.identity.height);
    editor.gpu.counts_report = Some(SurfaceCounts {
        rest: Some((
            tiles.version,
            luxforge_ui::photo_surface::CountsOutcome::Ready(Box::new(uniform(128, size))),
        )),
        tick: None,
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let analysis = editor.presentation.analysis.clone().expect("its report");
    assert_eq!(analysis.source, AnalysisSource::Gpu);
    assert_eq!(analysis.identity, target.identity);
    assert_eq!(
        analysis.report.r[128],
        u64::from(size.0) * u64::from(size.1)
    );
    assert!(!editor.presentation.analysis_updating());
    assert!(!editor.gpu_counts_pending());
    assert!(editor.gpu.counts.is_none());
    assert!(
        editor.gpu_counts_handed().is_none(),
        "no tiles handed for counts taken"
    );
    let agent = editor.owner.register();
    let (requested, _) = call(
        &editor.owner,
        agent,
        "analysis.request",
        serde_json::json!({"asset_id": analysis.identity.asset_id, "target": {"kind": "current"}}),
    )
    .unwrap();
    assert_eq!(requested["status"], "ready", "a hit: {requested}");
    assert_eq!(
        requested["identity"],
        serde_json::to_value(&analysis.identity).unwrap()
    );
    assert_eq!(
        requested["result"]["r"][128],
        serde_json::json!(u64::from(size.0) * u64::from(size.1))
    );
    editor.gpu.counts_report = None;
    finish(editor, catalog);
}

/// Counts the surface could not take refuse the content the GPU presented: it is asked for again,
/// and the reference renders it, with its own report, and the GPU presents it no more.
#[test]
fn counts_that_fail_send_the_content_to_the_reference_renderer() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-fail");
    commit(&mut editor, 0.5);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.gpu.counts.is_some()
    });
    let target = editor.gpu.counts.clone().unwrap();
    let log = attach_log(&mut editor);
    editor.gpu.counts_report = Some(SurfaceCounts {
        rest: Some((
            target.versions[0],
            luxforge_ui::photo_surface::CountsOutcome::Failed(HistogramError::ReadbackFailed),
        )),
        tick: None,
    });
    let task = editor.update(Message::Preview(PreviewMessage::Poll));
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "gpu_presented_refused").len(), 1);
    assert_eq!(editor.gpu.refused_content, Some(target.content));
    editor.gpu.counts_report = None;
    // The current preview, planned again as the runtime's executor would, is the reference's.
    drop(task);
    let asset = editor.document.state.as_ref().unwrap().asset.id.clone();
    let payload = crate::app::tasks::current_preview_now(
        &editor.owner,
        editor.client,
        asset,
        editor.displayed_entry(),
        editor.drawn(),
    )
    .unwrap();
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        payload,
    )))));
    deliver_until(&mut editor, "the reference's report", |editor| {
        editor
            .presentation
            .analysis
            .as_ref()
            .is_some_and(|analysis| analysis.source == AnalysisSource::Reference)
            && editor.presentation.analysis_content == Some(target.content)
    });
    assert_eq!(editor.presentation.gpu_presented, None);
    finish(editor, catalog);
}

/// While a gesture's ticks are drawn on the GPU the inspector plots the counts of the frame each
/// drew, the frame on screen, marked updating and handed to no one; the release's counts replace
/// them.
#[test]
fn a_gestures_ticks_plot_the_counts_of_the_frame_on_screen_as_updating() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-motion");
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    // The boundary, and the first tick's own frame landed, so no report arrives under the next.
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
            && !editor.presentation.queue.is_busy()
            && !editor.presentation.queue.ready()
    });
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let revision = editor.session.draft.as_ref().unwrap().draft_revision;
    let boundary = editor.gpu.held_version().unwrap();
    editor.gpu.counts_report = Some(SurfaceCounts {
        rest: None,
        tick: Some(TickCounts {
            boundary,
            revision,
            size: (240, 160),
            counts: luxforge_ui::photo_surface::CountsOutcome::Ready(Box::new(uniform(
                255,
                (240, 160),
            ))),
        }),
    });
    let before = editor.presentation.analysis.clone();
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let shown = editor.presentation.shown_analysis().unwrap().clone();
    assert_eq!(shown.source, AnalysisSource::Motion);
    assert_eq!(shown.report.all_highlight, 240 * 160);
    assert_eq!(
        shown
            .identity
            .draft
            .as_ref()
            .map(|stamp| stamp.draft_revision),
        Some(revision)
    );
    assert!(editor.presentation.analysis_updating(), "marked updating");
    assert_eq!(
        editor.presentation.analysis, before,
        "the report is the last whole one, never motion's"
    );
    editor.gpu.counts_report = None;
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    finish(editor, catalog);
}

/// Before the surface has checked its stage the GPU presents nothing: the commit's job renders the
/// reference's exact frame and its report, as before.
#[test]
fn before_the_surface_has_checked_its_stage_the_reference_renders_the_commit() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-pending");
    editor.renderer.stage = Some(GpuStageState::Unchecked);
    let log = attach_log(&mut editor);
    commit(&mut editor, 0.6);
    deliver_until(&mut editor, "the reference's report", |editor| {
        editor.presentation.analysis_content == Some(editor.presentation.content_serial)
            && !editor.presentation.queue.is_busy()
    });
    let records = logged(&mut editor, &log);
    assert!(editor.gpu.counts.is_none());
    assert_eq!(editor.presentation.gpu_presented, None);
    assert!(
        events(&records, "preview_displayed")
            .iter()
            .all(|displayed| displayed["path"] != "gpu")
    );
    assert_eq!(
        editor.presentation.analysis.as_ref().unwrap().source,
        AnalysisSource::Reference
    );
    finish(editor, catalog);
}

/// Compare over a content the GPU presented waits for the reference's frame of it: the photograph
/// under the GPU's picture is an earlier content's and is never taken as the After side.
#[test]
fn compare_over_a_content_the_gpu_presented_waits_for_the_references_frame_of_it() {
    let (mut editor, catalog) = opened_on_the_gpu("compare");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.gpu_presented == Some(editor.presentation.content_serial)
    });
    let content = editor.presentation.content_serial;
    drop(editor.compare_toggle());
    assert!(editor.presentation.compare_after.is_none(), "no After yet");
    assert!(editor.gpu.compare_waits);
    // The current preview, planned as the runtime's executor would, is the reference's.
    let asset = editor.document.state.as_ref().unwrap().asset.id.clone();
    let payload = crate::app::tasks::current_preview_now(
        &editor.owner,
        editor.client,
        asset,
        editor.displayed_entry(),
        editor.drawn(),
    )
    .unwrap();
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        payload,
    )))));
    deliver_until(
        &mut editor,
        "Compare begun over the reference's frame",
        |editor| editor.presentation.compare_after.is_some(),
    );
    assert!(!editor.gpu.compare_waits);
    assert_eq!(editor.presentation.gpu_presented, None);
    assert_eq!(editor.presentation.presented_content, content);
    finish(editor, catalog);
}

/// A stack the GPU presented whose picture at rest finds its programs compiling keeps the frame on
/// screen, marked rendering, and asks for the clock's one wake at the status bar's `compiling`
/// threshold: a compile that ends sooner leaves the stack the GPU's, with no reference render.
#[test]
fn a_presented_stack_whose_compile_ends_within_the_threshold_stays_the_gpus() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-compiling-short");
    let log = attach_log(&mut editor);
    commit(&mut editor, 0.4);
    // Presented, and its view plan handed to be drawn at rest once the release has settled.
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.gpu.counts.is_some() && editor.gpu_rest_plan().is_some()
    });
    let content = editor.presentation.content_serial;
    let compiling = crate::app::gpu_preview::SurfaceReport {
        fallback: Some(luxforge_ui::photo_surface::GpuFallback::Compiling),
        ..crate::app::gpu_preview::SurfaceReport::default()
    };
    editor.gpu.surface = Some(compiling);
    assert!(editor.gpu_rest_compiling());
    drop(editor.update(Message::Preview(PreviewMessage::Poll)));
    assert_eq!(
        editor.presentation.gpu_presented,
        Some(content),
        "still the GPU's"
    );
    assert!(
        editor.gpu_compile_deadline(),
        "the threshold's wake is asked for"
    );
    assert!(editor.activity.render.is_none(), "marked rendering");
    assert_eq!(
        editor.workspace.status.fallback, None,
        "no reference frame to name"
    );
    // The compile ends: the surface draws the GPU's picture and the wait ends with it.
    surface_ready(&mut editor);
    drop(editor.update(Message::Preview(PreviewMessage::Poll)));
    assert!(!editor.gpu_compile_deadline());
    assert_eq!(editor.presentation.gpu_presented, Some(content));
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "gpu_presented_refused").is_empty(),
        "{records:?}"
    );
    assert!(
        events(&records, "preview_job_requested")
            .iter()
            .all(|job| job["generation"].as_u64() < Some(editor.presentation.presented_generation)),
        "no reference render"
    );
    finish(editor, catalog);
}

/// A compile that outlasts the threshold sends the stack to the reference, whose frame the
/// warm-up's label then names, and the deadline's wake goes.
#[test]
fn a_presented_stack_whose_compile_outlasts_the_threshold_is_the_references() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-compiling-long");
    let log = attach_log(&mut editor);
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.gpu.counts.is_some() && editor.gpu_rest_plan().is_some()
    });
    let content = editor.presentation.content_serial;
    editor.gpu.surface = Some(crate::app::gpu_preview::SurfaceReport {
        fallback: Some(luxforge_ui::photo_surface::GpuFallback::Compiling),
        ..crate::app::gpu_preview::SurfaceReport::default()
    });
    drop(editor.update(Message::Preview(PreviewMessage::Poll)));
    assert_eq!(editor.presentation.gpu_presented, Some(content));
    // The threshold's wake, as if half a second had passed since the compile was first seen.
    let (seen, since) = editor.gpu.rest_compiling_since.expect("the wait recorded");
    editor.gpu.rest_compiling_since = Some((seen, since - crate::state::status::COMPILING_AFTER));
    drop(editor.update(Message::Preview(PreviewMessage::Poll)));
    let records = logged(&mut editor, &log);
    let refused = events(&records, "gpu_presented_refused");
    assert_eq!(refused.len(), 1, "{records:?}");
    assert_eq!(refused[0]["why"], "compiling");
    assert_eq!(editor.gpu.refused_content, Some(content));
    assert_eq!(editor.presentation.gpu_presented, None);
    assert!(!editor.gpu_compile_deadline(), "no further wake");
    finish(editor, catalog);
}
