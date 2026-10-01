//! Per-client view state: the developer gallery, workspace flags reaching the models only through
//! the adopted session, and one pan in flight.
use super::{
    message::control::ControlMessage,
    testing::{boot, crop_descriptor, finish, opened},
    *,
};
use luxforge_core::POINTER_MODE;

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
    assert!(editor.document.state.is_none());
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
    // The gallery's one refusal is written as it is, not replaced by a generic line.
    assert_eq!(editor.status.text, crate::state::IN_FLIGHT);
    editor.busy = false;
    editor.document.compare_return = Some(luxforge_core::HistorySelection::Current);
    editor.rederive();
    assert!(!editor.workspace.title.can_open_gallery);
    let _ = editor.update(Message::View(ViewMessage::Gallery(Some(0))));
    assert_eq!(
        editor.status.text,
        "Release Compare before opening Components"
    );
    editor.document.compare_return = None;
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
    let columns = |editor: &Editor| {
        let title = &editor.workspace.title;
        crate::layout::surface_columns(width, scale, title.state_panel_open, title.tools_panel_open)
    };
    let open = columns(&editor);
    assert_eq!(open[0], (crate::layout::STATE_PANEL_WIDTH * scale) as u32);
    session.revision = 3;
    session.workspace.state_panel = false;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
        session.clone()
    ))));
    assert!(!editor.workspace.title.state_panel_open);
    let collapsed = columns(&editor);
    assert_eq!(collapsed[0], 0, "the canvas now starts at the window edge");
    assert_eq!(collapsed[1], open[1], "the tools panel is still open");

    // And one that hides the tools panel gives the canvas the rest of the width.
    session.revision = 4;
    session.workspace.tools_panel = false;
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    assert!(!editor.workspace.title.tools_panel_open);
    assert_eq!(columns(&editor), [0, (width * scale) as u32]);
    finish(editor, catalog);
}

#[test]
fn pan_keeps_one_request_in_flight_and_only_the_newest_pending_position() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::View(ViewMessage::Panned(1.0, 2.0)));
    assert!(editor.view_state.pan.in_flight());
    assert_eq!(editor.view_state.pan.pending().copied(), None);
    let _ = editor.update(Message::View(ViewMessage::Panned(3.0, 4.0)));
    let _ = editor.update(Message::View(ViewMessage::Panned(5.0, 6.0)));
    assert_eq!(editor.view_state.pan.pending().copied(), Some((5.0, 6.0)));
    let _ = editor.update(Message::View(ViewMessage::PanSynced(Ok(
        ClientSession::default(),
    ))));
    assert!(
        editor.view_state.pan.in_flight(),
        "the pending position starts the next request"
    );
    assert_eq!(editor.view_state.pan.pending().copied(), None);
    let _ = editor.update(Message::View(ViewMessage::PanSynced(Ok(
        ClientSession::default(),
    ))));
    assert!(!editor.view_state.pan.in_flight());
    finish(editor, catalog);
}

#[test]
fn a_section_toggle_is_local_and_a_mode_change_is_session_state() {
    let crop = crop_descriptor();
    let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
    assert!(
        editor.controls.expanded.is_empty(),
        "defaults need no stored flag"
    );
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        crop.id.clone(),
    )));
    assert_eq!(editor.controls.expanded.get(&crop.id), Some(&false));
    assert_eq!(editor.snapshot()["expanded"][&crop.id], json!(false));
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        crop.id.clone(),
    )));
    assert_eq!(editor.controls.expanded.get(&crop.id), Some(&true));

    // Entering the crop module's mode opens its draft; leaving it with a draft open is refused.
    let _ = editor.update(Message::View(ViewMessage::SetMode(crop.id.clone())));
    assert!(
        editor.crop().is_some(),
        "the mode opens the draft: {}",
        editor.status.text
    );
    editor.session.workspace.mode = crop.id.clone();
    let _ = editor.update(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
    assert!(editor.crop().is_some(), "the draft is never discarded");
    assert!(
        editor.status.text.contains("Apply or Cancel"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// A status line a message sets is on the status bar once the screen is derived again.
#[test]
fn a_status_line_reaches_the_status_bar() {
    let (mut editor, catalog) = boot();
    editor.status.text = "Something happened".into();
    editor.rederive();
    assert_eq!(editor.workspace.status.message, "Something happened");
    finish(editor, catalog);
}

/// The title bar's percentage segment opens as the zoom field holding the percentage it showed,
/// and any zoom the field or a segment then sets closes it again.
#[test]
fn the_percentage_segment_opens_as_the_zoom_field_and_a_zoom_closes_it() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.presentation.dimensions = Some((480, 320));
    editor.rederive();
    let shown = editor.workspace.title.zoom_percent.clone();
    assert!(shown.ends_with('%'), "{shown}");
    let _ = editor.update(Message::View(ViewMessage::EditZoom));
    assert!(editor.workspace.title.zoom_editing);
    assert_eq!(editor.view_state.zoom, shown.trim_end_matches('%'));
    let _ = editor.update(Message::View(ViewMessage::Zoom("50".into())));
    let _ = editor.update(Message::View(ViewMessage::ApplyZoom));
    assert!(!editor.workspace.title.zoom_editing);
    let _ = editor.update(Message::View(ViewMessage::EditZoom));
    let _ = editor.update(Message::View(ViewMessage::Fit));
    assert!(
        !editor.workspace.title.zoom_editing,
        "a segment closes the field"
    );
    finish(editor, catalog);
}

/// A zoom stop asks for its percentage as the 100% segment does, and closes the typed field; the
/// stops then read the session's zoom: a percentage on a stop selects it and rests the thumb on
/// its notch, one between two stops rests it between their notches, and Fit selects none.
#[test]
fn a_zoom_stop_asks_for_its_percentage_and_the_stops_read_the_session() {
    use luxforge_core::Zoom;
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.presentation.dimensions = Some((480, 320));
    editor.rederive();
    let _ = editor.update(Message::View(ViewMessage::EditZoom));
    let _ = editor.update(Message::View(ViewMessage::ZoomTo(300.0)));
    assert!(editor.busy, "the stop asked the session");
    assert_eq!(editor.status.text, "Running view.set\u{2026}");
    assert_eq!(editor.view_state.zoom, "300");
    assert!(!editor.workspace.title.zoom_editing);
    editor.busy = false;
    // The session's percentage, the stop it selects and the range the thumb rests in.
    for (value, stop, low, high) in [
        (300.0, Some(4), 4.0, 4.0),
        (50.0, Some(0), 0.0, 0.0),
        (250.0, None, 3.0, 4.0),
    ] {
        editor.session.preview.view.zoom = Zoom::Percent { value };
        editor.rederive();
        let title = &editor.workspace.title;
        assert_eq!(title.zoom_stop, stop, "{value}%");
        let position = title.zoom_stop_position.unwrap();
        assert!(
            position >= low && position <= high,
            "{value}% at {position}"
        );
        if low < high {
            assert!(position > low && position < high, "{value}% at {position}");
        }
    }
    editor.session.preview.view.zoom = Zoom::Percent { value: 25.0 };
    editor.rederive();
    assert_eq!(
        editor.workspace.title.zoom_stop_position, None,
        "below the stops"
    );
    editor.session.preview.view.zoom = Zoom::Fit;
    editor.rederive();
    assert_eq!(editor.workspace.title.zoom_stop, None, "Fit is no stop");
    finish(editor, catalog);
}

/// A zoom step asks for the next stop from the zoom on screen and shows the stops for a moment;
/// past the last stop it asks for nothing but still shows them, and with no photograph it does
/// nothing at all.
#[test]
fn a_zoom_step_asks_for_the_next_stop_and_shows_the_stops() {
    use luxforge_core::Zoom;
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::View(ViewMessage::ZoomStep(1)));
    assert!(!editor.busy);
    assert_eq!(
        editor.workspace.title.zoom_reveal, 0,
        "no photograph, no stops"
    );
    finish(editor, catalog);

    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.presentation.dimensions = Some((480, 320));
    editor.session.preview.view.zoom = Zoom::Percent { value: 250.0 };
    editor.rederive();
    let _ = editor.update(Message::View(ViewMessage::ZoomStep(1)));
    assert!(editor.busy, "the step asked the session");
    assert_eq!(editor.view_state.zoom, "300");
    assert_eq!(editor.workspace.title.zoom_reveal, 1);
    editor.busy = false;
    let _ = editor.update(Message::View(ViewMessage::ZoomStep(-1)));
    assert_eq!(editor.view_state.zoom, "200", "down from 250% is 200%");
    assert_eq!(editor.workspace.title.zoom_reveal, 2);
    editor.busy = false;
    editor.session.preview.view.zoom = Zoom::Percent { value: 1200.0 };
    editor.rederive();
    let _ = editor.update(Message::View(ViewMessage::ZoomStep(1)));
    assert!(!editor.busy, "nothing past the last stop");
    assert_eq!(
        editor.workspace.title.zoom_reveal, 3,
        "but the stops still show"
    );
    finish(editor, catalog);
}
