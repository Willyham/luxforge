//! Per-client view state: the developer gallery, workspace flags reaching the models only through
//! the adopted session, and one pan in flight.
use super::{
    message::ControlMessage,
    testing::{boot, crop_descriptor, finish, opened},
    *,
};
use lightwell_core::{CropStage, POINTER_MODE};

/// The gallery page is this desktop's own view state: opening a page needs no photograph, sends
/// nothing to the owner and leaves the session exactly as it was.
#[test]
fn gallery_is_desktop_view_state_without_a_photo_and_respects_developer_mode() {
    let (mut editor, catalog) = boot();
    assert!(!editor.workspace.title.developer);
    assert!(editor.gallery_page().is_none());
    editor.developer = true;
    editor.rederive();
    assert!(editor.workspace.title.can_open_gallery);
    assert!(editor.state.is_none());
    let pages = view::gallery_page_info(0).unwrap().count;
    assert!(view::gallery_page_info(pages).is_none());
    let before = editor.session.clone();
    let _ = editor.update(Message::View(ViewMessage::Gallery(Some(6))));
    assert_eq!(editor.gallery_page(), Some(6));
    assert_eq!(editor.snapshot()["gallery"]["page"], json!(6));
    assert_eq!(editor.session, before, "the page is not session state");
    assert!(
        editor.snapshot()["workspace"]
            .get("component_gallery")
            .is_none()
    );
    // A page the board does not have is ignored.
    let _ = editor.update(Message::View(ViewMessage::Gallery(Some(pages))));
    assert_eq!(editor.gallery_page(), Some(6));
    let generation = editor.activity.requested;
    let _ = editor.update(Message::View(ViewMessage::GalleryPreview));
    assert_eq!(editor.session, before);
    assert_eq!(editor.activity.requested, generation);
    editor.developer = false;
    editor.rederive();
    assert!(editor.gallery_page().is_none());
    assert!(!editor.workspace.title.developer);
    editor.developer = true;
    editor.busy = true;
    editor.rederive();
    assert!(!editor.workspace.title.can_open_gallery);
    let _ = editor.update(Message::View(ViewMessage::Gallery(Some(0))));
    assert_eq!(editor.session, before);
    assert!(editor.status.contains("Finish the current operation"));
    finish(editor, catalog);
}

/// The panel toggles, the mode and the thirds overlay are the owner's per-client workspace
/// state: the desktop asks for a change and adopts whatever the session comes back with, so the
/// screen follows `session.state` and never a local flag.
#[test]
fn workspace_state_reaches_the_models_only_through_the_adopted_session() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    assert!(!editor.workspace.canvas.thirds);

    // The toggle sends the request; nothing changes until the owner answers.
    let _ = editor.update(Message::View(ViewMessage::ToggleThirds));
    assert!(
        !editor.workspace.canvas.thirds,
        "the desktop holds no flag of its own"
    );
    let mut session = ClientSession {
        revision: 2,
        ..ClientSession::default()
    };
    session.workspace.thirds = true;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
        session.clone()
    ))));
    assert!(editor.workspace.canvas.thirds);
    assert_eq!(editor.snapshot()["workspace"]["thirds"], json!(true));

    // A session that hides the state panel hides it and narrows the captured photo surface.
    let scale = 2.0;
    let width = 1440.0;
    let open = view::surface_columns(width, scale, &editor.workspace);
    assert_eq!(open[0], (view::STATE_PANEL_WIDTH * scale) as u32);
    session.revision = 3;
    session.workspace.state_panel = false;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
        session.clone()
    ))));
    assert!(!editor.workspace.title.state_panel_open);
    let collapsed = view::surface_columns(width, scale, &editor.workspace);
    assert_eq!(collapsed[0], 0, "the canvas now starts at the window edge");
    assert_eq!(collapsed[1], open[1], "the tools panel is still open");

    // And one that hides the tools panel gives the canvas the rest of the width.
    session.revision = 4;
    session.workspace.tools_panel = false;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    assert!(!editor.workspace.title.tools_panel_open);
    assert_eq!(
        view::surface_columns(width, scale, &editor.workspace),
        [0, (width * scale) as u32]
    );
    finish(editor, catalog);
}

#[test]
fn pan_keeps_one_request_in_flight_and_only_the_newest_pending_position() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::View(ViewMessage::Panned(1.0, 2.0)));
    assert!(editor.pan_in_flight);
    assert_eq!(editor.pending_pan, None);
    let _ = editor.update(Message::View(ViewMessage::Panned(3.0, 4.0)));
    let _ = editor.update(Message::View(ViewMessage::Panned(5.0, 6.0)));
    assert_eq!(editor.pending_pan, Some((5.0, 6.0)));
    let _ = editor.update(Message::View(ViewMessage::PanSynced(Ok(
        ClientSession::default(),
    ))));
    assert!(
        editor.pan_in_flight,
        "the pending position starts the next request"
    );
    assert_eq!(editor.pending_pan, None);
    let _ = editor.update(Message::View(ViewMessage::PanSynced(Ok(
        ClientSession::default(),
    ))));
    assert!(!editor.pan_in_flight);
    finish(editor, catalog);
}

#[test]
fn a_section_toggle_is_local_and_a_mode_change_is_session_state() {
    let crop = crop_descriptor();
    let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
    assert!(editor.expanded.is_empty(), "defaults need no stored flag");
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        crop.id.clone(),
    )));
    assert_eq!(editor.expanded.get(&crop.id), Some(&false));
    assert_eq!(editor.snapshot()["expanded"][&crop.id], json!(false));
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        crop.id.clone(),
    )));
    assert_eq!(editor.expanded.get(&crop.id), Some(&true));

    // Entering the crop module's mode opens its draft; leaving it with a draft open is refused.
    let _ = editor.update(Message::View(ViewMessage::SetMode(crop.id.clone())));
    assert!(
        editor.crop_pending().is_some(),
        "the mode opens the draft: {}",
        editor.status
    );
    editor.open_draft(CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    });
    editor.session.workspace.mode = crop.id.clone();
    let _ = editor.update(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
    assert!(editor.crop().is_some(), "the draft is never discarded");
    assert!(
        editor.status.contains("Apply or Cancel"),
        "{}",
        editor.status
    );
    finish(editor, catalog);
}

/// A message that changes nothing the screen shows builds no section of it, and one that changes a
/// single input builds the sections that read it and no others.
#[test]
fn a_message_that_changes_nothing_builds_no_section() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let built = editor.built.builds;
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(editor.built.builds, built, "an idle poll builds nothing");

    editor.status = "Something happened".into();
    editor.rederive();
    assert_eq!(
        editor.built.builds,
        built + 1,
        "a status line is the status bar's alone"
    );
    assert_eq!(editor.workspace.status.message, "Something happened");
    finish(editor, catalog);
}
