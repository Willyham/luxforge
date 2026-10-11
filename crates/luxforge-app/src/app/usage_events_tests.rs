//! The desktop's GUI-local usage producers: the button, the shortcut and the palette record the
//! same event; a command-backed operation's shortcut and palette entry raise the button's own
//! message, so the core records it once at the command; nothing is recorded while collection is off.
use super::{
    message::{
        history::HistoryMessage, overlay::OverlayMessage, palette::PaletteMessage,
        select::SelectMessage, view::ViewMessage,
    },
    testing::{finish, real_photo},
    *,
};
use crate::state::{palette::PaletteAction, select::Shown};
use iced::{
    event::Status,
    keyboard::{Event as KeyEvent, Key, Modifiers, key::Physical},
};
use luxforge_core::usage::{
    EventKind, Feature, ImageId, ImageKey, Renderer, SessionClock, SessionStart, Trigger, Workspace,
};

fn key() -> ImageKey {
    ImageKey::from_bytes([0x24; 32])
}

fn start(editor: &Editor) {
    editor.owner.usage().start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::manual().0,
        key(),
    );
}

/// The events recorded since the last call, `session.start` left out.
fn events(editor: &Editor) -> Vec<EventKind> {
    let Some(session) = editor.owner.usage().session() else {
        return Vec::new();
    };
    let Some(window) = session.detach() else {
        return Vec::new();
    };
    window
        .events()
        .iter()
        .map(|event| event.kind)
        .filter(|kind| !matches!(kind, EventKind::SessionStart { .. }))
        .collect()
}

fn image(editor: &Editor) -> ImageId {
    let state = editor.document.state.as_ref().expect("an open photograph");
    key().image_id_of_hex(&state.asset.fingerprint).unwrap()
}

/// One key through the key table, as the keyboard sends it.
fn pressed(editor: &Editor, key: &str, modifiers: Modifiers) -> Message {
    let key = Key::Character(key.into());
    let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    });
    crate::app::keymap::keymap(&event, Status::Ignored, &editor.key_context()).expect("a bound key")
}

/// Run the palette's entry for `action`, as a person picks it from the list for `query`.
fn run_palette(editor: &mut Editor, query: &str, action: &PaletteAction) {
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query(query.into())));
    let position = editor
        .workspace
        .palette
        .entries
        .iter()
        .position(|entry| &entry.action == action)
        .unwrap_or_else(|| {
            panic!(
                "no {action:?} entry: {:?}",
                editor.workspace.palette.entries
            )
        });
    editor.palette.selected = position;
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
}

fn catalog(name: &str) -> std::path::PathBuf {
    luxforge_testbase::paths::temp_path(&format!("usage-desktop-{name}.sqlite"))
}

#[test]
fn usage_a_zoom_from_the_button_the_shortcut_and_the_palette_records_one_identical_event_each() {
    let catalog = catalog("zoom");
    let (mut editor, _, _) = real_photo(&catalog);
    start(&editor);
    let image = image(&editor);
    let _ = editor.update(Message::View(ViewMessage::Fit));
    let shortcut = pressed(&editor, "f", Modifiers::empty());
    let _ = editor.update(shortcut);
    run_palette(&mut editor, "Fit", &PaletteAction::Fit);
    let zoom = EventKind::Interaction {
        feature: Feature::Zoom,
        image: Some(image),
    };
    assert_eq!(events(&editor), [zoom, zoom, zoom]);
    finish(editor, catalog);
}

#[test]
fn usage_view_toggles_record_their_interactions_without_an_image() {
    let catalog = catalog("toggles");
    let (mut editor, _, _) = real_photo(&catalog);
    start(&editor);
    let _ = editor.update(Message::View(ViewMessage::ToggleInformation));
    let shortcut = pressed(&editor, "i", Modifiers::empty());
    let _ = editor.update(shortcut);
    let _ = editor.update(Message::View(ViewMessage::TogglePanel(
        crate::state::palette::Panel::State,
    )));
    let _ = editor.update(Message::Overlay(OverlayMessage::ToggleClipping(None)));
    let of = |feature| EventKind::Interaction {
        feature,
        image: None,
    };
    assert_eq!(
        events(&editor),
        [
            of(Feature::InformationPanel),
            of(Feature::InformationPanel),
            of(Feature::PanelToggle),
            of(Feature::ClippingOverlay),
        ]
    );
    finish(editor, catalog);
}

/// A deliberate switch records its workspace; asking for the workspace already shown records
/// nothing.
#[test]
fn usage_a_workspace_switch_records_the_workspace_entered_once() {
    let catalog = catalog("workspace");
    let (mut editor, _, _) = real_photo(&catalog);
    start(&editor);
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    assert_eq!(
        events(&editor),
        [
            EventKind::WorkspaceEnter {
                workspace: Workspace::Select
            },
            EventKind::WorkspaceEnter {
                workspace: Workspace::Develop
            },
        ]
    );
    finish(editor, catalog);
}

/// Undo is a command: its shortcut and its palette entry raise exactly the message its button
/// does, which sends `history.undo`, so the core records it once at that command's boundary and
/// the desktop records nothing of its own.
#[test]
fn usage_a_command_backed_operation_converges_on_the_buttons_message_and_records_nothing_locally() {
    let catalog = catalog("undo");
    let (mut editor, _, _) = real_photo(&catalog);
    start(&editor);
    let shortcut = pressed(&editor, "z", Modifiers::COMMAND);
    assert_eq!(
        format!("{shortcut:?}"),
        format!("{:?}", Message::History(HistoryMessage::Undo))
    );
    let _ = editor.update(shortcut);
    run_palette(&mut editor, "Undo", &PaletteAction::Undo);
    assert_eq!(events(&editor), [], "the command's event is the core's");
    finish(editor, catalog);
}

#[test]
fn usage_a_settled_preview_carries_the_open_photographs_image_and_its_figure() {
    let catalog = catalog("settled");
    let (mut editor, _, _) = real_photo(&catalog);
    start(&editor);
    let image = image(&editor);
    editor.usage_preview_settled(Renderer::Reference, 12.4);
    editor.usage_preview_settled(Renderer::Gpu, f64::NAN);
    editor.usage_preview_settled(Renderer::Gpu, 3.6);
    let [first, second] = events(&editor)[..] else {
        panic!("two settled previews");
    };
    let EventKind::PreviewSettled {
        image: one,
        renderer: Renderer::Reference,
        duration,
    } = first
    else {
        panic!("{first:?}");
    };
    assert_eq!((one, duration.get()), (image, 12));
    assert!(matches!(
        second,
        EventKind::PreviewSettled { renderer: Renderer::Gpu, duration, .. } if duration.get() == 4
    ));
    finish(editor, catalog);
}

#[test]
fn usage_nothing_is_recorded_while_collection_is_off() {
    let catalog = catalog("off");
    let (mut editor, _, _) = real_photo(&catalog);
    let _ = editor.update(Message::View(ViewMessage::Fit));
    let _ = editor.update(Message::View(ViewMessage::ToggleInformation));
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    editor.usage_preview_settled(Renderer::Reference, 10.0);
    assert!(editor.owner.usage().session().is_none());
    assert!(editor.usage_image.is_empty(), "no image ID while off");
    finish(editor, catalog);
}
