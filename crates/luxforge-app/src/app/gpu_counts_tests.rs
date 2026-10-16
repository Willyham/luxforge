//! The GPU's histogram and clipping counts against a real owner and preview worker
//! (`docs/design/gpu-first.md`, stage 2): a committed stack the GPU draws at rest is presented with
//! no CPU render and its tiles' counts become its report, plotted and in the owner's store; counts
//! that fail send the content to the reference renderer; a gesture's ticks plot the counts of the
//! frame each drew, as updating; and before the surface has checked its stage the reference renders
//! as before. The surface is no test's: what it reports is stood in for. A gesture's tick over a
//! stack the GPU presented never leaves the earlier stack's CPU frame under it as the photograph.
use super::*;
use crate::app::{
    Message,
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, deliver_until, surface_ready},
    message::{draft::DraftMessage, preview::PreviewMessage},
    tasks::call,
    testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
};
use luxforge_gpu::{GpuStageState, TickCounts, histogram::Counts, histogram::HistogramError};
use luxforge_ui::photo_surface::SurfaceCounts;

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
            luxforge_gpu::CountsOutcome::Ready(Box::new(uniform(128, size))),
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

/// A commit the GPU presents, its counts still to come when the view reaches a zoom that shows the
/// minimap: the overview sweep draws the held picture at rest again, under new versions. The counts
/// still to come are the swept tiles', so its report is adopted and nothing (a capture among them)
/// waits on counts of tiles handed no more.
#[test]
fn counts_still_to_come_are_the_overview_sweeps_tiles() {
    let (mut editor, catalog) = opened_on_the_gpu("counts-sweep");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.gpu.counts.is_some()
    });
    luxforge_testkit::client::call(
        &editor.owner,
        editor.client,
        "view.set",
        serde_json::json!({"zoom": {"mode": "percent", "value": 400.0}}),
    )
    .expect("the zoom");
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 400.0 };
    let _ = editor.zoom_changed(&luxforge_core::Zoom::Fit);
    deliver_until(
        &mut editor,
        "the minimap's sweep of the held tiles",
        |editor| {
            let sweep = editor.overviews.sweep_version();
            sweep.is_some() && editor.gpu_rest_held_overview() == Some(sweep)
        },
    );
    let target = editor.gpu.counts.clone().expect("its counts wanted");
    assert!(editor.gpu_counts_pending());
    let handed = editor.surfaces();
    let tiles = handed
        .gpu_rest
        .or(handed.gpu_counts)
        .expect("the swept tiles handed");
    assert!(
        target.versions.contains(&tiles.version),
        "the counts wanted are the swept tiles': {:?} against {}",
        target.versions,
        tiles.version
    );
    let size = (target.identity.width, target.identity.height);
    editor.gpu.counts_report = Some(SurfaceCounts {
        rest: Some((
            tiles.version,
            luxforge_gpu::CountsOutcome::Ready(Box::new(uniform(128, size))),
        )),
        tick: None,
    });
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.gpu_counts_pending(), "its report adopted");
    assert_eq!(
        editor
            .presentation
            .analysis
            .as_ref()
            .map(|analysis| analysis.source),
        Some(AnalysisSource::Gpu)
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
            luxforge_gpu::CountsOutcome::Failed(HistogramError::ReadbackFailed),
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
            counts: luxforge_gpu::CountsOutcome::Ready(Box::new(uniform(255, (240, 160)))),
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

/// The real photograph opened with the surface's GPU stage reporting `stage` and the picture at
/// rest's programs compiled or not (`warm`), as the open's frame is asked for: the editor, its
/// catalog and the open's events.
fn opened_with(
    name: &str,
    stage: Option<GpuStageState>,
    warm: bool,
) -> (Editor, std::path::PathBuf, Vec<serde_json::Value>) {
    let catalog = catalog(name);
    let config = crate::Config::default();
    let (owner, join) = crate::app::testing::start_owner(&catalog, &config);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let (mut editor, _, _, log) =
        crate::app::testing::real_photo_prepared(owner, join, &fixture, config, |editor| {
            editor.renderer.stage = stage;
            editor.gpu.programs_warm = Some(warm);
            attach_log(editor)
        });
    let records = logged(&mut editor, &log);
    (editor, catalog, records)
}

/// An open whose picture at rest's programs are already compiled is presented by the GPU alone:
/// no preview job is queued for it, so its first picture is the GPU's view plan, then its tiles,
/// drawn over an empty frame placed at the photograph's stage, which holds no other pixels; its
/// tiles' counts are its report; and the commit after it is the GPU's too.
#[test]
fn a_warm_open_is_presented_by_the_gpu_with_no_reference_job() {
    let (mut editor, catalog, records) =
        opened_with("open-warm", Some(GpuStageState::Available), true);
    let content = editor.presentation.content_serial;
    assert_eq!(editor.presentation.gpu_presented, Some(content));
    assert!(
        events(&records, "preview_job_requested").is_empty(),
        "no reference job for the open: {records:?}"
    );
    let displayed = events(&records, "preview_displayed");
    let first = displayed.first().expect("the open presented");
    assert_eq!(
        (&first["path"], &first["opened"]),
        (&serde_json::json!("gpu"), &serde_json::json!(true))
    );
    let surfaces = editor.surfaces();
    let under = surfaces.photo.expect("a frame placed at the stage");
    assert_eq!(under.size(), (1, 1), "an empty frame, no reference pixels");
    assert!(
        surfaces.gpu.is_some() || surfaces.gpu_rest.is_some(),
        "the GPU's picture handed in its place"
    );
    assert!(
        editor.gpu.counts.is_some(),
        "its tiles' counts are its report"
    );
    assert_eq!(
        editor.workspace.canvas.photo,
        crate::state::canvas::PhotoView::Plain
    );
    // The next commit draws over the GPU's own picture of the photograph.
    editor.gpu.programs_warm = None;
    let log = attach_log(&mut editor);
    commit(&mut editor, 0.3);
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "preview_job_requested").is_empty(),
        "{records:?}"
    );
    assert_eq!(
        editor.presentation.gpu_presented,
        Some(editor.presentation.content_serial)
    );
    finish(editor, catalog);
}

/// An open on a cold cache — the picture at rest's programs not compiled yet — and an open with no
/// GPU are the reference's: its job is queued and its exact frame is the first picture.
#[test]
fn a_cold_open_and_an_open_without_a_gpu_draw_the_reference_first() {
    for (name, stage, warm) in [
        ("open-cold", Some(GpuStageState::Available), false),
        ("open-unchecked", Some(GpuStageState::Unchecked), true),
        ("open-no-gpu", None, true),
    ] {
        let (mut editor, catalog, records) = opened_with(name, stage, warm);
        assert_eq!(editor.presentation.gpu_presented, None, "{name}");
        assert!(
            !events(&records, "preview_job_requested").is_empty(),
            "{name}: the reference's job queued"
        );
        deliver_until(&mut editor, "the reference's frame", |editor| {
            editor.presentation.presenter.photo().is_some()
                && editor.presentation.analysis.is_some()
        });
        let photo = editor.presentation.presenter.photo().expect("its frame");
        assert!(photo.size().0 > 1, "{name}: the reference's own pixels");
        assert_eq!(
            editor
                .presentation
                .analysis
                .as_ref()
                .map(|analysis| analysis.source),
            Some(AnalysisSource::Reference),
            "{name}"
        );
        finish(editor, catalog);
    }
}

/// Compare over a content the GPU presented waits for the reference's frame of it: the photograph
/// under the GPU's picture is an earlier content's and is never taken as the After side.
#[test]
fn zoomed_compare_over_gpu_content_keeps_the_reference_fallback() {
    let (mut editor, catalog) = opened_on_the_gpu("compare");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.gpu_presented == Some(editor.presentation.content_serial)
    });
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
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
        fallback: Some(luxforge_gpu::GpuFallback::Compiling),
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
        fallback: Some(luxforge_gpu::GpuFallback::Compiling),
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

#[test]
fn fit_compare_reuses_gpu_after_without_rendering_the_edited_stack() {
    let (mut editor, catalog) = opened_on_the_gpu("compare-retain");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.gpu_presented == Some(editor.presentation.content_serial)
    });
    assert!(editor.gpu_compare_can_retain());
    let plan = editor.gpu_rest_plan().unwrap().0.steps.clone();
    let after_source = editor.gpu_source_handed().unwrap().version();
    drop(editor.compare_toggle());
    assert!(
        editor.presentation.compare_after.is_some(),
        "{}",
        editor.status.text
    );
    assert!(!editor.gpu.compare_waits);
    assert!(editor.gpu.refused_content.is_none());
    // Stand in for a later source delivery, and keep After's source tied to its retained
    // plans through zoom and exit. Eligibility separately requires a shared development.
    editor.gpu_hold_source(&luxforge_core::PreviewSource::Jpeg(
        luxforge_core::SourceImage {
            width: 1,
            height: 1,
            rgba: std::sync::Arc::new(vec![0, 0, 0, 255]),
            fingerprint: "compare-before-source".into(),
            orientation: 1,
            capture: Default::default(),
        },
    ));
    assert_ne!(editor.gpu_source_handed().unwrap().version(), after_source);
    assert_eq!(
        editor.surfaces().compare_source.unwrap().version(),
        after_source
    );
    assert_eq!(editor.surfaces().compare_gpu.unwrap().steps, plan);
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
    assert_eq!(
        editor.surfaces().compare_gpu.unwrap().steps,
        plan,
        "zoom keeps the correct display-detail After picture"
    );
    drop(editor.compare_toggle());
    assert_eq!(editor.gpu_source_handed().unwrap().version(), after_source);
    finish(editor, catalog);
}

#[test]
fn a_comparison_without_a_shared_source_discards_retired_gpu_plans() {
    let (mut editor, catalog) = opened_on_the_gpu("compare-unshared-source");
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.gpu_presented == Some(editor.presentation.content_serial)
    });
    assert!(editor.gpu_rest_plan().is_some());
    assert!(editor.gpu_source_handed().is_some());
    // A historical selection cannot reuse the current entry's Original development identity.
    editor.document.compare_return = Some(luxforge_core::HistorySelection::Entry(
        luxforge_core::EntryId::new(),
    ));
    editor.gpu_compare_begin();
    editor.gpu_compare_end();
    assert!(editor.gpu_rest_plan().is_none());
    assert!(editor.gpu_source_handed().is_none());
    finish(editor, catalog);
}

/// A commit the GPU presented, its picture at rest drawn whole, and the version that picture is
/// handed under: the CPU frame under it is the open's, an earlier stack's. At 50%, where the view
/// draws the stage smaller than it is, so its picture at rest is reduced to the view.
fn presented_at_rest(name: &str) -> (Editor, std::path::PathBuf, u64) {
    let (mut editor, catalog) = opened_on_the_gpu(name);
    crate::app::gpu_preview_tests::zoomed_out(&mut editor, 50.0);
    commit(&mut editor, 0.4);
    deliver_until(&mut editor, "the committed stack presented", |editor| {
        editor.presentation.frame_behind()
            && editor.gpu_rest_plan().is_some()
            && editor.gpu_rest_handed().is_some()
            && !editor.presentation.queue.is_busy()
            && !editor.presentation.queue.ready()
    });
    let version = editor
        .gpu_rest_handed()
        .expect("its picture at rest")
        .version;
    editor.gpu.rest_done = Some(version);
    (editor, catalog, version)
}

/// The open draft's newest tick has no plan of its own: its job was planned at no view
/// (`unplannable`), as a boundary not yet derivable or a layer the GPU has no program for leaves
/// it.
fn tick_without_a_plan(editor: &mut Editor) {
    let set = editor
        .session
        .draft
        .as_ref()
        .map(crate::app::gesture::identity)
        .expect("an open draft");
    assert_eq!(
        editor.gpu_tick(&set, None),
        crate::app::gpu_preview::Tick::Cpu
    );
}

/// Over a stack the GPU presented, a drag's tick whose own plan the surface cannot draw yet — its
/// source still uploading — never leaves the earlier stack's CPU frame as the photograph: the plan
/// is handed to draw, and under it the committed stack's picture at rest, drawn whole already,
/// stands in wherever its output is not.
#[test]
fn a_tick_the_surface_cannot_draw_yet_shows_the_picture_at_rest_under_it() {
    let (mut editor, catalog, rest) = presented_at_rest("stand-in-under");
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: None,
        fallback: Some(luxforge_gpu::GpuFallback::SourceUploading {
            uploaded: 0,
            bytes: 1,
        }),
        drawn: None,
        evaluated: None,
    });
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    assert_eq!(editor.gpu_plan_fallback(), Some("source-uploading".into()));
    let surfaces = editor.surfaces();
    assert!(
        surfaces.gpu.is_some() && !surfaces.gpu_hold,
        "the tick's plan"
    );
    assert!(surfaces.gpu_tag.is_some(), "tagged with its revision");
    assert_eq!(
        surfaces.gpu_rest.map(|picture| picture.version),
        Some(rest),
        "the picture at rest stands in under it"
    );
    assert!(
        surfaces.gpu_rest_under,
        "drawn only where the plan's is not"
    );
    // Once the CPU frame is the draft's own, it is no earlier stack's, and nothing stands in.
    editor.presentation.gpu_presented = None;
    assert!(editor.surfaces().gpu_rest.is_none());
    assert!(!editor.surfaces().gpu_rest_under);
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// Over a stack the GPU presented, a drag's tick with no plan of its own keeps the committed
/// stack's GPU picture on screen, as at rest — its view plan drawn, not held behind the CPU frame,
/// and its tiles — never the earlier stack's CPU frame behind it.
#[test]
fn a_tick_with_no_plan_of_its_own_keeps_the_picture_at_rest_on_screen() {
    let (mut editor, catalog, rest) = presented_at_rest("held-at-rest");
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    tick_without_a_plan(&mut editor);
    assert_eq!(editor.gpu_plan_fallback(), Some("unplannable".into()));
    let surfaces = editor.surfaces();
    let (plan, _) = editor
        .gpu_rest_plan()
        .expect("the committed stack's view plan");
    assert!(
        surfaces.gpu.is_some_and(|drawn| std::ptr::eq(drawn, plan)),
        "the view plan at rest is handed"
    );
    assert!(!surfaces.gpu_hold, "drawn in place of the CPU frame");
    assert!(surfaces.gpu_tag.is_none(), "no gesture's revision");
    assert_eq!(surfaces.gpu_rest.map(|picture| picture.version), Some(rest));
    assert!(!surfaces.gpu_rest_under, "the picture at rest, as at rest");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A drag whose own frame the GPU drew keeps that frame on screen through a later tick with no
/// plan of its own: its last plan stays drawn, not held behind the CPU frame, rather than going
/// back to the committed stack or to the earlier stack's CPU frame.
#[test]
fn a_tick_with_no_plan_after_a_drawn_one_keeps_the_drags_own_frame() {
    let (mut editor, catalog, _) = presented_at_rest("held-own-frame");
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.6);
    let drawn = editor.surfaces();
    let boundary = drawn.gpu.expect("the tick's plan").boundary.version();
    let (revision, serial) = (
        drawn.gpu_tag.expect("its revision"),
        drawn.gpu_change.expect("its serial").serial,
    );
    // The surface drew that plan's output.
    editor.gpu.surface = Some(SurfaceReport {
        ready_boundary: Some(boundary),
        fallback: None,
        drawn: Some((boundary, revision)),
        evaluated: Some(serial),
    });
    tick_without_a_plan(&mut editor);
    let surfaces = editor.surfaces();
    assert!(
        editor.gpu_rest_plan().is_none(),
        "not the committed stack's"
    );
    assert_eq!(
        surfaces.gpu_change.map(|change| change.serial),
        Some(serial),
        "the drag's own last plan"
    );
    assert!(!surfaces.gpu_hold, "drawn, not held behind the CPU frame");
    assert_eq!(surfaces.gpu_tag, Some(revision));
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// A picture at rest the surface stopped for a source it had let go — Develop shown again after
/// Select, before the job that hands the source back — is drawn again under new versions once the
/// desktop holds the source's pixels to hand, and only once: a second stop stands.
#[test]
fn a_picture_at_rest_stopped_for_a_let_go_source_is_drawn_again_once() {
    let (mut editor, catalog, version) = presented_at_rest("rest-source-missing");
    let stopped = |version| luxforge_gpu::RestFigures {
        version,
        fallback: Some(luxforge_gpu::GpuFallback::SourceMissing),
        ..Default::default()
    };
    editor.gpu.rest_figures = Some(stopped(version));
    editor.gpu_rest_again();
    let again = editor
        .gpu_rest_handed()
        .expect("its picture at rest, drawn again")
        .version;
    assert_ne!(again, version, "under a new version");
    editor.gpu.rest_figures = Some(stopped(again));
    editor.gpu_rest_again();
    assert_eq!(
        editor.gpu_rest_handed().map(|rest| rest.version),
        Some(again),
        "a second stop stands"
    );
    finish(editor, catalog);
}
