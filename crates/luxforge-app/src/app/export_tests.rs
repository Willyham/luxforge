//! The desktop's export flow: the title bar's button and menu, the refusals that keep one export
//! per window, the status line each answer leaves, and the read timer that exists only while a job
//! is live. The owner's answers are handed in as the messages the runtime delivers.
use super::{
    export::{ExportChoice, exported_text, refused_text},
    message::{ExportMessage, MenuTarget, PaletteAction, ViewMessage},
    tasks::CallError,
    testing::{boot, descriptors, finish, opened_with_modules},
    *,
};

fn start(editor: &mut Editor, keep_metadata: bool) {
    let _ = editor.update(Message::Export(ExportMessage::Start { keep_metadata }));
}

/// The plan answered and the dialog chose `/tmp/<name>`: the status names the file, and
/// `export.jpeg` is what the returned task would send.
fn chosen(editor: &mut Editor, name: &str) {
    let state = editor.state.as_ref().expect("a photograph is open");
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
    assert_eq!(editor.status, format!("Exporting {name}\u{2026}"));
    assert!(!editor.picker_open);
}

#[test]
fn with_no_photograph_export_is_disabled_and_refused() {
    let (mut editor, catalog) = boot();
    assert!(!editor.workspace.title.can_export);
    start(&mut editor, false);
    assert!(editor.export.run.is_none());
    assert_eq!(editor.status, "Open a photograph to export it");
    assert!(editor.export_poll_subscription().is_none());
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
    assert!(editor.picker_open);
    assert!(!editor.workspace.title.can_export);
    assert_eq!(
        editor.export.run.as_ref().map(|run| run.keep_metadata),
        Some(true)
    );
    // Nothing is queued at the core yet, so nothing is read.
    assert!(editor.export_poll_subscription().is_none());

    // A second press while the first runs is refused and changes nothing.
    start(&mut editor, false);
    assert_eq!(editor.status, "An export is already running");
    assert_eq!(
        editor.export.run.as_ref().map(|run| run.keep_metadata),
        Some(true)
    );

    // Cancelling the dialog ends it.
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(None))));
    assert_eq!(editor.status, "Export cancelled");
    assert!(editor.export.run.is_none());
    assert!(!editor.picker_open);
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
        editor.status,
        exported_text("photo-edited.jpg", 6000, 4000, 8_412_345)
    );
    assert!(editor.export.run.is_none());
    assert!(
        editor.export_poll_subscription().is_none(),
        "no timer once the job has ended"
    );
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
    assert_eq!(editor.status, refused_text("photo-edited.jpg"));
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
        editor.status,
        "Export failed: the destination must end in .jpg or .jpeg"
    );
    finish(editor, catalog);
}

#[test]
fn the_palette_lists_both_exports_for_export_jpeg() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 1);
    let _ = editor.update(Message::Palette(
        crate::app::message::PaletteMessage::Query("export".into()),
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
        assert!(editor.export.active(), "{}", editor.status);
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while let Some(job) = editor
            .export
            .run
            .as_ref()
            .and_then(|run| run.job_id.clone())
        {
            assert!(std::time::Instant::now() < deadline, "the export never ended");
            let result = read_now(&editor.owner, editor.client, &job);
            let _ = editor.update(Message::Export(ExportMessage::Read {
                job_id: job,
                result,
            }));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };

    export_once(&mut editor);
    let written = std::fs::read(&destination).unwrap();
    let decoded = image::load_from_memory(&written).unwrap();
    let expected = format!(
        "Exported photo-edited.jpg \u{b7} {} \u{d7} {} \u{b7} ",
        decoded.width(),
        decoded.height()
    );
    assert!(editor.status.starts_with(&expected), "{}", editor.status);

    // The same file again is refused, and the file is exactly as it was.
    export_once(&mut editor);
    assert_eq!(editor.status, refused_text("photo-edited.jpg"));
    assert_eq!(std::fs::read(&destination).unwrap(), written);
    assert!(editor.export.run.is_none());

    std::fs::remove_dir_all(&folder).unwrap();
    finish(editor, catalog);
}
