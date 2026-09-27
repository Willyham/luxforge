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
