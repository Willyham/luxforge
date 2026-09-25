//! Preview presentation: the proxy and exact phases, their histogram reports, the bounds a job is
//! rendered at, and zoom handing the retained frame back without a render.
use super::{
    evidence::Settle,
    preview::short,
    tasks::Upload,
    testing::{analysed, attach_log, drafted, entry, finish, logged, opened},
    *,
};
use crate::state::histogram::HistogramStatus;
use lightwell_core::{AssetId, ModuleRegistry, PreviewJob, PreviewSource, SourceImage, Zoom};

/// A report is taken up with the pixels it was reduced from, under the same generation: the
/// plot, the counters and the identity all describe the frame that is on screen.
#[test]
fn a_report_is_adopted_with_the_pixels_of_its_own_generation() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    // Four pixels: one all-black, one all-white, one at both endpoints, one ordinary.
    let pixels = [
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [0, 200, 255, 255],
        [12, 34, 56, 255],
    ];
    let (analysis, raster) = analysed(&editor, 7, &pixels, 2, 2);
    editor.preview_generation = 7;
    editor.incoming = Some((analysis, raster));
    editor.adopt_analysis(7);
    editor.rederive();
    let model = &editor.workspace.histogram;
    assert_eq!(model.status, HistogramStatus::Ready);
    assert!(!model.stale);
    assert_eq!(model.counters.any_shadow, 2);
    assert_eq!(model.counters.any_highlight, 2);
    assert_eq!(model.counters.both, 1);
    assert_eq!(model.counters.all_shadow, 1);
    // Both triangles are tinted because both endpoints have pixels; neither overlay is on yet.
    assert!(model.shadow.tinted && model.highlight.tinted);
    assert!(!model.shadow.active && !model.highlight.active);
    let identity = model.identity.as_ref().expect("a render identity");
    assert_eq!(identity.entry, entry_id.as_str());
    assert_eq!(identity.generation, 7);
    assert_eq!((identity.width, identity.height), (2, 2));
    assert_eq!(identity.draft_revision, None);
    // The raster is retained for the overlay, sharing the render's own buffer, under the
    // generation it arrived with.
    assert_eq!(
        editor.raster.as_ref().map(|(generation, _)| *generation),
        Some(7)
    );
    // The correlated state carries the whole inspector, so a captured frame is checkable.
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["histogram"]["status"], json!("ready"));
    assert_eq!(snapshot["histogram"]["counters"]["both"], json!(1));
    assert_eq!(
        snapshot["histogram"]["plotted_max"],
        json!(model.plotted_max)
    );
    assert_eq!(
        snapshot["histogram"]["identity"]["domain"],
        json!("srgb-8bit-output")
    );
    assert_eq!(snapshot["workspace"]["clip_shadows"], json!(false));
    assert_eq!(snapshot["workspace"]["clip_highlights"], json!(false));
    assert_eq!(snapshot["readout"], Value::Null);
    // The desktop hands its report to the owner, so an API client's request for the same
    // identity is a cache hit rather than a second render.
    let records = logged(&mut editor, &log);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == "analysis_adopted"
                && record["detail"]["generation"] == json!(7)),
        "no adoption was recorded: {records:?}"
    );
    finish(editor, catalog);
}

/// A report whose generation is not the one whose pixels just reached the screen describes
/// another frame, so it is dropped rather than plotted against the wrong photograph.
#[test]
fn a_report_from_an_older_generation_is_ignored() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let (analysis, raster) = analysed(&editor, 6, &[[0, 0, 0, 255]], 1, 1);
    editor.preview_generation = 7;
    editor.incoming = Some((analysis, raster));
    editor.adopt_analysis(7);
    editor.rederive();
    assert!(editor.analysis.is_none());
    assert!(editor.raster.is_none(), "no stale raster is retained");
    assert_eq!(editor.workspace.histogram.status, HistogramStatus::Pending);
    assert_eq!(
        editor.snapshot()["histogram"]["status"],
        json!("pending"),
        "the plot says pending rather than showing another frame's counts"
    );
    finish(editor, catalog);
}

/// While a newer frame is rendering the previous counts stay on screen and are marked stale,
/// rather than the plot going blank for the length of a render.
#[test]
fn a_newer_render_marks_the_shown_counts_stale_without_discarding_them() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let (analysis, raster) = analysed(&editor, 1, &[[9, 9, 9, 255]], 1, 1);
    editor.preview_generation = 1;
    editor.incoming = Some((analysis, raster));
    editor.adopt_analysis(1);
    editor.rederive();
    assert_eq!(editor.workspace.histogram.status, HistogramStatus::Ready);
    // A newer frame is in flight: the same counts are still plotted, now marked stale.
    let (newer, newer_raster) = analysed(&editor, 2, &[[0, 0, 0, 255]], 1, 1);
    editor.preview_generation = 2;
    editor.incoming = Some((newer, newer_raster));
    editor.rederive();
    let model = &editor.workspace.histogram;
    assert_eq!(model.status, HistogramStatus::Updating);
    assert!(model.stale);
    assert!(model.bins.is_some(), "the previous plot is still shown");
    // The dimmed plot is the stale label; no words are drawn over it.
    assert_eq!(model.notice(), None);
    finish(editor, catalog);
}

/// The photograph an open gesture shows is the drafted render, and the contract ties the counts
/// to the image presented. So a drafted result is adopted exactly like a committed one: its
/// report arrives with its own pixels under its own generation, the identity carries the draft
/// revision those pixels were planned from, and the counters are the drafted population.
#[test]
fn a_drafted_report_is_adopted_with_the_pixels_it_was_reduced_from() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    let draft_id = lightwell_core::DraftId::new();
    // A drafted exposure has driven two of the four pixels to the highlight endpoint.
    let pixels = [
        [255, 255, 255, 255],
        [255, 255, 255, 255],
        [10, 20, 30, 255],
        [0, 0, 0, 255],
    ];
    let (analysis, raster) = drafted(&editor, 9, &draft_id, 3, &pixels, 2, 2);
    editor.preview_generation = 9;
    editor.incoming = Some((analysis, raster));
    editor.adopt_analysis(9);
    editor.rederive();
    let model = &editor.workspace.histogram;
    assert_eq!(model.status, HistogramStatus::Ready);
    assert!(!model.stale, "the drafted frame on screen is not behind");
    assert_eq!(model.counters.any_highlight, 2);
    assert_eq!(model.counters.any_shadow, 1);
    assert_eq!(model.counters.all_highlight, 2);
    assert!(
        model.plotted_max > 0 && model.bins.is_some(),
        "the drafted population is plotted rather than left empty"
    );
    assert!(model.highlight.tinted, "the endpoint triangle is coloured");
    let identity = model.identity.as_ref().expect("a drafted render identity");
    assert_eq!(identity.draft_revision, Some(3));
    assert_eq!(identity.generation, 9);
    assert_eq!(identity.entry, entry_id.as_str());
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["histogram"]["status"], json!("ready"));
    assert_eq!(
        snapshot["histogram"]["identity"]["draft_revision"],
        json!(3),
        "a captured frame correlates the plot with the drafted revision"
    );
    assert_eq!(
        snapshot["histogram"]["counters"]["any_highlight"],
        json!(2),
        "a drafted frame reports the counts it can justify"
    );
    // The report goes to the owner store under the drafted identity, so an
    // `analysis.request {target: draft}` for it is a cache hit, not a second render.
    let records = logged(&mut editor, &log);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == "analysis_adopted"
                && record["detail"]["generation"] == json!(9)
                && record["detail"]["draft_revision"] == json!(3)),
        "the drafted report was not adopted: {records:?}"
    );
    finish(editor, catalog);
}

/// Between the gesture's tick and the drafted pixels reaching the screen the previous report
/// stays plotted and is marked updating, exactly as it is for a committed render: the plot
/// never blanks and never reports zeroes for an image it has not reduced. A drafted report
/// from a generation the screen has already moved past describes another image and is dropped.
#[test]
fn a_drafted_render_marks_the_previous_counts_updating_and_ignores_an_older_one() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let (committed, raster) = analysed(&editor, 4, &[[9, 9, 9, 255]], 1, 1);
    editor.preview_generation = 4;
    editor.incoming = Some((committed, raster));
    editor.adopt_analysis(4);
    editor.rederive();
    assert_eq!(editor.workspace.histogram.status, HistogramStatus::Ready);
    let before = editor.workspace.histogram.counters.clone();

    // The gesture's tick took a generation for the drafted preview; its pixels are not here.
    editor.preview_generation = 5;
    editor.rederive();
    let model = &editor.workspace.histogram;
    assert_eq!(model.status, HistogramStatus::Updating);
    assert!(model.stale);
    assert_eq!(model.counters, before, "the previous counts are kept");
    assert!(model.bins.is_some(), "the previous plot is still shown");
    assert_eq!(
        editor.snapshot()["histogram"]["identity"]["generation"],
        json!(4),
        "the plot still names the frame it describes"
    );

    // A drafted report for a generation that has been superseded is not plotted.
    let draft_id = lightwell_core::DraftId::new();
    let (older, older_raster) = drafted(&editor, 5, &draft_id, 1, &[[0, 0, 0, 255]], 1, 1);
    editor.preview_generation = 6;
    editor.incoming = Some((older, older_raster));
    editor.adopt_analysis(6);
    editor.rederive();
    assert_eq!(
        editor.workspace.histogram.counters, before,
        "an older drafted report replaced the counts on screen"
    );
    assert_eq!(editor.workspace.histogram.status, HistogramStatus::Updating);
    assert_eq!(
        editor
            .workspace
            .histogram
            .identity
            .as_ref()
            .map(|i| i.generation),
        Some(4)
    );
    finish(editor, catalog);
}

/// A view change re-renders nothing and re-reduces nothing: the retained report and raster are
/// untouched, no preview generation is taken and no request is opened. Only the overlay's cell
/// grid follows the zoom, and only while an overlay is actually on.
#[test]
fn zooming_and_panning_ask_for_no_preview_and_no_analysis() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let (analysis, raster) = analysed(&editor, 3, &[[0, 0, 0, 255]; 4], 2, 2);
    editor.preview_generation = 3;
    editor.presented_generation = 3;
    editor.incoming = Some((analysis, raster));
    editor.adopt_analysis(3);
    let before = (
        editor.preview_generation,
        editor.activity.requested,
        editor.analysis.clone(),
    );
    // No overlay is on, so a zoom or a pan derives nothing at all.
    for message in [
        Message::View(ViewMessage::Fit),
        Message::View(ViewMessage::HundredPercent),
        Message::View(ViewMessage::Zoom("50".into())),
        Message::View(ViewMessage::ApplyZoom),
        Message::View(ViewMessage::Panned(120.0, 40.0)),
        Message::View(ViewMessage::Resized(1200.0, 800.0)),
    ] {
        let _ = editor.update(message);
    }
    assert_eq!(editor.preview_generation, before.0, "no preview requested");
    assert_eq!(editor.activity.requested, before.1, "no request opened");
    assert_eq!(editor.analysis, before.2, "the report is untouched");
    assert!(editor.overlay_request.is_none(), "nothing to derive");

    // With an overlay on, the same view changes re-derive only the bounded overlay, from the
    // retained raster: still no preview, no request and no second reduction.
    editor.session.workspace.clip_shadows = true;
    editor.session.preview.view.zoom = Zoom::Fit;
    let _ = editor.update(Message::View(ViewMessage::Resized(1440.0, 900.0)));
    let fitted = editor.overlay_request.clone().expect("a fitted overlay");
    assert!(fitted.shadows && !fitted.highlights);
    // The photograph is 2x2 and drawn far larger than itself, so the grid is the source.
    assert_eq!((fitted.cells_w, fitted.cells_h), (2, 2));
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.update(Message::View(ViewMessage::ApplyZoom));
    let hundred = editor.overlay_request.clone().expect("a 100% overlay");
    assert_eq!(
        (hundred.cells_w, hundred.cells_h),
        (2, 2),
        "one cell per pixel"
    );
    assert_eq!(editor.preview_generation, before.0, "still no preview");
    assert_eq!(editor.activity.requested, before.1);
    assert_eq!(
        editor.analysis, before.2,
        "the histogram is not reduced again"
    );
    // An unchanged view derives nothing a second time.
    let repeated = editor.overlay_request.clone();
    let _ = editor.update(Message::View(ViewMessage::Panned(10.0, 10.0)));
    assert_eq!(editor.overlay_request, repeated, "a pan re-derives nothing");
    finish(editor, catalog);
}

/// The Fit bounds are the photo surface less the canvas padding, in physical pixels: exactly
/// the rectangle a fitted photograph is drawn into, which is the whole point of the proxy.
#[test]
fn the_fit_bounds_are_the_padded_photo_surface_in_physical_pixels() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.window = (1440.0, 900.0);
    editor.scale_factor = 2.0;
    editor.session.workspace.state_panel = true;
    editor.session.workspace.tools_panel = true;
    let surface = state::histogram::photo_surface(editor.window, true, true);
    let padding = 2.0 * view::canvas::PHOTO_PADDING;
    let bounds = editor
        .proxy_bounds()
        .expect("Fit is bounded by the display");
    assert_eq!(
        (bounds.width, bounds.height),
        (
            ((surface.0 - padding) * 2.0).round() as u32,
            ((surface.1 - padding) * 2.0).round() as u32
        ),
    );
    // Collapsing a panel widens the surface, so the next job's bounds widen with it.
    editor.session.workspace.state_panel = false;
    let wider = editor.proxy_bounds().expect("Fit is still bounded");
    assert!(wider.width > bounds.width && wider.height == bounds.height);
    // A window with no room at all offers nothing rather than a degenerate rectangle.
    editor.window = (0.0, 0.0);
    assert_eq!(editor.proxy_bounds(), None);
    finish(editor, catalog);
}

/// The zoom rule: a percentage that draws the stage smaller than itself is bounded by that
/// drawn size, and 100% and above are not bounded at all, which is what keeps the 100% view the
/// exact render of the exact recipe.
#[test]
fn a_percentage_is_bounded_only_while_the_stage_is_drawn_smaller_than_itself() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.window = (1440.0, 900.0);
    editor.scale_factor = 1.0;
    editor.dimensions = Some((6000, 4000));
    editor.session.preview.view.zoom = Zoom::Percent { value: 50.0 };
    let half = editor.proxy_bounds().expect("a zoomed-out view is bounded");
    assert_eq!((half.width, half.height), (3000, 2000));
    for value in [100.0, 200.0, 400.0] {
        editor.session.preview.view.zoom = Zoom::Percent { value };
        assert_eq!(
            editor.proxy_bounds(),
            None,
            "{value}% shows a stage pixel in a display pixel or more"
        );
    }
    // Nothing is known about the stage before a frame has arrived, so nothing is offered.
    editor.dimensions = None;
    editor.session.preview.view.zoom = Zoom::Percent { value: 50.0 };
    assert_eq!(editor.proxy_bounds(), None);
    finish(editor, catalog);
}

/// A zoom across the proxy boundary hands the surface pixels that already exist and renders
/// nothing; a zoom that stays on one side of it hands over nothing at all, so the texture is
/// written once and a pan writes nothing.
#[test]
fn a_zoom_hands_the_retained_raster_to_the_surface_and_asks_for_no_preview() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    // A proxy of generation 7 is on screen, with its exact phase adopted beside it.
    let pixels = |code: u8| {
        Arc::new(lightwell_core::Raster {
            width: 2,
            height: 2,
            rgba: vec![code; 16].into(),
            source_fingerprint: "source-1".into(),
            snapshot_id: lightwell_core::SnapshotId::new(),
        })
    };
    editor.presented_generation = 7;
    editor.presented_entry = Some(entry_id);
    editor.presented_proxy = true;
    editor.preview_generation = 7;
    editor.proxy_frame = Some(ProxyFrame {
        generation: 7,
        raster: pixels(1),
        dimensions: (1200, 900),
        built: true,
        approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        render_ms: 12.0,
    });
    editor.raster = Some((7, pixels(2)));
    editor.exact_render_ms = Some((7, 85.0));
    // What the status bar reports is the time of the picture on screen, and each retained
    // frame brings its own: the proxy's while the proxy is shown, the exact render's at 100%.
    editor.activity.render = Some(state::status::RenderTime {
        ms: 12.0,
        proxy: true,
        approximate: false,
    });

    // Fit to 100%: the retained exact raster becomes the surface's source and no job is
    // queued. Nothing is written here; the next redraw's `prepare` writes it once.
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    assert!(
        !editor.presented_proxy,
        "the exact raster is what is on screen"
    );
    let exact = editor.presenter.photo_version();
    assert!(exact > 0, "the exact raster was handed to the surface");
    assert_eq!(
        editor.activity.render,
        Some(state::status::RenderTime {
            ms: 85.0,
            proxy: false,
            approximate: false,
        }),
        "the exact raster on screen reports its own render time"
    );
    assert_eq!(
        editor.preview_generation, 7,
        "no preview job was requested: nothing was rendered for a view change"
    );

    // 100% to 200% stays on the exact side: nothing at all happens, so the surface is not
    // given the same raster again and a pan writes nothing.
    editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
    let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert_eq!(
        editor.presenter.photo_version(),
        exact,
        "a zoom within the exact view handed the raster over again"
    );
    assert_eq!(editor.preview_generation, 7, "and asked for no preview");

    // Back to Fit: the retained proxy is handed over again rather than rendered again.
    editor.session.preview.view.zoom = Zoom::Fit;
    let _ = editor.zoom_changed(&Zoom::Percent { value: 200.0 });
    assert!(editor.presented_proxy, "the proxy is back on screen");
    assert_eq!(
        editor.presenter.photo_version(),
        exact + 1,
        "the retained proxy was handed to the surface"
    );
    assert_eq!(
        editor.activity.render,
        Some(state::status::RenderTime {
            ms: 12.0,
            proxy: true,
            approximate: false,
        }),
        "the proxy on screen reports its own render time again"
    );
    assert_eq!(
        editor.preview_generation, 7,
        "no preview job was requested: nothing was rendered for a view change"
    );
    finish(editor, catalog);
}

/// A frame that approximates a drafted RAW white balance is presented like any frame and says
/// so — in the status bar, the `preview_displayed` event and the state summary, at Fit and at
/// 100% — but it is never taken for a report: the last exact report stays plotted, marked
/// updating, through both phases of the approximate job, and the next exact report replaces it.
/// An overlay derived from its full-size phase is approximate too.
#[test]
fn an_approximate_white_balance_frame_is_shown_and_labelled_but_never_replaces_the_report() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.preview_queue = PreviewQueue::default();
    let pixels = [
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [0, 200, 255, 255],
        [12, 34, 56, 255],
    ];
    let (analysis, raster) = analysed(&editor, 7, &pixels, 2, 2);
    let identity = analysis.identity.clone();
    editor.preview_generation = 7;
    editor.incoming = Some((analysis, raster.clone()));
    editor.adopt_analysis(7);

    // The drafted job's proxy phase is presented.
    editor.preview_generation = 8;
    editor.proxy_frame = Some(ProxyFrame {
        generation: 8,
        raster: raster.clone(),
        dimensions: (2, 2),
        built: false,
        approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: true,
        render_ms: 9.2,
    });
    let upload = Upload {
        generation: 8,
        draft_revision: Some(1),
        width: 4000,
        height: 3000,
        entry_id: entry_id.clone(),
        snapshot_id: raster.snapshot_id.to_string(),
        source_fingerprint: raster.source_fingerprint.clone(),
        proxy: true,
        proxy_dimensions: Some((2, 2)),
        proxy_built: false,
        proxy_approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: true,
        reason: None,
        render_ms: Some(9.2),
    };
    editor.present(upload, &raster);
    editor.rederive();
    assert_eq!(
        editor.workspace.status.render,
        "Rendered in 9 ms (proxy, approximate)"
    );
    let histogram = |editor: &Editor| {
        let model = &editor.workspace.histogram;
        (
            model.status,
            model.identity.as_ref().map(|identity| identity.generation),
        )
    };
    assert_eq!(
        histogram(&editor),
        (HistogramStatus::Updating, Some(7)),
        "the last exact report stays plotted and says it is updating"
    );

    // Its exact phase lands with no report, as an approximate job's always does.
    editor.adopt_exact(8, identity, None, (*raster).clone(), 140.0, true);
    editor.rederive();
    assert_eq!(
        histogram(&editor),
        (HistogramStatus::Updating, Some(7)),
        "an approximate frame never replaces the report, not even with nothing"
    );
    assert_eq!(
        editor.raster.as_ref().map(|(generation, _)| *generation),
        Some(8),
        "its pixels are retained for the overlay and the 100% view"
    );
    assert_eq!(
        editor
            .overlay_source()
            .map(|(_, _, approximate)| approximate),
        Some(true),
        "a mask derived from it is approximate"
    );
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["approximate_white_balance"], json!(true));
    assert_eq!(snapshot["status_bar"]["render_approximate"], json!(true));
    assert_eq!(snapshot["histogram"]["status"], json!("updating"));

    // At 100% the retained full-size phase is shown, and says so.
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    editor.rederive();
    assert!(!editor.presented_proxy);
    assert_eq!(
        editor.workspace.status.render,
        "Rendered in 140 ms (approximate)"
    );
    let records = logged(&mut editor, &log);
    let displayed: Vec<_> = records
        .iter()
        .filter(|record| record["event"] == "preview_displayed")
        .map(|record| record["detail"]["approximate_white_balance"].clone())
        .collect();
    assert_eq!(displayed, vec![json!(true), json!(true)]);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "analysis_adopted"
                && record["detail"]["generation"] == json!(8)),
        "no report was adopted for the approximate generation"
    );

    // The exact frame the release produces replaces the report, and the flag.
    let (analysis, raster) = analysed(&editor, 9, &pixels, 2, 2);
    editor.preview_generation = 9;
    editor.incoming = Some((analysis, raster.clone()));
    let upload = Upload {
        generation: 9,
        draft_revision: None,
        width: 4000,
        height: 3000,
        entry_id,
        snapshot_id: raster.snapshot_id.to_string(),
        source_fingerprint: raster.source_fingerprint.clone(),
        proxy: false,
        proxy_dimensions: None,
        proxy_built: false,
        proxy_approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        reason: None,
        render_ms: Some(150.0),
    };
    editor.present(upload, &raster);
    editor.rederive();
    assert_eq!(histogram(&editor), (HistogramStatus::Ready, Some(9)));
    assert!(!editor.raster_approximate_white_balance);
    assert_eq!(editor.snapshot()["approximate_white_balance"], json!(false));
    assert_eq!(editor.workspace.status.render, "Rendered in 150 ms");
    finish(editor, catalog);
}

/// The status bar's "Rendered in" figure is the presented frame's own worker time, not the
/// time since the last open or commit. A drafted frame, a zoom hand-over or a refit is
/// presented long after that request; before this was measured on the worker, a frame
/// presented minutes after the open reported minutes.
#[test]
fn the_render_figure_is_the_presented_frames_own_time_not_the_time_since_the_request() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    // The last open or commit began long ago, as it has in any real session after a while.
    editor.activity.request_started = Instant::now()
        .checked_sub(std::time::Duration::from_secs(500))
        .unwrap_or_else(Instant::now);
    let raster = lightwell_core::Raster {
        width: 2,
        height: 2,
        rgba: vec![7; 16].into(),
        source_fingerprint: "source-1".into(),
        snapshot_id: lightwell_core::SnapshotId::new(),
    };
    let upload = |generation: u64, proxy: bool, render_ms: f64| Upload {
        generation,
        draft_revision: None,
        width: 480,
        height: 320,
        entry_id: entry_id.clone(),
        snapshot_id: raster.snapshot_id.to_string(),
        source_fingerprint: raster.source_fingerprint.clone(),
        proxy,
        proxy_dimensions: proxy.then_some((240, 160)),
        proxy_built: false,
        proxy_approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        reason: None,
        render_ms: Some(render_ms),
    };
    // The renderer is idle, so the bar reports a figure rather than "Rendering…".
    editor.preview_queue = PreviewQueue::default();
    editor.present(upload(5, true, 12.4), &raster);
    editor.rederive();
    assert_eq!(
        editor.workspace.status.render, "Rendered in 12 ms (proxy)",
        "the proxy's own time, not the 500 s since the request"
    );
    // An exact frame presented later (a 100% view) reports its own time and says nothing of a
    // proxy.
    editor.present(upload(6, false, 85.2), &raster);
    editor.rederive();
    assert_eq!(editor.workspace.status.render, "Rendered in 85 ms");
    // The evidence event carries the same figure, so a run can assert it is plausible.
    let records = logged(&mut editor, &log);
    let displayed: Vec<_> = records
        .iter()
        .filter(|record| record["event"] == "preview_displayed")
        .map(|record| record["detail"]["render_ms"].clone())
        .collect();
    assert_eq!(displayed, vec![json!(12.4), json!(85.2)]);
    finish(editor, catalog);
}

/// With no proxy retained for the frame on screen — it was rendered exactly, at 100% — a zoom
/// back to Fit is the one view change that asks for a render.
#[test]
fn a_zoom_back_to_fit_with_no_retained_proxy_requests_one_preview() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.presented_generation = 3;
    editor.presented_proxy = false;
    editor.session.preview.view.zoom = Zoom::Fit;
    let path = crate::app::testing::attach_log(&mut editor);
    // The returned task is the owner round trip that ends in one preview job. Nothing reaches
    // the surface, because there are no pixels of this frame at the size Fit now asks for.
    let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert_eq!(
        editor.presenter.photo_version(),
        0,
        "there were no pixels to hand over"
    );
    let records = crate::app::testing::logged(&mut editor, &path);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == json!("preview_proxy_requested")),
        "the one render a view change asks for is recorded"
    );
    finish(editor, catalog);
}

/// A one-pixel whole-stack preview job of a fresh entry: enough for the queue to plan and
/// nothing more, as the other queue tests build theirs.
fn preview_job_for(_editor: &Editor) -> PreviewJob {
    let asset = AssetId::new();
    let entry = entry(&asset, 1, None);
    PreviewJob {
        source: PreviewSource::Jpeg(SourceImage {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255].into(),
            fingerprint: "test".into(),
            orientation: 1,
        }),
        registry: Arc::new(ModuleRegistry::builtin()),
        context: lightwell_core::RenderContext::new(),
        recipe: entry.snapshot.recipe.clone(),
        layer_count: None,
        draft_revision: None,
        identity: lightwell_core::analysis::AnalysisIdentity::of(
            &entry.asset_id,
            "test",
            &entry,
            &entry.snapshot.recipe,
            None,
            Some((1, 1)),
        )
        .expect("a test analysis identity"),
        analyse: false,
        proxy: None,
        mask_overlay: None,
        entry,
    }
}

/// The bounds a job renders for are the window, the panels and the display scale of the
/// moment it is requested, not of the moment its owner task was created: the display scale
/// arrives after launch, and every job requested after it must already be at it.
#[test]
fn a_preview_job_takes_the_bounds_of_the_moment_it_is_requested() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 1.0;
    let at_one = editor.proxy_bounds().expect("Fit asks for a proxy");
    editor.scale_factor = 2.0;
    let at_two = editor.proxy_bounds().expect("Fit asks for a proxy");
    assert_eq!(
        at_two.width,
        at_one.width * 2,
        "the bounds follow the scale"
    );
    let mut job = preview_job_for(&editor);
    // Whatever the task carried is replaced: a job made for the wrong scale is corrected here.
    job.proxy = Some(at_one);
    let generation = editor.request_preview(job);
    assert_eq!(
        editor.pending_bounds.get(&generation).copied().flatten(),
        Some(at_two),
        "the job was given the bounds of the request"
    );
    finish(editor, catalog);
}

/// A proxy on screen whose bounds no longer match the window is re-rendered once, and not
/// again while that refit is on its way.
#[test]
fn a_bounds_change_refits_the_presented_proxy_once() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 1.0;
    editor.presented_generation = 7;
    editor.presented_proxy = true;
    editor.preview_generation = 7;
    editor.presented_bounds = editor.proxy_bounds();
    let path = crate::app::testing::attach_log(&mut editor);
    // Same bounds: nothing is asked for.
    let _ = editor.update(Message::View(ViewMessage::ScaleFactor(1.0)));
    assert!(!editor.refit_pending);
    // The display scale arrives: the proxy on screen was made for half the pixels.
    let _ = editor.update(Message::View(ViewMessage::ScaleFactor(2.0)));
    assert!(editor.refit_pending, "one refit is on its way");
    let _ = editor.update(Message::View(ViewMessage::ScaleFactor(2.0)));
    let records = crate::app::testing::logged(&mut editor, &path);
    let refits = records
        .iter()
        .filter(|record| {
            record["event"] == json!("preview_proxy_requested")
                && record["detail"]["reason"] == json!("bounds")
        })
        .count();
    assert_eq!(refits, 1, "a refit is asked for once, not per event");
    finish(editor, catalog);
}

/// A scripted step whose frame is due — its session round trip settled it earlier in the same
/// update — waits instead for the refit the view just asked for, so its capture never shows a
/// proxy made for the previous bounds.
#[test]
fn a_settled_step_waits_for_the_refit_its_view_asked_for() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 1.0;
    editor.presented_generation = 7;
    editor.presented_proxy = true;
    editor.preview_generation = 7;
    editor.presented_bounds = editor.proxy_bounds();
    if let Some(evidence) = &mut editor.evidence {
        evidence.awaiting = None;
        evidence.capture_pending = true;
    }
    let _ = editor.update(Message::View(ViewMessage::ScaleFactor(2.0)));
    assert!(editor.refit_pending, "the new bounds asked for a frame");
    let evidence = crate::app::testing::evidence(&editor);
    assert!(!evidence.capture_pending, "the old proxy is not captured");
    assert_eq!(evidence.awaiting, Some(Settle::Preview));
    finish(editor, catalog);
}

#[test]
fn short_ids_are_safe_for_status_display() {
    assert_eq!(short("abc"), "abc");
    assert_eq!(short("123456789012345"), "123456789012");
}
