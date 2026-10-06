//! Evidence capture waits for the frame its state describes, and an event is built only for a log.
use super::{
    message::sync::SyncMessage,
    testing::{boot, boot_with, finish},
    *,
};
use luxforge_core::{CropStage, Zoom};

#[test]
fn failed_request_records_the_api_refusal_and_captures_its_state() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    let _ = editor.next_step();
    editor.status.text = "validation: white balance is unavailable".into();
    editor.outcome(crate::app::outcome::Outcome::RequestEnded { failed: true });
    let evidence = crate::app::testing::evidence(&editor);
    let record = evidence.current.as_ref().expect("running step");
    assert_eq!(record["status"], json!("failed"));
    assert_eq!(
        record["reason"],
        json!("validation: white balance is unavailable")
    );
    assert!(evidence.capture_pending);
    assert!(evidence.had_errors);
    finish(editor, catalog);
}

/// An empty catalog has no photograph to draw, so GPU photo readiness must not block its frame.
#[test]
fn empty_editor_capture_needs_no_photo_texture() {
    let (mut editor, catalog) = boot();
    editor.document.state = None;
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
    editor.view_state.gallery = Some(0);
    assert!(
        editor.capture_clipping_ready(),
        "the gallery has its own surface"
    );
    editor.view_state.gallery = None;
    editor.presentation.render_error = Some(luxforge_core::Error::render("failed"));
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
        editor.status.text.contains("Tool discovery failed"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// An open can complete its exact analysis while its first reduction is still being refitted
/// for the display scale. Its outcome arms evidence, but the old reduction is not a settled frame.
#[test]
fn evidence_capture_waits_for_the_reduction_at_current_bounds() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.view_state.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.view_state.scale_factor = 1.0;
    editor.presentation.presented_reduced = true;
    editor.presentation.presented_bounds = editor.proxy_bounds();
    editor.view_state.scale_factor = 2.0;
    editor.presentation.refit_pending = true;
    editor.outcome(crate::app::outcome::Outcome::RequestEnded { failed: false });
    assert!(crate::app::testing::evidence(&editor).capture_pending);
    assert!(
        !editor.capture_refit_ready(),
        "the old 1× reduction is not ready"
    );

    editor.presentation.presented_bounds = editor.proxy_bounds();
    assert!(!editor.capture_refit_ready(), "the refit is still pending");
    editor.presentation.render_error = Some(luxforge_core::Error::resource_limit("refit failed"));
    assert!(
        editor.capture_refit_ready(),
        "a failed refit is captured as an error"
    );
    editor.presentation.render_error = None;
    editor.presentation.refit_pending = false;
    assert!(editor.capture_refit_ready(), "the 2× reduction is ready");

    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    assert!(
        editor.capture_refit_ready(),
        "100% does not require a reduction"
    );
    editor.presentation.presented_reduced = false;
    assert!(
        editor.capture_refit_ready(),
        "an exact frame needs no refit"
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
    editor.view_state.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.view_state.scale_factor = 2.0;
    editor.presentation.presented_reduced = true;
    editor.presentation.presented_generation = 7;
    editor.session.workspace.tools_panel = true;
    editor.presentation.presented_bounds = editor.proxy_bounds();
    editor.session.workspace.tools_panel = false;
    assert_ne!(editor.presentation.presented_bounds, editor.proxy_bounds());
    assert!(
        !editor.capture_refit_ready(),
        "outside a draft, refit is required"
    );

    crate::app::testing::hold_slider(&mut editor, "set-basic", "exposure");
    let _ = editor.refit_view();
    assert!(
        !editor.presentation.refit_pending,
        "the slider defers refit"
    );
    assert!(
        editor.capture_refit_ready(),
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
    editor.presentation.refit_pending = true; // The queued refit was superseded by crop input-stage work.
    assert!(
        editor.capture_refit_ready(),
        "the crop frame can be captured"
    );
    editor.gesture = None;
    assert!(
        !editor.capture_refit_ready(),
        "a pending refit blocks ordinary captures"
    );
    finish(editor, catalog);
}

/// A screenshot requested against one frame may return after the refit frame is presented.
/// That readback is discarded before a frame event or file is published and retried on a
/// newly drawn frame. An overlay capture keeps its overlay requirement across the retry.
#[test]
fn evidence_retries_a_readback_superseded_by_new_pixels() {
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
    editor.view_state.window = (1440.0, 900.0);
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.view_state.scale_factor = 2.0;
    editor.presentation.presented_reduced = true;
    editor.presentation.presented_bounds = editor.proxy_bounds();
    // Two photographs have reached the surface since the capture recorded the first.
    for _ in 0..2 {
        editor
            .presentation
            .presenter
            .show_photo(&luxforge_core::Raster {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                source_fingerprint: "f".into(),
                snapshot_id: luxforge_core::SnapshotId::new(),
            });
    }
    assert_eq!(editor.presentation.presenter.photo_version(), 2);
    let path = crate::app::testing::attach_log(&mut editor);
    let evidence = editor.evidence.as_mut().expect("evidence run");
    evidence.sync.state = Some((json!({"old":"proxy"}), 1, 1, None));
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
    editor
        .presentation
        .presenter
        .show_photo(&luxforge_core::Raster {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255].into(),
            source_fingerprint: "f".into(),
            snapshot_id: luxforge_core::SnapshotId::new(),
        });
    let photo_version = editor.presentation.presenter.photo_version();
    editor.session.workspace.clip_highlights = true;
    let generation = editor.presentation.presented_generation;
    editor.overlays.request = Some(overlay::OverlayRequest {
        generation,
        cells_w: 1,
        cells_h: 1,
        shadows: false,
        highlights: true,
        approximate: false,
    });
    assert!(
        editor
            .presentation
            .presenter
            .show_clipping(generation, vec![255; 4], (1, 1))
    );
    let first_clipping = editor.overlay_surface().unwrap().version();
    assert!(
        editor
            .presentation
            .presenter
            .show_clipping(generation, vec![0; 4], (1, 1))
    );
    assert_eq!(editor.presentation.presenter.photo_version(), photo_version);
    assert_ne!(editor.overlay_surface().unwrap().version(), first_clipping);

    let evidence = editor.evidence.as_mut().expect("evidence run");
    evidence.sync.state = Some((json!({"old":"clipping"}), 1, photo_version, None));
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

/// An ordinary session has no diagnostics log and asked for no events on stderr, so an event builds
/// nothing, not even its detail; once a log exists the same call writes its record.
#[test]
fn an_event_is_built_only_when_a_log_takes_it() {
    let (mut editor, catalog) = boot();
    assert!(editor.log.diagnostics.is_none() && !editor.log.verbose);
    editor.event("unbuilt", || {
        panic!("an event with no log built its detail")
    });
    let path = crate::app::testing::attach_log(&mut editor);
    let built = std::cell::Cell::new(false);
    editor.event(format_args!("{}_built", "event"), || {
        built.set(true);
        json!({"built": true})
    });
    assert!(built.get());
    let records = crate::app::testing::logged(&mut editor, &path);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == "event_built" && record["detail"]["built"] == true),
        "{records:?}"
    );
    finish(editor, catalog);
}

/// What an editor started from `config` asked the runtime to do at startup, in the runtime's own
/// units of work: one for each task that does something, none for `Task::none()`.
fn startup_units(config: crate::Config) -> usize {
    let (editor, startup, catalog) = boot_with(config);
    let units = startup.units();
    finish(editor, catalog);
    units
}

/// The configuration of an evidence run, which names a directory it does not create until it saves.
fn an_evidence_config() -> crate::Config {
    crate::Config {
        evidence: Some(std::env::temp_dir().join("luxforge-evidence-system-information")),
        ..crate::Config::default()
    }
}

/// Iced answers a system-information request with a walk of every process on the host, and only an
/// evidence run reads what it brings. A normal launch's startup is therefore an evidence run's
/// less exactly that request, and the request itself is the one task that asks.
#[test]
fn a_normal_launch_asks_for_no_system_information() {
    assert_eq!(system_information(false).units(), 0);
    let request = system_information(true).units();
    assert_eq!(request, 1, "the request is one task");
    assert_eq!(
        startup_units(crate::Config::default()) + request,
        startup_units(an_evidence_config()),
        "a normal startup carries every task an evidence startup does but the request",
    );
}

/// An evidence run's frames, its capture gate and its `backend` event all read the graphics
/// backend, so its startup still asks for it.
#[test]
fn an_evidence_launch_still_asks_for_the_graphics_backend() {
    assert_eq!(system_information(true).units(), 1);
    assert!(
        startup_units(an_evidence_config()) > startup_units(crate::Config::default()),
        "an evidence startup asks for what a normal one does not"
    );
}

/// With nothing asking, the backend stays unknown and every state reads it as `null`; the answer to
/// an evidence run's request starts an enumeration of its backend off the update loop, whose answer
/// records the backend and the adapter, with what the enumeration found of it, and the state
/// carries it.
#[test]
fn the_graphics_backend_stays_unknown_until_an_evidence_run_reads_it() {
    let (mut editor, catalog) = boot();
    assert!(editor.activity.backend.is_none());
    assert_eq!(editor.snapshot()["backend"], Value::Null);
    let information = iced::system::Information {
        system_name: None,
        system_kernel: None,
        system_version: None,
        system_short_version: None,
        cpu_brand: String::new(),
        cpu_cores: None,
        memory_total: 0,
        memory_used: None,
        graphics_backend: "Metal".into(),
        graphics_adapter: "Test adapter".into(),
    };
    let _ = editor.update(Message::Evidence(EvidenceMessage::Info(
        information.clone(),
    )));
    assert!(
        editor.activity.backend.is_none(),
        "the capture waits for the adapter's identity"
    );
    // The enumeration the task runs, handed back as the runtime does: this host has no adapter of
    // that name, so only Iced's names are recorded.
    let adapter = crate::app::renderer::identify("Metal", "Test adapter");
    assert_eq!(adapter, None);
    let _ = editor.update(Message::Evidence(EvidenceMessage::Adapter(Box::new((
        information,
        adapter,
    )))));
    let named = json!({"backend": "Metal", "adapter": "Test adapter", "device_type": null,
        "software": null, "vendor": null, "device": null, "driver": null, "driver_info": null});
    assert_eq!(editor.activity.backend, Some(named.clone()));
    assert_eq!(editor.snapshot()["backend"], named);
    finish(editor, catalog);
}

/// A `type` curve step types one coordinate into the open Points list and presses Enter in it,
/// exactly as the list's field does: it commits that one coordinate and nothing else, a list that
/// is closed has no field to type into, and a coordinate that would make the curve decrease is
/// refused with the kind's own text and sends nothing.
#[test]
fn an_evidence_type_step_commits_one_coordinate() {
    const ACTION: &str = "set-curve";
    const PARAMETER: &str = "luminance";
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-curve-type-{}-{}.sqlite",
        std::process::id(),
        tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
    let four = json!([[0.0, 0.0], [0.25, 0.25], [0.75, 0.75], [1.0, 1.0]]);
    let revision = editor.document.state.as_ref().unwrap().revision;
    let refreshed = tasks::command_now(
        &editor.owner,
        editor.client,
        asset.clone(),
        "edit.set-curve",
        json!({"asset_id": asset, PARAMETER: four, "mutation": tasks::mutation(revision)}),
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    assert_eq!(editor.control_field_value(ACTION, PARAMETER), Some(four));
    crate::app::testing::attach_script(
        &mut editor,
        r#"[
            {"curve":{"action":"set-curve","parameter":"luminance","event":"type","index":1,"axis":1,"text":"0.2"}},
            {"curve":{"action":"set-curve","parameter":"luminance","event":"points","open":true}},
            {"curve":{"action":"set-curve","parameter":"luminance","event":"type","index":1,"axis":1,"text":"0.2"}},
            {"curve":{"action":"set-curve","parameter":"luminance","event":"type","index":2,"axis":1,"text":"0.1"}}
        ]"#,
    );
    let status = |editor: &Editor| {
        crate::app::testing::evidence(editor)
            .current
            .clone()
            .expect("a step record")["status"]
            .clone()
    };

    // The list starts closed, so there is no field to type into.
    let _ = editor.next_step();
    assert_eq!(status(&editor), json!("failed"));
    assert!(!editor.busy, "nothing was sent");

    // Opening it is view state: it sends nothing and is captured at once.
    let _ = editor.next_step();
    assert_eq!(status(&editor), json!("sent"));
    assert!(!editor.busy, "opening the list sends nothing");
    assert!(crate::app::testing::evidence(&editor).capture_pending);

    // One coordinate typed and entered: the request carries the four points with only point 1's
    // output changed.
    let _ = editor.next_step();
    assert_eq!(status(&editor), json!("sent"));
    assert!(editor.busy, "Enter commits at once");
    let request = editor
        .request_for(ACTION, Some(PARAMETER))
        .expect("the control's request");
    assert_eq!(request["method"], json!("edit.set-curve"));
    let typed = json!([[0.0, 0.0], [0.25, 0.2], [0.75, 0.75], [1.0, 1.0]]);
    assert_eq!(request["params"][PARAMETER], typed);
    let refreshed = tasks::command_now(
        &editor.owner,
        editor.client,
        asset.clone(),
        "edit.set-curve",
        request["params"].clone(),
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    let state = editor.document.state.as_ref().unwrap();
    assert_eq!(
        state.revision,
        revision + 2,
        "one entry for the typed coordinate"
    );
    assert_eq!(state.current_entry.label, "Tone curve 4 points");
    assert_eq!(editor.control_field_value(ACTION, PARAMETER), Some(typed));
    assert!(!editor.busy);

    // An output below its left neighbour's would make the curve decrease: refused with the kind's
    // text, and nothing is sent.
    let _ = editor.next_step();
    let record = crate::app::testing::evidence(&editor)
        .current
        .clone()
        .expect("a step record");
    assert_eq!(record["status"], json!("failed"));
    assert!(
        record["reason"].as_str().is_some_and(|reason| reason
            .starts_with("the curve coordinate was not committed: ")
            && reason.contains("luminance")),
        "{record}"
    );
    assert!(!editor.busy, "nothing was sent");
    assert_eq!(
        editor.document.state.as_ref().unwrap().revision,
        revision + 2
    );
    finish(editor, catalog);
}
