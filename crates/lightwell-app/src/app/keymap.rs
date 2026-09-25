//! The keyboard table, as one pure function. Key codes never reach the update function: an event
//! becomes a semantic message here or nothing at all, so the whole mapping is testable without a
//! window.
use crate::app::message::{
    BrushEdit, CropMessage, DraftMessage, HistoryMessage, MaskMessage, Message, OverlayMessage,
    PaletteMessage, Panel, SyncMessage, ViewMessage,
};
use iced::{
    Event,
    event::Status,
    keyboard::{Event as Keys, Key, key::Named},
};
use lightwell_core::{MASK_MODE, POINTER_MODE};

/// What the mapping depends on: whether a draft is open, whether the palette has the keyboard, and
/// the canvas modes the registry offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyContext {
    pub(crate) gallery_open: bool,
    /// A crop draft is open, so Enter, Escape, Space and Option drive it.
    pub(crate) drafting: bool,
    /// A slider gesture's draft is open, so Escape discards it and the arrow key that is stepping
    /// it commits it on key-up.
    pub(crate) slider_drafting: bool,
    /// A mask shape gesture is open, so Enter applies it and Escape cancels it, exactly as the crop
    /// draft's keys do for its own gesture.
    pub(crate) mask_drafting: bool,
    /// Mask mode is active, so the brush's own keys are live: the brackets size it, the shifted
    /// brackets feather it, and the erase modifier erases while it is held. They are here rather
    /// than only while a stroke is down, because the brush is sized before it is put down.
    pub(crate) mask_brush: bool,
    /// The command palette is open, so Escape closes it rather than reaching a draft.
    pub(crate) palette_open: bool,
    /// A module's canvas mode is active, so Escape leaves it. A mode that owns a draft answers
    /// Escape with its own cancel first; a mode without one has nothing to discard.
    pub(crate) mode_active: bool,
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
    // Gallery previews must never send editor shortcuts to the hidden photograph.
    if context.gallery_open {
        return match keyboard {
            Keys::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            } => Some(Message::View(ViewMessage::Gallery(None))),
            Keys::KeyPressed {
                key: Key::Named(Named::Tab),
                modifiers,
                ..
            } => Some(if modifiers.shift() {
                Message::View(ViewMessage::FocusPrevious)
            } else {
                Message::View(ViewMessage::FocusNext)
            }),
            _ => None,
        };
    }
    // Compare is a hold, so its release must arrive whatever has focus: a field that swallowed the
    // press would otherwise leave the original preview on screen with nothing to end it.
    if let Keys::KeyReleased { key, .. } = keyboard
        && character(key, "\\")
    {
        return Some(Message::History(HistoryMessage::CompareEnd));
    }
    // The slider guard emits one release for keyboard stepping. The window keymap must not send a
    // second commit for the same key-up; it only handles Escape for an open gesture below.
    // The modifier the canvas reads lives in the app, so it follows every change while drafting.
    // The brush's erase modifier is a hold, not a latch: it erases while it is down, and a stroke
    // already on the photograph keeps the flag it started with, which the draft enforces. The
    // modifier has to arrive whatever has focus, exactly as the crop's does.
    if context.mask_brush
        && let Keys::ModifiersChanged(modifiers) = keyboard
    {
        return Some(Message::Mask(MaskMessage::Brush(BrushEdit::EraseHeld(
            modifiers.alt(),
        ))));
    }
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
    let Keys::KeyPressed {
        key,
        modifiers,
        repeat,
        ..
    } = keyboard
    else {
        return None;
    };
    if modifiers.command() {
        if character(key, "o") {
            return Some(Message::Sync(SyncMessage::Open));
        }
        if character(key, "z") {
            return Some(if modifiers.shift() {
                Message::History(HistoryMessage::Redo)
            } else {
                Message::History(HistoryMessage::Undo)
            });
        }
        if character(key, "k") {
            return Some(Message::Palette(PaletteMessage::Open));
        }
        // The panel toggles are the one pair that also needs Option, so they cannot collide with a
        // bracket a field might want.
        if modifiers.alt() {
            if character(key, "[") {
                return Some(Message::View(ViewMessage::TogglePanel(Panel::State)));
            }
            if character(key, "]") {
                return Some(Message::View(ViewMessage::TogglePanel(Panel::Tools)));
            }
        }
        return None;
    }
    // The palette owns Escape and the arrow keys while it is open, whatever its own query field
    // did with the key: the field has focus, so these must act regardless of `status`.
    if context.palette_open {
        match key {
            Key::Named(Named::Escape) => return Some(Message::Palette(PaletteMessage::Close)),
            Key::Named(Named::ArrowUp) => return Some(Message::Palette(PaletteMessage::Move(-1))),
            Key::Named(Named::ArrowDown) => return Some(Message::Palette(PaletteMessage::Move(1))),
            _ => {}
        }
    }
    // Tab walks the generated fields; shift is the only modifier it tolerates.
    if matches!(key, Key::Named(Named::Tab))
        && !modifiers.alt()
        && !modifiers.control()
        && !modifiers.logo()
    {
        return Some(if modifiers.shift() {
            Message::View(ViewMessage::FocusPrevious)
        } else {
            Message::View(ViewMessage::FocusNext)
        });
    }
    // Escape discards an open slider gesture. It is checked before `status` because the slider's
    // rail captures the arrow keys that may be driving it, and a discarded gesture must never
    // depend on which widget last saw a key.
    if context.slider_drafting && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::Draft(DraftMessage::Cancel));
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
    // A mask shape gesture answers the same two keys, because it is the same kind of draft.
    if context.mask_drafting {
        match key {
            Key::Named(Named::Enter) => return Some(Message::Draft(DraftMessage::Commit)),
            Key::Named(Named::Escape) => return Some(Message::Draft(DraftMessage::Cancel)),
            _ => {}
        }
    }
    // The brush's own keys, in Mask mode: `[` and `]` size it, `Shift+[` and `Shift+]` feather it.
    // They move by the parameter's **declared** step, so a key and the panel's own nudge can never
    // disagree, and they repeat while held because sizing a brush is a held gesture. The panel
    // toggles above already claimed the brackets with Command and Option, so these cannot collide.
    if context.mask_brush {
        for (letter, steps) in [("[", -1.0), ("]", 1.0)] {
            if character(key, letter) {
                return Some(Message::Mask(MaskMessage::Brush(BrushEdit::Nudge {
                    name: if modifiers.shift() { "feather" } else { "size" }.to_owned(),
                    steps,
                })));
            }
        }
    }
    // A canvas mode without a draft of its own — a pick mode — is left with Escape, which commits
    // nothing. A mode that owns a draft answered Escape above by cancelling that draft, which is
    // what returns it to the pointer.
    if context.mode_active
        && !context.drafting
        && !context.mask_drafting
        && matches!(key, Key::Named(Named::Escape))
    {
        return Some(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
    }
    // Single-key shortcuts act only when no text field took the key, and only on the first press:
    // holding a letter down must not re-run its command once per repeat.
    if *repeat {
        return None;
    }
    if character(key, "f") {
        return Some(Message::View(ViewMessage::Fit));
    }
    if character(key, "1") {
        return Some(Message::View(ViewMessage::HundredPercent));
    }
    if character(key, "o") {
        return Some(Message::View(ViewMessage::ToggleThirds));
    }
    // Both clipping overlays at once. The histogram's triangles toggle them one at a time; this
    // key and the title bar's Clipping button move the pair together.
    if character(key, "j") {
        return Some(Message::Overlay(OverlayMessage::ToggleClipping(None)));
    }
    if character(key, "v") {
        return Some(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
    }
    // M enters Mask mode; Shift+M toggles its overlay. Both are host keys, because a mask is a host
    // object: no module declares this mode, so no module's letter can claim them.
    if character(key, "m") {
        return Some(if modifiers.shift() {
            Message::Mask(MaskMessage::ToggleOverlay)
        } else {
            Message::View(ViewMessage::SetMode(MASK_MODE.into()))
        });
    }
    if character(key, "\\") {
        return Some(Message::History(HistoryMessage::CompareBegin));
    }
    // A module's declared canvas-mode letter. The host's own letters above are reserved: the
    // registry rejects a duplicate shortcut, but not one that collides with a host key.
    context
        .modes
        .iter()
        .find(|(letter, _)| character(key, letter.to_lowercase().to_string().as_str()))
        .map(|(_, module)| Message::View(ViewMessage::SetMode(module.clone())))
}

fn character(key: &Key, letter: &str) -> bool {
    matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(letter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;

    fn pressed(key: Key, modifiers: Modifiers) -> Event {
        held(key, modifiers, false)
    }

    /// The same press with the toolkit's auto-repeat flag, so a held key is distinguishable.
    fn held(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
        Event::Keyboard(Keys::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat,
        })
    }

    fn released(key: Key) -> Event {
        Event::Keyboard(Keys::KeyReleased {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
        })
    }

    fn letter(value: &str) -> Key {
        Key::Character(value.into())
    }

    fn context() -> KeyContext {
        KeyContext {
            gallery_open: false,
            drafting: false,
            slider_drafting: false,
            mask_drafting: false,
            mask_brush: false,
            palette_open: false,
            mode_active: false,
            modes: vec![('R', "lightwell.crop".into())],
        }
    }

    #[test]
    fn gallery_escape_returns_without_forwarding_photo_shortcuts() {
        let context = KeyContext {
            gallery_open: true,
            ..context()
        };
        assert!(matches!(
            keymap(
                &pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Captured,
                &context
            ),
            Some(Message::View(ViewMessage::Gallery(None)))
        ));
        for (key, modifiers) in [
            (letter("z"), Modifiers::LOGO),
            (letter("o"), Modifiers::LOGO),
            (letter("r"), Modifiers::empty()),
            (letter("j"), Modifiers::empty()),
            (Key::Named(Named::Enter), Modifiers::empty()),
        ] {
            assert!(keymap(&pressed(key, modifiers), Status::Ignored, &context).is_none());
        }
    }

    #[test]
    fn the_key_table_maps_exactly_the_declared_shortcuts() {
        let plain = context();
        let drafting = KeyContext {
            drafting: true,
            ..context()
        };
        let palette = KeyContext {
            palette_open: true,
            ..context()
        };
        let drafting_palette = KeyContext {
            drafting: true,
            palette_open: true,
            ..context()
        };
        let command = Modifiers::COMMAND;
        let shift_command = Modifiers::COMMAND | Modifiers::SHIFT;
        let option_command = Modifiers::COMMAND | Modifiers::ALT;
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
                Some("Sync(Open)"),
            ),
            (
                "undo",
                pressed(letter("z"), command),
                Status::Ignored,
                &plain,
                Some("History(Undo)"),
            ),
            (
                "redo",
                pressed(letter("Z"), shift_command),
                Status::Ignored,
                &plain,
                Some("History(Redo)"),
            ),
            (
                "unbound command key",
                pressed(letter("q"), command),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "command palette",
                pressed(letter("k"), command),
                Status::Ignored,
                &plain,
                Some("Palette(Open)"),
            ),
            (
                "close the palette",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Captured,
                &palette,
                Some("Palette(Close)"),
            ),
            (
                "the palette's Escape beats a draft's",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &drafting_palette,
                Some("Palette(Close)"),
            ),
            (
                "the palette's down arrow moves the selection, even though its own field has focus",
                pressed(Key::Named(Named::ArrowDown), Modifiers::empty()),
                Status::Captured,
                &palette,
                Some("Palette(Move(1))"),
            ),
            (
                "the palette's up arrow moves the selection the other way",
                pressed(Key::Named(Named::ArrowUp), Modifiers::empty()),
                Status::Captured,
                &palette,
                Some("Palette(Move(-1))"),
            ),
            (
                "an arrow key does nothing while the palette is closed",
                pressed(Key::Named(Named::ArrowDown), Modifiers::empty()),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "toggle the state panel",
                pressed(letter("["), option_command),
                Status::Ignored,
                &plain,
                Some("View(TogglePanel"),
            ),
            (
                "toggle the tools panel",
                pressed(letter("]"), option_command),
                Status::Ignored,
                &plain,
                Some("View(TogglePanel"),
            ),
            (
                "a bracket without Option",
                pressed(letter("["), command),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "fit",
                pressed(letter("f"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("View(Fit)"),
            ),
            (
                "one hundred percent",
                pressed(letter("1"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("View(HundredPercent)"),
            ),
            (
                "thirds",
                pressed(letter("o"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("View(ToggleThirds)"),
            ),
            (
                "pointer mode",
                pressed(letter("v"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("View(SetMode"),
            ),
            (
                "compare begins on the first press",
                pressed(letter("\\"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("History(CompareBegin)"),
            ),
            (
                "a repeated backslash press",
                held(letter("\\"), Modifiers::empty(), true),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "a repeated letter press",
                held(letter("f"), Modifiers::empty(), true),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "a letter typed into a focused field",
                pressed(letter("f"), Modifiers::empty()),
                Status::Captured,
                &plain,
                None,
            ),
            (
                "a thirds letter typed into a focused field",
                pressed(letter("o"), Modifiers::empty()),
                Status::Captured,
                &plain,
                None,
            ),
            (
                "focus next",
                pressed(Key::Named(Named::Tab), Modifiers::empty()),
                Status::Captured,
                &plain,
                Some("View(FocusNext)"),
            ),
            (
                "focus previous",
                pressed(Key::Named(Named::Tab), Modifiers::SHIFT),
                Status::Captured,
                &plain,
                Some("View(FocusPrevious)"),
            ),
            (
                "mode letter",
                pressed(letter("r"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                Some("View(SetMode"),
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
        let release = released(Key::Named(Named::Space));
        assert!(keymap(&release, Status::Ignored, &drafting).is_some());
        assert!(keymap(&release, Status::Ignored, &plain).is_none());
        // Compare's release arrives whatever took the press, and whatever else is open.
        for (case, status, context) in [
            ("plain", Status::Ignored, &plain),
            ("a field has focus", Status::Captured, &plain),
            ("a draft is open", Status::Ignored, &drafting),
        ] {
            let mapped = keymap(&released(letter("\\")), status, context);
            assert!(
                mapped.as_ref().is_some_and(
                    |message| format!("{message:?}").starts_with("History(CompareEnd)")
                ),
                "{case}: {mapped:?}"
            );
        }
        assert!(
            keymap(&released(letter("f")), Status::Ignored, &plain).is_none(),
            "no other release is bound"
        );
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
