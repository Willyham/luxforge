//! Evidence capture waits for the frame its state describes.
use super::{
    message::SyncMessage,
    testing::{boot, finish},
    *,
};
use luxforge_core::{CropStage, Zoom};

/// An empty catalog has no photograph to draw, so GPU photo readiness must not block its frame.
#[test]
fn empty_editor_capture_needs_no_photo_texture() {
    let (mut editor, catalog) = boot();
    editor.state = None;
    editor.session.workspace.clip_highlights = true;
    assert!(editor.capture_photo_ready());
    assert!(editor.capture_clipping_ready());
    finish(editor, catalog);
}

#[test]
fn clipping_readiness_applies_only_to_the_ordinary_photo() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.session.workspace.clip_highlights = true;
    assert!(
        !editor.capture_clipping_ready(),
        "the ordinary overlay is pending"
    );
    crate::app::testing::hold_crop(
        &mut editor,
        crate::crop_draft::CropDraft::neutral(
            CropStage {
                width: 4000,
                height: 3000,
                angle: 0.0,
            },
            0,
        ),
        crate::app::crop::StageView::Shown,
    );
    assert!(
        editor.capture_clipping_ready(),
        "the crop stage has its own surface"
    );
    editor.gesture = None;
    editor.developer = true;
    editor.gallery = Some(0);
    assert!(
        editor.capture_clipping_ready(),
        "the gallery has its own surface"
    );
    editor.gallery = None;
    editor.render_error = Some(luxforge_core::Error::render("failed"));
    assert!(
        editor.capture_clipping_ready(),
        "an error is captured explicitly"
    );
    finish(editor, catalog);
}

#[test]
fn failed_discovery_is_reported_and_never_blocks_evidence() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Err(
        "protocol: gone".into(),
    ))));
    assert!(editor.modules_ready);
    assert!(editor.modules.is_empty());
    assert!(
        editor.status.contains("Tool discovery failed"),
        "{}",
        editor.status
    );
    finish(editor, catalog);
}

/// An open can complete its exact analysis while the first proxy is still being refitted
/// for the display scale. Its outcome arms evidence, but the old proxy is not a settled frame.
#[test]
fn evidence_capture_waits_for_the_proxy_at_current_bounds() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 1.0;
    editor.presented_proxy = true;
    editor.presented_bounds = editor.proxy_bounds();
    editor.scale_factor = 2.0;
    editor.refit_pending = true;
    editor.outcome_ready(false);
    assert!(crate::app::testing::evidence(&editor).capture_pending);
    assert!(
        !editor.capture_proxy_ready(),
        "the old 1× proxy is not ready"
    );

    editor.presented_bounds = editor.proxy_bounds();
    assert!(!editor.capture_proxy_ready(), "the refit is still pending");
    editor.render_error = Some(luxforge_core::Error::resource_limit("refit failed"));
    assert!(
        editor.capture_proxy_ready(),
        "a failed refit is captured as an error"
    );
    editor.render_error = None;
    editor.refit_pending = false;
    assert!(editor.capture_proxy_ready(), "the 2× proxy is ready");

    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    assert!(
        editor.capture_proxy_ready(),
        "100% does not require a proxy"
    );
    editor.presented_proxy = false;
    assert!(
        editor.capture_proxy_ready(),
        "an exact frame needs no proxy refit"
    );
    finish(editor, catalog);
}

/// A panel toggle changes Fit bounds, but a slider or crop draft owns the preview and
/// deliberately defers its refit. A queued refit can also be superseded by a crop input-stage
/// job, leaving the boolean set while no refit frame will arrive. Both settled drafts can be
/// captured at the pixels they actually show.
#[test]
fn evidence_capture_accepts_bounds_deferred_by_slider_and_crop_drafts() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 2.0;
    editor.presented_proxy = true;
    editor.presented_generation = 7;
    editor.session.workspace.tools_panel = true;
    editor.presented_bounds = editor.proxy_bounds();
    editor.session.workspace.tools_panel = false;
    assert_ne!(editor.presented_bounds, editor.proxy_bounds());
    assert!(
        !editor.capture_proxy_ready(),
        "outside a draft, refit is required"
    );

    crate::app::testing::hold_slider(&mut editor, "set-basic", "exposure");
    let _ = editor.refit_proxy();
    assert!(!editor.refit_pending, "the slider defers refit");
    assert!(
        editor.capture_proxy_ready(),
        "the slider's frame can be captured"
    );
    editor.gesture = None;

    crate::app::testing::hold_crop(
        &mut editor,
        crate::crop_draft::CropDraft::neutral(
            CropStage {
                width: 4000,
                height: 3000,
                angle: 0.0,
            },
            0,
        ),
        crate::app::crop::StageView::Shown,
    );
    editor.refit_pending = true; // The queued refit was superseded by crop input-stage work.
    assert!(
        editor.capture_proxy_ready(),
        "the crop frame can be captured"
    );
    editor.gesture = None;
    assert!(
        !editor.capture_proxy_ready(),
        "a pending refit blocks ordinary captures"
    );
    finish(editor, catalog);
}

/// A screenshot requested against one proxy may return after the refit proxy is presented.
/// That readback is discarded before a frame event or file is published and retried on a
/// newly drawn frame. An overlay capture keeps its overlay requirement across the retry.
#[test]
fn evidence_retries_a_readback_superseded_by_new_pixels() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.scale_factor = 2.0;
    editor.presented_proxy = true;
    editor.presented_bounds = editor.proxy_bounds();
    // Two photographs have reached the surface since the capture recorded the first.
    for _ in 0..2 {
        editor.presenter.show_photo(&luxforge_core::Raster {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255].into(),
            source_fingerprint: "f".into(),
            snapshot_id: luxforge_core::SnapshotId::new(),
        });
    }
    assert_eq!(editor.presenter.photo_version(), 2);
    let path = crate::app::testing::attach_log(&mut editor);
    let evidence = editor.evidence.as_mut().expect("evidence run");
    evidence.sync.state = Some((json!({"old":"proxy"}), 1, 1));
    evidence.capture_pending = false;
    evidence.capture_overlay = true;
    evidence.saving = true;
    let shot = iced::window::Screenshot::new([0, 0, 0, 255].to_vec(), iced::Size::new(1, 1), 1.0);
    let _ = editor.dispatch(Message::Evidence(EvidenceMessage::Captured(shot)));

    let evidence = crate::app::testing::evidence(&editor);
    assert!(evidence.capture_pending, "retry is armed");
    assert!(!evidence.saving, "the stale save was cancelled");
    assert!(evidence.sync.state.is_none(), "stale state was dropped");
    assert!(
        evidence.capture_overlay,
        "overlay requirement survives retry"
    );
    assert!(evidence.frames.is_empty(), "no frame was published");
    assert!(
        !crate::app::testing::logged(&mut editor, &path)
            .iter()
            .any(|event| event["event"] == "frame_captured"),
        "no capture event was published"
    );
    finish(editor, catalog);
}

/// An overlay may change while a screenshot is in flight without changing the photograph. Its
/// recorded state must not be published beside pixels from a newer clipping frame.
#[test]
fn evidence_retries_a_readback_superseded_only_by_clipping() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.presenter.show_photo(&luxforge_core::Raster {
        width: 1,
        height: 1,
        rgba: vec![0, 0, 0, 255].into(),
        source_fingerprint: "f".into(),
        snapshot_id: luxforge_core::SnapshotId::new(),
    });
    let photo_version = editor.presenter.photo_version();
    editor.session.workspace.clip_highlights = true;
    let generation = editor.presented_generation;
    editor.overlay_request = Some(overlay::OverlayRequest {
        generation,
        cells_w: 1,
        cells_h: 1,
        shadows: false,
        highlights: true,
        approximate: false,
        region: None,
    });
    assert!(
        editor
            .presenter
            .show_clipping(generation, vec![255; 4], (1, 1))
    );
    let first_clipping = editor.overlay_surface().unwrap().version();
    assert!(
        editor
            .presenter
            .show_clipping(generation, vec![0; 4], (1, 1))
    );
    assert_eq!(editor.presenter.photo_version(), photo_version);
    assert_ne!(editor.overlay_surface().unwrap().version(), first_clipping);

    let evidence = editor.evidence.as_mut().expect("evidence run");
    evidence.sync.state = Some((json!({"old":"clipping"}), 1, photo_version));
    evidence.sync.clipping_version = Some(first_clipping);
    evidence.capture_pending = false;
    evidence.saving = true;
    let shot = iced::window::Screenshot::new([0, 0, 0, 255].to_vec(), iced::Size::new(1, 1), 1.0);
    let _ = editor.dispatch(Message::Evidence(EvidenceMessage::Captured(shot)));
    let evidence = crate::app::testing::evidence(&editor);
    assert!(
        evidence.capture_pending && !evidence.saving,
        "a new draw is requested"
    );
    assert!(evidence.sync.state.is_none());
    assert!(evidence.sync.clipping_version.is_none());
    assert!(
        evidence.frames.is_empty(),
        "no mismatched frame was published"
    );
    finish(editor, catalog);
}
