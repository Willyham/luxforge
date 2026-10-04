//! The desktop's export flow: the title bar's button and menu, the refusals that keep one export
//! per window, the status line each answer leaves, and the read timer that exists only while a job
//! is live. The owner's answers are handed in as the messages the runtime delivers.
use super::{
    export::{ExportChoice, exported_text, refused_text},
    gesture,
    message::{export::ExportMessage, view::ViewMessage},
    tasks::CallError,
    testing::{
        boot, derive_ran, descriptors, finish, idle_workers, mark_no_derive, opened_with_modules,
    },
    *,
};
use crate::state::MenuTarget;
use crate::state::palette::PaletteAction;

fn start(editor: &mut Editor, keep_metadata: bool) {
    let _ = editor.update(Message::Export(ExportMessage::Start { keep_metadata }));
}

/// The plan answered and the dialog chose `/tmp/<name>`: the status names the file, and
/// `export.jpeg` is what the returned task would send.
fn chosen(editor: &mut Editor, name: &str) {
    let state = editor
        .document
        .state
        .as_ref()
        .expect("a photograph is open");
    let choice = ExportChoice {
        asset_id: state.asset.id.clone(),
        entry_id: state.current_entry.id.clone(),
        destination: std::path::PathBuf::from("/tmp").join(name),
        keep_metadata: false,
        plan: json!({"suggested": null}),
    };
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(Some(Box::new(
        choice,
    ))))));
    assert_eq!(editor.status.text, format!("Exporting {name}\u{2026}"));
    assert!(!editor.view_state.picker_open);
}

#[test]
fn with_no_photograph_export_is_disabled_and_refused() {
    let (mut editor, catalog) = boot();
    assert!(!editor.workspace.title.can_export);
    start(&mut editor, false);
    assert!(editor.export.run.is_none());
    assert_eq!(editor.status.text, "Open a photograph to export it");
    assert!(editor.export_poll_subscription().is_none());
    finish(editor, catalog);
}

/// Export's busy half is the one refusal's, and the Export button's enabled state is the same
/// answer as the press: refused and disabled while a request is in flight, but neither for an open
/// draft, since an export writes the displayed entry the draft does not change.
#[test]
fn export_refusal_and_the_menu_agree() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let agree = |editor: &mut Editor, case: &str| {
        editor.rederive();
        assert_eq!(
            editor.workspace.title.can_export,
            editor.export_refusal().is_none(),
            "{case}"
        );
        editor.workspace.title.can_export
    };
    editor.busy = true;
    assert!(!agree(&mut editor, "busy"));
    assert_eq!(
        editor.export_refusal(),
        editor.gesture_refusal(gesture::Starting::Export)
    );
    start(&mut editor, false);
    assert_eq!(editor.status.text, crate::state::IN_FLIGHT);
    assert!(editor.export.run.is_none(), "nothing starts");

    editor.busy = false;
    let (action, parameter) = crate::app::testing::patch_control(&editor);
    let _ = crate::app::testing::slide(&mut editor, &action, &parameter, 25.0);
    assert!(editor.slider_gesture().is_some());
    assert!(agree(&mut editor, "a draft is open"));
    start(&mut editor, false);
    assert!(editor.export.run.is_some(), "{}", editor.status.text);
    finish(editor, catalog);
}

#[test]
fn the_button_opens_its_menu_and_an_item_starts_one_export_at_a_time() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    assert!(editor.workspace.title.can_export);
    assert!(!editor.workspace.title.export_menu_open);
    let _ = editor.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Export)));
    assert!(editor.workspace.title.export_menu_open);
    assert_eq!(editor.snapshot()["export"]["menu_open"], json!(true));

    // An item closes the menu and starts the chain: the dialog is open, so Open and Export wait.
    start(&mut editor, true);
    assert!(!editor.workspace.title.export_menu_open);
    assert!(editor.view_state.picker_open);
    assert!(!editor.workspace.title.can_export);
    assert_eq!(
        editor.export.run.as_ref().map(|run| run.keep_metadata),
        Some(true)
    );
    // Nothing is queued at the core yet, so nothing is read.
    assert!(editor.export_poll_subscription().is_none());

    // A second press while the first runs is refused and changes nothing.
    start(&mut editor, false);
    assert_eq!(editor.status.text, "An export is already running");
    assert_eq!(
        editor.export.run.as_ref().map(|run| run.keep_metadata),
        Some(true)
    );

    // Cancelling the dialog ends it.
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(None))));
    assert_eq!(editor.status.text, "Export cancelled");
    assert!(editor.export.run.is_none());
    assert!(!editor.view_state.picker_open);
    assert!(editor.workspace.title.can_export);
    finish(editor, catalog);
}

#[test]
fn a_queued_job_is_read_on_a_timer_until_it_ends_and_says_what_was_written() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    start(&mut editor, false);
    chosen(&mut editor, "photo-edited.jpg");
    let _ = editor.update(Message::Export(ExportMessage::Queued(Ok(
        json!({"job_id":"job-1","status":"queued"}),
    ))));
    assert!(editor.export_poll_subscription().is_some());
    // The first read is already in flight, so a tick asks for nothing more.
    assert!(editor.export.reading);

    // A read for a job this window no longer follows is dropped.
    let _ = editor.update(Message::Export(ExportMessage::Read {
        job_id: "job-0".into(),
        result: Ok(json!({"job_id":"job-0","status":"ready"})),
    }));
    assert!(editor.export.run.is_some());

    let _ = editor.update(Message::Export(ExportMessage::Read {
        job_id: "job-1".into(),
        result: Ok(json!({"job_id":"job-1","status":"running","progress":0.5})),
    }));
    assert!(!editor.export.reading);
    assert!(editor.export_poll_subscription().is_some());

    let _ = editor.update(Message::Export(ExportMessage::Read {
        job_id: "job-1".into(),
        result: Ok(json!({"job_id":"job-1","status":"ready","progress":1.0,
            "result":{"path":"/tmp/photo-edited.jpg","bytes":8_412_345,"width":6000,"height":4000,"metadata":[]}})),
    }));
    assert_eq!(
        editor.status.text,
        exported_text("photo-edited.jpg", 6000, 4000, 8_412_345)
    );
    assert!(editor.export.run.is_none());
    assert!(
        editor.export_poll_subscription().is_none(),
        "no timer once the job has ended"
    );
    finish(editor, catalog);
}

/// A queued export with its first read already applied: the desktop holds a running record at
/// `progress`, and the next tick's read is not yet in flight.
fn running_export(progress: f64) -> (Editor, std::path::PathBuf) {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    idle_workers(&mut editor);
    start(&mut editor, false);
    chosen(&mut editor, "photo-edited.jpg");
    let _ = editor.update(Message::Export(ExportMessage::Queued(Ok(
        json!({"job_id":"job-1","status":"queued"}),
    ))));
    let _ = editor.update(read(
        "job-1",
        json!({"status":"running","progress":progress}),
    ));
    assert!(!editor.export.reading);
    (editor, catalog)
}

fn read(job_id: &str, record: Value) -> Message {
    let mut record = record;
    record["job_id"] = json!(job_id);
    Message::Export(ExportMessage::Read {
        job_id: job_id.into(),
        result: Ok(record),
    })
}

/// While an export runs, its timer's tick only starts a read, and a read that answers the record
/// the desktop already holds changes nothing: neither runs the hooks or the derive. A read that
/// differs, and one that ends the job, take the full update and show it in the status line and
/// the title bar's Export button.
#[test]
fn an_export_poll_that_changes_nothing_skips_the_hooks_and_the_derive() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    idle_workers(&mut editor);
    start(&mut editor, false);
    chosen(&mut editor, "photo-edited.jpg");
    let _ = editor.update(Message::Export(ExportMessage::Queued(Ok(
        json!({"job_id":"job-1","status":"queued"}),
    ))));
    assert!(editor.export.reading);
    assert!(!editor.workspace.title.can_export, "an export is running");

    // The first read differs from what is held, which is nothing yet.
    let updates = editor.full_updates;
    mark_no_derive(&editor);
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.5})));
    assert_eq!(editor.full_updates, updates + 1);
    assert!(derive_ran(&editor));

    // The tick starts a read and nothing else.
    let updates = editor.full_updates;
    mark_no_derive(&editor);
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    assert!(editor.export.reading, "the read is in flight");
    assert_eq!(editor.full_updates, updates, "a tick runs no hooks");
    assert!(!derive_ran(&editor), "and no derive");
    assert_eq!(editor.log.loop_timing.get().last_rederive_ms, 0.0);

    // The same record again changes nothing.
    mark_no_derive(&editor);
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.5})));
    assert!(!editor.export.reading, "the read was applied");
    assert_eq!(
        editor.full_updates, updates,
        "an unchanged read runs no hooks"
    );
    assert!(!derive_ran(&editor), "and no derive");

    // Progress is a change: the full update runs and the record held moves on.
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    assert_eq!(editor.full_updates, updates);
    mark_no_derive(&editor);
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.75})));
    assert_eq!(
        editor.full_updates,
        updates + 1,
        "a changed read takes the full update"
    );
    assert!(derive_ran(&editor));
    assert_eq!(
        editor.export.run.as_ref().and_then(|run| run.seen.as_ref()),
        Some(&json!({"job_id":"job-1","status":"running","progress":0.75}))
    );
    let updates = editor.full_updates;
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.75})));
    assert_eq!(
        editor.full_updates, updates,
        "the new record is the one compared"
    );
    assert!(
        editor.export_poll_subscription().is_some(),
        "the timer stays while it runs"
    );

    // The end of the job is a change, and the panel and the status line show it.
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    mark_no_derive(&editor);
    let _ = editor.update(read(
        "job-1",
        json!({"status":"ready","progress":1.0,
            "result":{"path":"/tmp/photo-edited.jpg","bytes":8_412_345,"width":6000,"height":4000,"metadata":[]}}),
    ));
    assert_eq!(editor.full_updates, updates + 1);
    assert!(derive_ran(&editor));
    assert_eq!(
        editor.status.text,
        exported_text("photo-edited.jpg", 6000, 4000, 8_412_345)
    );
    assert!(
        editor.workspace.title.can_export,
        "the button follows the derive"
    );
    assert!(editor.export_poll_subscription().is_none());
    finish(editor, catalog);
}

/// A failed or cancelled job and a read that could not be made are changes too: the full update
/// ends the export and the status line says why, whatever the record held was.
#[test]
fn an_export_poll_that_ends_or_fails_takes_the_full_update() {
    for (record, status) in [
        (
            json!({"status":"failed","error":{"code":"export","message":"no space left"}}),
            "Export failed: no space left",
        ),
        (json!({"status":"cancelled"}), "Export cancelled"),
    ] {
        let (mut editor, catalog) = running_export(0.5);
        let _ = editor.update(Message::Export(ExportMessage::Poll));
        let updates = editor.full_updates;
        let _ = editor.update(read("job-1", record));
        assert_eq!(editor.full_updates, updates + 1, "{status}");
        assert_eq!(editor.status.text, status);
        assert!(editor.export.run.is_none());
        assert!(editor.workspace.title.can_export);
        finish(editor, catalog);
    }

    let (mut editor, catalog) = running_export(0.5);
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    let updates = editor.full_updates;
    let _ = editor.update(Message::Export(ExportMessage::Read {
        job_id: "job-1".into(),
        result: Err("the owner stopped".into()),
    }));
    assert_eq!(editor.full_updates, updates + 1);
    assert_eq!(editor.status.text, "Export failed: the owner stopped");
    assert!(editor.export.run.is_none());
    finish(editor, catalog);
}

/// The skip holds only while nothing else is going on: with a gesture open, the same tick and the
/// same read take the full update, as every other message does.
#[test]
fn an_export_poll_during_a_gesture_takes_the_full_update() {
    let (mut editor, catalog) = running_export(0.5);
    let (action, parameter) = crate::app::testing::patch_control(&editor);
    let _ = crate::app::testing::slide(&mut editor, &action, &parameter, 25.0);
    assert!(editor.slider_gesture().is_some());
    let updates = editor.full_updates;
    let _ = editor.update(Message::Export(ExportMessage::Poll));
    assert_eq!(editor.full_updates, updates + 1);
    let _ = editor.update(read("job-1", json!({"status":"running","progress":0.5})));
    assert_eq!(editor.full_updates, updates + 2);
    finish(editor, catalog);
}

#[test]
fn a_destination_that_exists_is_refused_in_the_status_bar() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    start(&mut editor, false);
    if let Some(run) = &mut editor.export.run {
        run.file_name = Some("photo-edited.jpg".into());
    }
    let _ = editor.update(Message::Export(ExportMessage::Queued(Err(CallError {
        code: "conflict".into(),
        message: "a file already exists at the destination".into(),
        data: None,
        job_id: None,
    }))));
    assert_eq!(editor.status.text, refused_text("photo-edited.jpg"));
    assert!(editor.export.run.is_none());

    start(&mut editor, false);
    chosen(&mut editor, "b.png");
    let _ = editor.update(Message::Export(ExportMessage::Queued(Err(CallError {
        code: "validation".into(),
        message: "the destination must end in .jpg or .jpeg".into(),
        data: None,
        job_id: None,
    }))));
    assert_eq!(
        editor.status.text,
        "Export failed: the destination must end in .jpg or .jpeg"
    );
    finish(editor, catalog);
}

#[test]
fn the_palette_lists_both_exports_for_export_jpeg() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Open,
    ));
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Query("export".into()),
    ));
    let exports: Vec<_> = editor
        .workspace
        .palette
        .entries
        .iter()
        .filter(|entry| matches!(entry.action, PaletteAction::Export { .. }))
        .map(|entry| {
            (
                entry.label.as_str(),
                entry.detail.as_str(),
                entry.action.clone(),
            )
        })
        .collect();
    assert_eq!(
        exports,
        vec![
            (
                "Export JPEG\u{2026}",
                "export.jpeg",
                PaletteAction::Export {
                    keep_metadata: false
                }
            ),
            (
                "Export JPEG, keep metadata\u{2026}",
                "export.jpeg",
                PaletteAction::Export {
                    keep_metadata: true
                }
            ),
        ]
    );
    finish(editor, catalog);
}

/// One whole export against a real owner, with the owner's answers handed in as the runtime would:
/// the plan, the chosen file, the queued job and its reads, then the same file again, refused.
#[test]
fn an_export_through_the_owner_writes_a_new_file_and_never_replaces_it() {
    use super::export::{plan_now, read_now, send_now};
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-export-{}-{}.sqlite",
        std::process::id(),
        super::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, asset, _) = super::testing::real_photo(&catalog);
    let folder = catalog.with_extension("exports");
    std::fs::create_dir_all(&folder).unwrap();
    let destination = folder.join("photo-edited.jpg");
    let entry = editor.displayed_entry().unwrap();

    let export_once = |editor: &mut Editor| {
        let _ = editor.export_start(true, Some(destination.clone()));
        assert!(editor.export.active(), "{}", editor.status.text);
        let plan = plan_now(&editor.owner, editor.client, &asset, &entry).unwrap();
        assert_eq!(plan["entry_id"], json!(entry));
        let choice = ExportChoice {
            asset_id: asset.clone(),
            entry_id: entry.clone(),
            destination: destination.clone(),
            keep_metadata: true,
            plan,
        };
        let queued = send_now(&editor.owner, editor.client, &choice);
        let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(Some(Box::new(
            choice,
        ))))));
        let _ = editor.update(Message::Export(ExportMessage::Queued(queued)));
        luxforge_testbase::wait_until("the export ends", || {
            let Some(job) = editor
                .export
                .run
                .as_ref()
                .and_then(|run| run.job_id.clone())
            else {
                return true;
            };
            let result = read_now(&editor.owner, editor.client, &job);
            let _ = editor.update(Message::Export(ExportMessage::Read {
                job_id: job,
                result,
            }));
            false
        });
    };

    export_once(&mut editor);
    let written = std::fs::read(&destination).unwrap();
    let decoded = image::load_from_memory(&written).unwrap();
    let expected = format!(
        "Exported photo-edited.jpg \u{b7} {} \u{d7} {} \u{b7} ",
        decoded.width(),
        decoded.height()
    );
    assert!(
        editor.status.text.starts_with(&expected),
        "{}",
        editor.status.text
    );

    // The same file again is refused, and the file is exactly as it was.
    export_once(&mut editor);
    assert_eq!(editor.status.text, refused_text("photo-edited.jpg"));
    assert_eq!(std::fs::read(&destination).unwrap(), written);
    assert!(editor.export.run.is_none());

    std::fs::remove_dir_all(&folder).unwrap();
    finish(editor, catalog);
}

/// A real slider commit must be the After raster and the JPEG target, including while the
/// comparison session selects the Original for its Before surface.
#[test]
fn a_slider_edit_is_compared_and_exported_while_before_is_selected() {
    use super::{
        export::{plan_now, send_now},
        message::{history::HistoryMessage, preview::PreviewMessage},
        testing::{let_go, real_photo, run_commit, slide},
    };
    use luxforge_core::PreviewRequest;

    let dir = luxforge_testbase::paths::temp_dir("compare-export-edited");
    let catalog = dir.join("catalog.sqlite");
    let (mut editor, asset, _) = real_photo(&catalog);
    let settle = |editor: &mut Editor| {
        luxforge_testbase::wait_until("the saved entry's exact frame", || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            editor.presentation.exact().is_some()
                && editor.presentation.presented_entry == editor.document.display_entry
                && editor.presentation.displayed_draft_id.is_none()
        });
    };
    settle(&mut editor);
    let original = editor.presentation.exact().unwrap().raster.clone();
    let original_entry = editor.displayed_entry().unwrap();
    let _ = slide(&mut editor, "set-basic", "exposure", 0.8);
    let _ = let_go(&mut editor, "set-basic", "exposure");
    assert!(run_commit(&mut editor));
    settle(&mut editor);
    let edited = editor.presentation.exact().unwrap().raster.clone();
    let edited_entry = editor.displayed_entry().unwrap();
    assert_ne!(edited_entry, original_entry);
    assert_ne!(edited.rgba, original.rgba, "the commit changes real pixels");

    let _ = editor.update(Message::History(HistoryMessage::CompareToggle));
    let after = editor.presentation.compare_after.as_ref().unwrap();
    assert_eq!(after.full().pixels(), edited.rgba.as_slice());
    let before_entry = editor
        .session
        .preview
        .selected_entry(&asset)
        .unwrap()
        .clone();
    assert_eq!(before_entry, original_entry);
    let job = editor
        .owner
        .preview_job(
            PreviewRequest::new(editor.client, asset.clone())
                .entry(Some(before_entry))
                .analyse(),
        )
        .unwrap();
    let session = editor.session.clone();
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload { job, session },
    )))));
    settle(&mut editor);
    assert_eq!(
        editor.presentation.exact().unwrap().raster.rgba,
        original.rgba
    );
    assert_eq!(
        editor
            .presentation
            .compare_after
            .as_ref()
            .unwrap()
            .full()
            .pixels(),
        edited.rgba.as_slice()
    );

    let destination = dir.join("edited.jpg");
    let _ = editor.export_start(false, Some(destination.clone()));
    assert!(editor.export.active());
    let target = editor.export_entry().unwrap();
    let plan = plan_now(&editor.owner, editor.client, &asset, &target).unwrap();
    assert_eq!(
        plan["entry_id"],
        json!(edited_entry),
        "comparison exports After"
    );
    let choice = ExportChoice {
        asset_id: asset,
        entry_id: target,
        destination: destination.clone(),
        keep_metadata: false,
        plan,
    };
    let queued = send_now(&editor.owner, editor.client, &choice).unwrap();
    let job = queued["job_id"].as_str().unwrap();
    luxforge_testbase::wait_until("the edited JPEG", || {
        let read = super::export::read_now(&editor.owner, editor.client, job).unwrap();
        if read["status"] == "ready" {
            return true;
        }
        assert!(
            matches!(read["status"].as_str(), Some("queued" | "running")),
            "{read}"
        );
        false
    });
    let decoded = image::open(destination).unwrap().to_rgba8();
    let delta = |reference: &luxforge_core::Raster| -> u64 {
        decoded
            .as_raw()
            .iter()
            .zip(reference.rgba.iter())
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum()
    };
    assert!(
        delta(&edited) < delta(&original),
        "JPEG pixels follow the edit"
    );
    finish(editor, catalog);
    std::fs::remove_dir_all(dir).unwrap();
}
