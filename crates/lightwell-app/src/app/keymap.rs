//! The keyboard table, as one pure function. Key codes never reach the update function: an event
//! becomes a semantic message here or nothing at all, so the whole mapping is testable without a
//! window.
use crate::app::message::{CropMessage, Message};
use iced::{
    Event,
    event::Status,
    keyboard::{Event as Keys, Key, key::Named},
};

/// What the mapping depends on: whether a draft is open, and the canvas modes the registry offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyContext {
    /// A crop draft is open, so Enter, Escape, Space and Option drive it.
    pub(crate) drafting: bool,
    /// The declared canvas-mode shortcut letters and the module each one selects.
    pub(crate) modes: Vec<(char, String)>,
}

/// One event as one message, or nothing. `status` is Iced's: a key a text field already consumed
/// arrives as `Captured` and never reaches a letter shortcut or a draft key.
pub(crate) fn keymap(event: &Event, status: Status, context: &KeyContext) -> Option<Message> {
    if matches!(event, Event::Window(iced::window::Event::CloseRequested)) {
        return Some(Message::Close);
    }
    let Event::Keyboard(keyboard) = event else {
        return None;
    };
    // The modifier the canvas reads lives in the app, so it follows every change while drafting.
    if context.drafting {
        match keyboard {
            Keys::ModifiersChanged(modifiers) => {
                return Some(Message::Crop(CropMessage::Option(modifiers.alt())));
            }
            Keys::KeyReleased {
                key: Key::Named(Named::Space),
                ..
            } => return Some(Message::Crop(CropMessage::Space(false))),
            _ => {}
        }
    }
    let Keys::KeyPressed { key, modifiers, .. } = keyboard else {
        return None;
    };
    if modifiers.command() {
        if character(key, "o") {
            return Some(Message::Open);
        }
        if character(key, "z") {
            return Some(if modifiers.shift() {
                Message::Redo
            } else {
                Message::Undo
            });
        }
        return None;
    }
    // Tab walks the generated fields; shift is the only modifier it tolerates.
    if matches!(key, Key::Named(Named::Tab))
        && !modifiers.alt()
        && !modifiers.control()
        && !modifiers.logo()
    {
        return Some(if modifiers.shift() {
            Message::FocusPrevious
        } else {
            Message::FocusNext
        });
    }
    if status != Status::Ignored {
        return None;
    }
    if context.drafting {
        match key {
            Key::Named(Named::Space) => return Some(Message::Crop(CropMessage::Space(true))),
            Key::Named(Named::Enter) => return Some(Message::Crop(CropMessage::Apply)),
            Key::Named(Named::Escape) => return Some(Message::Crop(CropMessage::Cancel)),
            _ => {}
        }
    }
    // A module's declared canvas-mode letter, which acts only when no text field took the key.
    context
        .modes
        .iter()
        .find(|(letter, _)| character(key, letter.to_lowercase().to_string().as_str()))
        .map(|(_, module)| Message::SetMode(module.clone()))
}

fn character(key: &Key, letter: &str) -> bool {
    matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(letter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;

    fn pressed(key: Key, modifiers: Modifiers) -> Event {
        Event::Keyboard(Keys::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    fn letter(value: &str) -> Key {
        Key::Character(value.into())
    }

    fn context() -> KeyContext {
        KeyContext {
            drafting: false,
            modes: vec![('R', "lightwell.crop".into())],
        }
    }

    #[test]
    fn the_key_table_maps_exactly_the_declared_shortcuts() {
        let plain = context();
        let drafting = KeyContext {
            drafting: true,
            ..context()
        };
        let command = Modifiers::COMMAND;
        let shift_command = Modifiers::COMMAND | Modifiers::SHIFT;
        let cases: Vec<(&str, Event, Status, &KeyContext, Option<&str>)> = vec![
            (
                "close",
                Event::Window(iced::window::Event::CloseRequested),
                Status::Ignored,
                &plain,
                Some("Close"),
            ),
            (
                "open",
                pressed(letter("o"), command),
                Status::Ignored,
                &plain,
                Some("Open"),
            ),
            (
                "undo",
                pressed(letter("z"), command),
                Status::Ignored,
                &plain,
                Some("Undo"),
            ),
            (
                "redo",
                pressed(letter("Z"), shift_command),
                Status::Ignored,
                &plain,
                Some("Redo"),
            ),
            (
                "unbound command key",
                pressed(letter("q"), command),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "focus next",
                pressed(Key::Named(Named::Tab), Modifiers::empty()),
                Status::Captured,
                &plain,
                Some("FocusNext"),
            ),
            (
                "focus previous",
                pressed(Key::Named(Named::Tab), Modifiers::SHIFT),
                Status::Captured,
                &plain,
                Some("FocusPrevious"),
            ),
            (
                "mode letter",
                pressed(letter("r"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("SetMode"),
            ),
            (
                "a mode letter a field consumed",
                pressed(letter("r"), Modifiers::empty()),
                Status::Captured,
                &plain,
                None,
            ),
            (
                "an undeclared letter",
                pressed(letter("k"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "apply the draft",
                pressed(Key::Named(Named::Enter), Modifiers::empty()),
                Status::Ignored,
                &drafting,
                Some("Crop"),
            ),
            (
                "cancel the draft",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &drafting,
                Some("Crop"),
            ),
            (
                "space pans the draft",
                pressed(Key::Named(Named::Space), Modifiers::empty()),
                Status::Ignored,
                &drafting,
                Some("Crop"),
            ),
            (
                "a draft key a field consumed",
                pressed(Key::Named(Named::Enter), Modifiers::empty()),
                Status::Captured,
                &drafting,
                None,
            ),
            (
                "Enter without a draft",
                pressed(Key::Named(Named::Enter), Modifiers::empty()),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "option while drafting",
                Event::Keyboard(Keys::ModifiersChanged(Modifiers::ALT)),
                Status::Ignored,
                &drafting,
                Some("Crop"),
            ),
            (
                "option without a draft",
                Event::Keyboard(Keys::ModifiersChanged(Modifiers::ALT)),
                Status::Ignored,
                &plain,
                None,
            ),
        ];
        for (case, event, status, context, expected) in cases {
            let mapped = keymap(&event, status, context);
            match expected {
                Some(name) => {
                    let mapped = mapped.unwrap_or_else(|| panic!("{case}: nothing was mapped"));
                    assert!(
                        format!("{mapped:?}").starts_with(name),
                        "{case}: {mapped:?}"
                    );
                }
                None => assert!(mapped.is_none(), "{case}: {mapped:?}"),
            }
        }
        // Space released ends the pan only while a draft is open.
        let release = Event::Keyboard(Keys::KeyReleased {
            key: Key::Named(Named::Space),
            modified_key: Key::Named(Named::Space),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
        });
        assert!(keymap(&release, Status::Ignored, &drafting).is_some());
        assert!(keymap(&release, Status::Ignored, &plain).is_none());
        // A registry without canvas modes binds no letters at all.
        let bare = KeyContext::default();
        assert!(
            keymap(
                &pressed(letter("r"), Modifiers::empty()),
                Status::Ignored,
                &bare
            )
            .is_none()
        );
    }
}
