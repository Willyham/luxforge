//! What the canvas shows when the preview of the displayed state fails.
//!
//! A commit whose render fails leaves history and the recipe naming the edit, so the picture from
//! before the edit must not stay on screen as though it were the edit's result — which is how a
//! RAW crop came to read "Crop 7°" in history over an uncropped photograph when its render was
//! refused. These tests drive the real preview queue: the failing job is a JPEG source whose
//! declared size is over the 512 MiB frame limit, so the worker answers `resource-limit` exactly as
//! a refused render does.
use super::{
    Editor, ProxyFrame,
    crop::PendingDraft,
    message::Message,
    testing::{attach_log, crop_layer, entry, finish, logged, opened, refresh_for},
};
use crate::state::{canvas::PhotoView, histogram::HistogramStatus};
use lightwell_core::{
    AssetId, CropPayload, CropStage, EntryId, Error, ErrorKind, HistoryEntry, POINTER_MODE,
    PreviewSource, SourceImage, Zoom,
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
        approximate: false,
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
    editor.crop_pending = Some(PendingDraft {
        layer: None,
        layer_index: 0,
        payload: None,
        base_revision: 4,
        reapply: false,
    });
    editor.draft_generation = Some(99);
    editor.draft_preview_failed(&error);
    assert!(editor.crop_pending.is_none() && editor.draft_generation.is_none());
    assert!(editor.crop.is_none());
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
    editor.crop = Some(crate::crop_draft::CropDraft::neutral(stage, 4, 0));
    editor.crop_pending = Some(PendingDraft {
        layer: None,
        layer_index: 0,
        payload: None,
        base_revision: 5,
        reapply: true,
    });
    editor.draft_generation = Some(100);
    editor.draft_preview_failed(&error);
    assert!(
        editor.crop.is_some(),
        "a failed reapply discarded the draft"
    );
    assert!(editor.crop_pending.is_none() && editor.draft_generation.is_none());
    finish(editor, catalog);
}
