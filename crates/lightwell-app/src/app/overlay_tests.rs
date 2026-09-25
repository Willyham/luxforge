//! The clipping overlay: its toggles send exactly their own flags and commit nothing, and it
//! follows the frame on screen.
use super::{
    message::{ClipEndpoint, OverlayMessage},
    overlay::clip_params,
    testing::{analysed, drafted, finish, opened},
    *,
};
use lightwell_core::Zoom;

/// The overlay is keyed on the image it describes, not on the newest preview asked for: a
/// frame still rendering re-derives nothing, and the drafted frame that reaches the screen
/// re-derives the mask from its own raster, so the overlay never describes the frame the
/// gesture replaced.
#[test]
fn the_clipping_overlay_follows_the_drafted_raster() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.session.workspace.clip_highlights = true;
    editor.session.preview.view.zoom = Zoom::Fit;
    let (committed, raster) = analysed(&editor, 4, &[[9, 9, 9, 255]; 4], 2, 2);
    editor.preview_generation = 4;
    // The pixels reach the screen first, exactly as `Message::Uploaded` presents them: the
    // overlay describes the frame on screen, so nothing is derived until one is.
    editor.presented_generation = 4;
    editor.incoming = Some((committed, raster));
    editor.adopt_analysis(4);
    let _ = editor.update(Message::View(ViewMessage::Resized(1440.0, 900.0)));
    let derived = editor.overlay_request.clone().expect("an overlay");
    assert_eq!(derived.generation, 4);
    assert!(derived.highlights && !derived.shadows);

    // The gesture's tick asks for a drafted preview. Nothing new can be derived from an image
    // that has not arrived, so the mask of the frame on screen is left alone.
    editor.preview_generation = 5;
    editor.refresh_overlay();
    assert_eq!(
        editor.overlay_request.as_ref(),
        Some(&derived),
        "an in-flight render re-derived the overlay from the frame it replaces"
    );

    // The drafted pixels arrive: the retained raster is theirs, and the overlay follows it.
    let draft_id = lightwell_core::DraftId::new();
    let (analysis, drafted_raster) =
        drafted(&editor, 5, &draft_id, 2, &[[255, 255, 255, 255]; 4], 2, 2);
    editor.presented_generation = 5;
    editor.incoming = Some((analysis, drafted_raster));
    editor.adopt_analysis(5);
    editor.refresh_overlay();
    let (generation, retained) = editor.raster.as_ref().expect("the drafted raster");
    assert_eq!(*generation, 5);
    assert_eq!(retained.rgba[0], 255, "the drafted pixels are retained");
    let drafted_overlay = editor.overlay_request.as_ref().expect("a drafted overlay");
    assert_eq!(
        drafted_overlay.generation, 5,
        "the overlay still describes the frame the gesture replaced"
    );
    assert!(
        editor.overlay_surface().is_none(),
        "the previous frame's mask was drawn over the drafted photograph"
    );
    finish(editor, catalog);
}

/// One triangle sends exactly its own flag; the pair moves together; neither is an edit.
#[test]
fn a_clipping_toggle_sets_one_view_flag_and_commits_nothing() {
    let mut workspace = lightwell_core::WorkspaceState::default();
    assert_eq!(
        clip_params(&workspace, Some(ClipEndpoint::Shadows)),
        json!({"clip_shadows": true}),
        "the shadow triangle names its own flag and no other"
    );
    assert_eq!(
        clip_params(&workspace, Some(ClipEndpoint::Highlights)),
        json!({"clip_highlights": true})
    );
    assert_eq!(
        clip_params(&workspace, None),
        json!({"clip_shadows": true, "clip_highlights": true}),
        "J moves both"
    );
    // One on, one off: the key turns the pair on rather than flipping each.
    workspace.clip_shadows = true;
    assert_eq!(
        clip_params(&workspace, None),
        json!({"clip_shadows": true, "clip_highlights": true})
    );
    assert_eq!(
        clip_params(&workspace, Some(ClipEndpoint::Shadows)),
        json!({"clip_shadows": false}),
        "a lit triangle turns its own overlay off"
    );
    // Both on: the key turns the pair off, so one key both shows and hides them.
    workspace.clip_highlights = true;
    assert_eq!(
        clip_params(&workspace, None),
        json!({"clip_shadows": false, "clip_highlights": false})
    );

    // Driven through the editor, a toggle takes no mutation path at all: nothing is marked
    // busy, no request is opened, and the committed stack and its revision are untouched.
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let before = (
        editor.activity.requested,
        editor.state.as_ref().expect("an open asset").revision,
        editor.history.entries.len(),
    );
    for message in [
        Message::Overlay(OverlayMessage::ToggleClipping(Some(ClipEndpoint::Shadows))),
        Message::Overlay(OverlayMessage::ToggleClipping(Some(
            ClipEndpoint::Highlights,
        ))),
        Message::Overlay(OverlayMessage::ToggleClipping(None)),
    ] {
        let _ = editor.update(message);
        assert!(!editor.busy, "a view toggle never takes the mutation path");
    }
    let state = editor.state.as_ref().expect("an open asset");
    assert_eq!(editor.activity.requested, before.0, "no request was opened");
    assert_eq!(state.revision, before.1, "no edit committed");
    assert!(state.current_entry.snapshot.recipe.layers.is_empty());
    assert_eq!(editor.history.entries.len(), before.2, "no history entry");
    finish(editor, catalog);
}

/// `J` reaches the same message the title bar's Clipping button sends, and only when no field
/// has taken the key.
#[test]
fn the_j_key_toggles_both_overlays() {
    let context = keymap::KeyContext::default();
    let key = iced::keyboard::Key::Character("j".into());
    let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers: iced::keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    });
    assert!(matches!(
        keymap::keymap(&event, iced::event::Status::Ignored, &context),
        Some(Message::Overlay(OverlayMessage::ToggleClipping(None)))
    ));
    // A field that took the key keeps it: letters never act while text has focus.
    assert!(
        keymap::keymap(&event, iced::event::Status::Captured, &context).is_none(),
        "J typed into a field is not a shortcut"
    );
}
