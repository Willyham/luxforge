//! What the canvas shows when the preview of the displayed state fails.
//!
//! A commit whose render fails leaves history and the recipe naming the edit, so the picture from
//! before the edit must not stay on screen as though it were the edit's result — which is how a
//! RAW crop came to read "Crop 7°" in history over an uncropped photograph when its render was
//! refused. These tests drive the real preview queue: the failing job is a JPEG source whose
//! declared size is over the 512 MiB frame limit, so the worker answers `resource-limit` exactly as
//! a refused render does.
use super::{
    Editor, ProxyFrame, Settle,
    crop::PendingDraft,
    message::{CropMessage, Message},
    tasks::SyncResult,
    testing::{attach_log, crop_layer, entry, finish, logged, opened, refresh_for},
};
use crate::state::{canvas::PhotoView, histogram::HistogramStatus};
use lightwell_core::{
    AssetId, BASIC_EFFECT, CropPayload, CropStage, EFFECT_FORMAT, EntryId, Error, ErrorKind,
    HistoryEntry, Layer, LayerId, POINTER_MODE, PreviewJob, PreviewSource, SourceImage, Zoom,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// Deliver worker results until `done` holds. The deadline is generous: these tests assert what is
/// shown, never how fast.
fn poll_until(editor: &mut Editor, what: &str, done: impl Fn(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(editor) {
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {}",
            editor.status
        );
        let _ = editor.update(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A committed entry that straightens and crops the one before it.
fn cropped(asset: &AssetId, parent: &HistoryEntry, sequence: u64) -> HistoryEntry {
    let mut next = entry(asset, sequence, Some(&parent.id));
    next.label = "Crop 7°".into();
    next.action_id = "crop".into();
    next.snapshot = parent
        .snapshot
        .append(crop_layer(CropPayload {
            angle: 7.0,
            x: 0.2,
            y: 0.2,
            width: 0.6,
            height: 0.6,
        }))
        .expect("a valid stack");
    next
}

/// The refresh a commit of `current` brings back, whose preview job renders `source`.
fn committed(
    asset: &AssetId,
    current: &HistoryEntry,
    older: &[&HistoryEntry],
    source: SourceImage,
) -> super::tasks::Refresh {
    let mut page = vec![current.clone()];
    page.extend(older.iter().map(|entry| (*entry).clone()));
    let mut lineage = vec![current];
    lineage.extend(older.iter().copied());
    let mut refresh = refresh_for(asset, current, page, &lineage, false);
    refresh.job.source = PreviewSource::Jpeg(source);
    refresh
}

/// A small photograph the crop above covers at 7°.
fn small() -> SourceImage {
    SourceImage {
        width: 64,
        height: 48,
        rgba: [10, 20, 30, 255].repeat(64 * 48).into(),
        fingerprint: "f".into(),
        orientation: 1,
    }
}

/// A source the renderer refuses before allocating anything: its declared frame is 2.2 GiB.
fn over_the_frame_limit() -> SourceImage {
    SourceImage {
        width: 30_000,
        height: 20_000,
        rgba: vec![0, 0, 0, 255].into(),
        fingerprint: "f".into(),
        orientation: 1,
    }
}

fn opened_and_shown() -> (Editor, PathBuf, AssetId, HistoryEntry) {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 4);
    poll_until(&mut editor, "the opened entry's frame", |editor| {
        editor.presented_entry.as_ref() == Some(&entry_id)
    });
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    (editor, catalog, asset, current)
}

fn events<'a>(records: &'a [Value], name: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["event"] == json!(name))
        .map(|record| &record["detail"])
        .collect()
}

/// A crop that committed but whose render failed: the uncropped picture is withdrawn rather than
/// left under history's "Crop 7°", the canvas and the histogram say why, a zoom hands nothing over
/// in its place, and the next frame that renders puts a picture back.
#[test]
fn a_commit_whose_render_fails_withdraws_the_earlier_picture_instead_of_presenting_it() {
    let (mut editor, catalog, asset, original) = opened_and_shown();
    let log = attach_log(&mut editor);
    let version = editor.photo_version;
    assert!(editor.photo.is_some(), "the opened frame is on screen");

    let crop = cropped(&asset, &original, 5);
    let refresh = committed(&asset, &crop, &[&original], over_the_frame_limit());
    let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
    assert_eq!(
        editor.state.as_ref().expect("open").current_entry.id,
        crop.id,
        "history names the crop"
    );
    poll_until(&mut editor, "the crop's failure", |editor| {
        editor.render_error.is_some()
    });

    assert_eq!(
        editor.render_error.as_ref().map(|(kind, _)| *kind),
        Some(ErrorKind::ResourceLimit)
    );
    assert!(
        editor.photo.is_none(),
        "the uncropped picture is still on the surface under the crop's entry"
    );
    assert_eq!(editor.presented_entry, None);
    assert_eq!(
        editor.stack_summary()["displayed"],
        Value::Null,
        "the evidence names a displayed entry although nothing is shown"
    );
    assert!(editor.raster.is_none() && editor.proxy_frame.is_none() && editor.analysis.is_none());
    assert!(
        editor.status.starts_with("resource-limit: "),
        "{}",
        editor.status
    );
    let canvas = &editor.workspace.canvas;
    match &canvas.photo {
        PhotoView::Empty(line) => assert!(
            line.starts_with("Preview unavailable: ") && line.contains("512 MiB"),
            "{line}"
        ),
        other => panic!("the canvas still draws a photograph: {other:?}"),
    }
    assert!(
        canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Rendering limit"),
        "{:?}",
        canvas.notices
    );
    assert_eq!(
        editor.workspace.histogram.status,
        HistogramStatus::Unavailable
    );

    // A zoom either way hands nothing retained over in the picture's place and asks for no render.
    let generation = editor.preview_generation;
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    editor.session.preview.view.zoom = Zoom::Fit;
    let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert!(editor.photo.is_none(), "a zoom put a stale picture back");
    assert_eq!(editor.photo_version, version, "nothing was handed over");
    assert_eq!(
        editor.preview_generation, generation,
        "nothing was asked for"
    );
    assert!(editor.render_error.is_some(), "the failure is still shown");

    // The next state that renders is shown, and the failure with it is over.
    let mut next = entry(&asset, 6, Some(&crop.id));
    next.snapshot = crop.snapshot.clone();
    let refresh = committed(&asset, &next, &[&crop, &original], small());
    let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
    poll_until(&mut editor, "the next frame", |editor| {
        editor.photo.is_some()
    });
    assert_eq!(editor.presented_entry.as_ref(), Some(&next.id));
    assert_eq!(editor.render_error, None);
    assert_eq!(editor.workspace.canvas.photo, PhotoView::Plain);

    let records = logged(&mut editor, &log);
    let failed = events(&records, "preview_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["entry_id"], json!(crop.id));
    assert_eq!(failed[0]["error_code"], json!("resource-limit"));
    let withdrawn = events(&records, "preview_withdrawn");
    assert_eq!(withdrawn.len(), 1, "{withdrawn:?}");
    assert_eq!(withdrawn[0]["target_entry"], json!(crop.id));
    assert_eq!(withdrawn[0]["withdrawn_entry"], json!(original.id));
    assert_eq!(withdrawn[0]["error_code"], json!("resource-limit"));
    let displayed: Vec<&Value> = events(&records, "preview_displayed");
    assert!(
        displayed
            .iter()
            .all(|detail| detail["entry_id"] != json!(crop.id)),
        "a frame was presented as the crop's: {displayed:?}"
    );
    finish(editor, catalog);
}

/// The full-resolution phase of the state on screen failed after its display proxy was shown: the
/// proxy is that state's own picture, so it stays, and the failure is still named.
#[test]
fn a_failed_exact_phase_keeps_the_proxy_of_the_same_state() {
    let (mut editor, catalog, _, current) = opened_and_shown();
    let presented = editor.presented_generation;
    let error = Error::new(ErrorKind::ResourceLimit, "linear output exceeds 512 MiB");
    editor.preview_failed(presented, false, &current.id, None, &error);
    editor.rederive();
    assert!(
        editor.photo.is_some(),
        "the target's own proxy was withdrawn"
    );
    assert_eq!(editor.presented_entry.as_ref(), Some(&current.id));
    assert_eq!(editor.workspace.canvas.photo, PhotoView::Plain);
    assert!(
        editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Rendering limit")
    );

    // A drafted revision of the same entry is another picture: its failure withdraws the frame.
    editor.preview_failed(presented + 1, false, &current.id, Some(3), &error);
    assert!(editor.photo.is_none(), "a frame of another revision stayed");
    finish(editor, catalog);
}

/// A zoom while a newer entry is rendering hands over the picture on screen under that picture's
/// own entry, and leaves the entry the desktop is waiting for as the one it asked for.
#[test]
fn a_zoom_hands_over_the_retained_picture_under_its_own_entry() {
    let (mut editor, catalog, asset, current) = opened_and_shown();
    editor.window = (1440.0, 900.0);
    editor.dimensions = Some((4000, 3000));
    editor.session.preview.view.zoom = Zoom::Fit;
    let raster = |code: u8| {
        Arc::new(lightwell_core::Raster {
            width: 2,
            height: 2,
            rgba: vec![code; 16].into(),
            source_fingerprint: "f".into(),
            snapshot_id: current.snapshot.id.clone(),
        })
    };
    let generation = editor.presented_generation;
    editor.presented_proxy = true;
    editor.proxy_frame = Some(ProxyFrame {
        generation,
        raster: raster(1),
        dimensions: (1200, 900),
        built: false,
        approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        render_ms: 5.0,
    });
    editor.raster = Some((generation, raster(2)));
    // The next entry is committed and asked for; its frame has not arrived.
    let next: EntryId = cropped(&asset, &current, 5).id;
    editor.display_entry = Some(next.clone());
    let log = attach_log(&mut editor);

    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    assert!(
        !editor.presented_proxy,
        "the retained exact raster is on screen"
    );
    assert_eq!(editor.presented_entry.as_ref(), Some(&current.id));
    assert_eq!(
        editor.display_entry.as_ref(),
        Some(&next),
        "the hand-over moved the requested entry back"
    );
    let records = logged(&mut editor, &log);
    let displayed = events(&records, "preview_displayed");
    assert_eq!(displayed.len(), 1, "{displayed:?}");
    assert_eq!(displayed[0]["entry_id"], json!(current.id));
    assert_eq!(displayed[0]["snapshot_id"], json!(current.snapshot.id));
    assert_eq!(displayed[0]["reason"], json!("zoom"));
    finish(editor, catalog);
}

/// The crop layer's input stage could not be rendered: a start that was waiting ends in the pointer
/// mode with the reason in the status bar, while the photograph — the current state — stays; a
/// reapply keeps the draft it was rebasing.
#[test]
fn a_draft_whose_input_stage_fails_ends_explicitly_and_keeps_the_photograph() {
    let (mut editor, catalog, _, _) = opened_and_shown();
    let error = Error::new(ErrorKind::ResourceLimit, "linear output exceeds 512 MiB");
    editor.set_crop_pending(Some(PendingDraft {
        layer: None,
        layer_index: 0,
        ahead: lightwell_core::Orientation::NEUTRAL,
        payload: None,
        base_revision: 4,
        reapply: false,
        queued: Vec::new(),
    }));
    editor.draft_generation = Some(99);
    editor.draft_preview_failed(&error);
    assert!(editor.crop_pending().is_none() && editor.draft_generation.is_none());
    assert!(editor.crop().is_none());
    assert_eq!(editor.mode_sync.as_deref(), Some(POINTER_MODE));
    assert!(
        editor
            .status
            .starts_with("The crop's input stage could not be rendered: resource-limit"),
        "{}",
        editor.status
    );
    assert_eq!(
        editor.render_error, None,
        "the photograph's own state did not fail"
    );
    assert!(editor.photo.is_some());

    let stage = CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    };
    editor.set_crop(Some(crate::crop_draft::CropDraft::neutral(stage, 4, 0)));
    editor.set_crop_pending(Some(PendingDraft {
        layer: None,
        layer_index: 0,
        ahead: lightwell_core::Orientation::NEUTRAL,
        payload: None,
        base_revision: 5,
        reapply: true,
        queued: Vec::new(),
    }));
    editor.draft_generation = Some(100);
    editor.draft_preview_failed(&error);
    assert!(
        editor.crop().is_some(),
        "a failed reapply discarded the draft"
    );
    assert!(editor.crop_pending().is_none() && editor.draft_generation.is_none());
    finish(editor, catalog);
}

/// A scripted step waiting for the newest preview's pixels ends on that preview's failure, so the
/// evidence captures the failure instead of waiting out its deadline; an older job's failure does
/// not end it.
#[test]
fn a_scripted_step_waiting_for_a_preview_ends_on_its_failure() {
    let steps = json!([{"wait": {"ms": 1}}]).to_string();
    let (mut editor, catalog, _, _) = crate::app::testing::scripted(&steps);
    let error = Error::new(ErrorKind::ResourceLimit, "linear output exceeds 512 MiB");
    let entry = EntryId::new();
    editor.preview_generation = 9;
    if let Some(evidence) = editor.evidence.as_mut() {
        evidence.awaiting = Some(Settle::Preview);
        evidence.capture_pending = false;
    }
    editor.preview_failed(8, false, &entry, None, &error);
    let evidence = crate::app::testing::evidence(&editor);
    assert_eq!(
        evidence.awaiting,
        Some(Settle::Preview),
        "an older job's failure ended the step"
    );
    editor.preview_failed(9, false, &entry, None, &error);
    let evidence = crate::app::testing::evidence(&editor);
    assert!(evidence.awaiting.is_none() && evidence.capture_pending);
    finish(editor, catalog);
}

/// A layer the crop's input stage renders, so that stage costs a real colour pass.
fn basic() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 0.5, "contrast": 20.0}),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// A photograph whose input stage is still rendering when the next request arrives: its colour
/// pass takes over half a second in the unoptimized test build, and the update that supersedes it
/// arrives within a millisecond.
fn large() -> SourceImage {
    SourceImage {
        width: 4000,
        height: 3000,
        rgba: [90, 110, 130, 255].repeat(4000 * 3000).into(),
        fingerprint: "f".into(),
        orientation: 1,
    }
}

/// The job `crop_preview_task` hands back for the starting draft: the current stack truncated to
/// the layers before the crop, over `source`.
fn draft_job(editor: &Editor, source: SourceImage) -> PreviewJob {
    let state = editor.state.as_ref().expect("an open asset");
    let current = &state.current_entry;
    let pending = editor.crop_pending().expect("a starting draft");
    let mut job = refresh_for(&state.asset.id, current, Vec::new(), &[current], false).job;
    job.source = PreviewSource::Jpeg(source);
    job.layer_count = Some(pending.layer_index);
    job
}

/// Another client commits: the event sync reads the state back and requests the new entry's frame.
fn committed_elsewhere(
    editor: &mut Editor,
    asset: &AssetId,
    sequence: u64,
    source: SourceImage,
) -> HistoryEntry {
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    let mut next = entry(asset, sequence, Some(&current.id));
    next.snapshot = current.snapshot.clone();
    let refresh = committed(asset, &next, &[&current], source);
    let _ = editor.update(Message::Synced(Ok(SyncResult::changed(refresh))));
    next
}

/// [`poll_until`] through `dispatch`, so the mode a draft's end asks the session for is still
/// there to read afterwards rather than folded into a task this test never runs.
fn dispatch_polls_until(editor: &mut Editor, what: &str, done: impl Fn(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(editor) {
        assert!(
            Instant::now() < deadline,
            "{what} never happened: {}",
            editor.status
        );
        let _ = editor.dispatch(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A starting draft's input stage is still rendering when another client's commit requests the
/// new entry's frame, which stops the draft's job: the job's cancelled outcome is recorded under
/// the draft's own generation, and the draft ends explicitly, back in the pointer mode with the
/// reason in the status bar, instead of waiting for pixels that will never come. The new entry's
/// frame is then shown as usual, and its own status replaces the reason.
#[test]
fn a_starting_draft_whose_input_stage_a_newer_request_cancels_ends_explicitly() {
    let (mut editor, catalog, asset, _) = opened(vec![basic()], 4);
    poll_until(&mut editor, "the opened frame", |editor| {
        editor.presented_generation > 0 && !editor.preview_queue.is_busy()
    });
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let job = draft_job(&editor, large());
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    let draft = editor
        .draft_generation
        .expect("the draft's job was requested");
    assert_eq!(editor.status, "Rendering the crop's input stage…");

    // The new entry is as large, so its frame is still rendering when the draft ends.
    let next = committed_elsewhere(&mut editor, &asset, 5, large());
    let newer = editor.preview_generation;
    assert!(newer > draft);
    dispatch_polls_until(&mut editor, "the draft's end", |editor| {
        editor.crop_pending().is_none()
    });
    assert_eq!(editor.draft_generation, None);
    assert!(editor.crop().is_none() && editor.draft_photo.is_none());
    assert_eq!(
        editor.mode_sync.as_deref(),
        Some(POINTER_MODE),
        "the session was not asked to leave the crop mode"
    );
    assert_eq!(
        editor.status,
        "The crop's input stage was superseded by a newer preview: start the crop again"
    );

    dispatch_polls_until(&mut editor, "the new entry's frame", |editor| {
        editor.presented_generation == newer && !editor.preview_queue.is_busy()
    });
    assert!(!editor.uploading, "nothing of the draft was uploaded");
    assert!(editor.crop_pending().is_none() && editor.draft_photo.is_none());
    assert_eq!(editor.presented_entry.as_ref(), Some(&next.id));
    assert_eq!(editor.render_error, None);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "preview_exact_cancelled"),
        vec![&json!({"generation": draft, "draft": true})],
        "the cancelled phase is recorded as the draft's"
    );
    assert_eq!(
        events(&records, "crop_draft_failed"),
        vec![&json!({
            "reapply": false,
            "error_code": "cancelled",
            "detail": "superseded by a newer preview",
            "generation": draft,
        })]
    );
    finish(editor, catalog);
}

/// A reapply's input stage waits in the pending slot behind the frame of one commit from another
/// client when a second commit's frame replaces it there, so it never starts: the reapply ends as
/// it is replaced, and keeps the conflicted draft it was rebasing for another reapply. The status
/// bar goes straight on to the second commit's frame, which is what the photograph is waiting for;
/// the draft's own notice still says it changed elsewhere.
#[test]
fn a_reapply_whose_input_stage_a_newer_request_replaces_keeps_the_conflicted_draft() {
    let (mut editor, catalog, asset, _) = opened(vec![basic()], 4);
    poll_until(&mut editor, "the opened frame", |editor| {
        editor.presented_generation > 0 && !editor.preview_queue.is_busy()
    });
    let _ = editor.update(Message::Crop(CropMessage::Start));
    editor.open_draft(CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    });
    // The first commit makes the draft conflicted; its frame is still rendering.
    committed_elsewhere(&mut editor, &asset, 5, large());
    assert!(editor.crop().expect("the draft is kept").conflicted);
    let _ = editor.update(Message::Crop(CropMessage::Reapply));
    assert!(editor.crop_pending().expect("a pending rebase").reapply);
    let log = attach_log(&mut editor);
    let job = draft_job(&editor, large());
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    let reapply = editor
        .draft_generation
        .expect("the reapply's job was requested");
    assert_eq!(
        editor.preview_queue.pending_generation(),
        Some(reapply),
        "the reapply's job waits behind the first commit's frame"
    );

    let next = committed_elsewhere(&mut editor, &asset, 6, small());
    assert!(
        editor.crop_pending().is_none() && editor.draft_generation.is_none(),
        "the replaced reapply is still waiting"
    );
    let draft = editor.crop().expect("the reapply kept its draft");
    assert!(draft.conflicted, "the kept draft is still conflicted");
    assert_eq!(draft.base_revision, 4);

    poll_until(&mut editor, "the second commit's frame", |editor| {
        editor.presented_entry.as_ref() == Some(&next.id) && !editor.preview_queue.is_busy()
    });
    assert!(!editor.uploading, "the replaced job delivered a stage");
    assert!(editor.crop().expect("the draft is kept").conflicted);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "crop_draft_failed"),
        vec![&json!({
            "reapply": true,
            "error_code": "cancelled",
            "detail": "superseded by a newer preview",
            "generation": reapply,
        })]
    );
    assert!(
        events(&records, "preview_exact_cancelled")
            .iter()
            .all(|detail| detail["generation"] != json!(reapply)),
        "a job that never started was cancelled: {records:?}"
    );
    finish(editor, catalog);
}

/// The stack changed while the owner planned the draft's job: the job it answers truncates an entry
/// that is not the current one the desktop holds, so its input stage is stale and the draft ends
/// explicitly as a cancelled one does, without a render — decided from the answer itself, with no
/// currency request of its own.
#[test]
fn a_draft_whose_job_the_owner_finds_superseded_ends_explicitly() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let mut job = draft_job(&editor, small());
    job.entry = entry(&asset, 5, Some(&job.entry.id));
    let _ = editor.dispatch(Message::Crop(CropMessage::PreviewReady(Ok(Box::new(job)))));
    assert_eq!(
        editor.draft_generation, None,
        "the stale stage was not requested"
    );
    assert!(editor.crop_pending().is_none() && editor.crop().is_none());
    assert_eq!(editor.mode_sync.as_deref(), Some(POINTER_MODE));
    assert!(
        editor.status.ends_with("start the crop again"),
        "{}",
        editor.status
    );
    let records = logged(&mut editor, &log);
    let failed = events(&records, "crop_draft_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["generation"], Value::Null);
    finish(editor, catalog);
}
