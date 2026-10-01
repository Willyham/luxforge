//! The keyboard table, as one pure function. Key codes never reach the update function: an event
//! becomes a semantic message here or nothing at all, so the whole mapping is testable without a
//! window.
use crate::app::Editor;
use crate::app::message::{
    Message, crop::CropMessage, draft::DraftMessage, export::ExportMessage,
    history::HistoryMessage, mask::BrushEdit, mask::KindMenu, mask::MaskKey, mask::MaskMessage,
    mask::TypingEdit, overlay::OverlayMessage, palette::PaletteMessage, sync::SyncMessage,
    view::ViewMessage,
};
use crate::app::message::{
    develop::DevelopMessage,
    loupe::LoupeMessage,
    select::{SelectMessage, Step},
    select_catalog::CatalogMessage,
};
use crate::state::palette::Panel;
use crate::state::select::{SelectPanel, Shown};
use crate::state::select_catalog::CatalogAction;
use iced::{
    Event, Subscription,
    event::Status,
    keyboard::{Event as Keys, Key, key::Named},
};
use luxforge_core::{MASK_MODE, POINTER_MODE};

/// What the mapping depends on: whether a draft is open, whether the palette has the keyboard, and
/// the canvas modes the registry offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyContext {
    pub(crate) gallery_open: bool,
    /// A crop draft or a mask shape gesture is open, so Enter applies it and Escape cancels it
    /// through the one draft lifecycle, whichever it is.
    pub(crate) drafting: bool,
    /// The open draft is the crop's, so Space pans the photograph and Option scales a handle about
    /// the centre. No other draft has these keys.
    pub(crate) crop: bool,
    /// A slider gesture's draft is open, so Escape discards it and the arrow key that is stepping
    /// it commits it on key-up.
    pub(crate) slider_drafting: bool,
    /// Mask mode is active, so the brush's own keys are live: the brackets size it, the shifted
    /// brackets feather it, and the erase modifier erases while it is held. They are here rather
    /// than only while a stroke is down, because the brush is sized before it is put down.
    pub(crate) mask_brush: bool,
    /// The command palette is open, so Escape closes it rather than reaching a draft.
    pub(crate) palette_open: bool,
    /// The title bar's Export menu is open, so Escape closes it.
    pub(crate) export_menu_open: bool,
    /// A module's canvas mode is active, so Escape leaves it. A mode that owns a draft answers
    /// Escape with its own cancel first; a mode without one has nothing to discard.
    pub(crate) mode_active: bool,
    /// Where Escape leaves a pick mode for: Mask for a pick taken on a mask, which returns to the
    /// Masks panel it was entered from, and the pointer otherwise (`None`).
    pub(crate) leave_to: Option<String>,
    /// The declared canvas-mode shortcut letters and the module each one selects.
    pub(crate) modes: Vec<(char, String)>,
    /// The Masks panel's text field is open, so Escape closes it without sending anything,
    /// whichever widget last saw the key.
    pub(crate) mask_typing: bool,
    /// One of the Masks panel's menus is open, so Escape closes it before anything else hears it.
    pub(crate) mask_menu_open: bool,
    /// The New mask or Add component menu is open, with the letter of each kind it lists: while it
    /// is, a kind's letter starts that kind, ahead of any canvas-mode letter.
    pub(crate) kind_menu: Option<(KindMenu, Vec<(char, String)>)>,
    /// Mask mode is active with no shape gesture open, so the panel's keys act on its selection:
    /// `X` inverts, `⌫` deletes, the arrows move the selection and `⌥` with them reorders.
    pub(crate) mask_keys: bool,
    /// The Select workspace is shown: its own keys act, and none of Develop's.
    pub(crate) select: bool,
    /// One of Select's chip or sort menus is open, so Escape closes it.
    pub(crate) select_menu_open: bool,
    /// The loupe is open over Select's centre, so Escape returns to the grid.
    pub(crate) loupe_open: bool,
    /// Develop N's confirmation is open, so Escape cancels it and Return develops.
    pub(crate) develop_confirm: bool,
    /// Develop has a development set, so `←` and `→` move through it.
    pub(crate) development_set: bool,
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
    // press would otherwise leave the original preview on screen with nothing to end it. Shift+\
    // arrives as `|` on a layout that shifts the backslash to it, so either key ends either hold,
    // whichever of the two keys is let go first.
    if let Keys::KeyReleased { key, .. } = keyboard
        && (character(key, "\\") || character(key, "|"))
    {
        return Some(Message::History(HistoryMessage::CompareEnd));
    }
    // The Select workspace has its own keys (`docs/design/catalog.md#keyboard`). None of Develop's
    // reaches it, so nothing acts on a photograph it does not show.
    if context.select {
        return select_keys(keyboard, status, context);
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
    if context.crop {
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
        // Export the displayed entry; Shift keeps its metadata.
        if character(key, "e") {
            return Some(Message::Export(ExportMessage::Start {
                keep_metadata: modifiers.shift(),
            }));
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
            // The filmstrip collapses and expands with the side panels' modifiers.
            if context.development_set && character(key, "f") {
                return Some(Message::Develop(DevelopMessage::Collapse));
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
    // An open Export menu closes on Escape before anything else hears it, as a native menu does.
    if context.export_menu_open && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::View(ViewMessage::CloseMenu));
    }
    // The Masks panel's text field answers Escape by closing with nothing sent. The field has
    // focus and has already taken the key, so this acts whatever `status` says.
    if context.mask_typing && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::Mask(MaskMessage::Typing(TypingEdit::Cancel)));
    }
    // So does an open Masks panel menu: Escape puts the menu away before it reaches an armed brush
    // or the mode.
    if context.mask_menu_open && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::View(ViewMessage::CloseMenu));
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
    // `G` shows the Select workspace. The switch answers the one start refusal, so an open draft
    // refuses it with its reason.
    if !*repeat && character(key, "g") && plain(modifiers) {
        return Some(Message::Select(SelectMessage::Switch(Shown::Select)));
    }
    // While a kind menu is open its letters start its kinds. The menu is what the person is looking
    // at, so its letters win over a canvas-mode letter that happens to be the same.
    if let Some((menu, letters)) = &context.kind_menu
        && !*repeat
        && let Some((_, kind)) = letters
            .iter()
            .find(|(letter, _)| character(key, letter.to_string().as_str()))
    {
        return Some(Message::Mask(MaskMessage::Choose {
            menu: *menu,
            kind: kind.clone(),
        }));
    }
    if context.crop && matches!(key, Key::Named(Named::Space)) {
        return Some(Message::Crop(CropMessage::Space(true)));
    }
    // The crop draft and a mask shape gesture answer the same two keys, because they are the same
    // kind of draft.
    if context.drafting {
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
    // The Masks panel's keys, on its selection. The arrows move it, and with Option reorder it;
    // they repeat while held, as walking a list does.
    if context.mask_keys {
        let step = match key {
            Key::Named(Named::ArrowUp) => Some(-1),
            Key::Named(Named::ArrowDown) => Some(1),
            _ => None,
        };
        if let Some(step) = step {
            return Some(Message::Mask(MaskMessage::Key(if modifiers.alt() {
                MaskKey::Move(step)
            } else {
                MaskKey::Select(step)
            })));
        }
        if !*repeat {
            if matches!(key, Key::Named(Named::Backspace | Named::Delete)) {
                return Some(Message::Mask(MaskMessage::Key(MaskKey::Delete)));
            }
            if character(key, "x") && !modifiers.shift() {
                return Some(Message::Mask(MaskMessage::Key(MaskKey::Invert)));
            }
        }
    }
    // `←` and `→` move through the development set when no text field, draft or gesture holds
    // them, and repeat while held. A slider on the pointer's rail captures them itself.
    if context.development_set
        && !context.drafting
        && !context.slider_drafting
        && !context.mask_keys
        && !modifiers.shift()
        && !modifiers.alt()
        && !modifiers.control()
        && !modifiers.logo()
    {
        match key {
            Key::Named(Named::ArrowLeft) => {
                return Some(Message::Develop(DevelopMessage::Step(-1)));
            }
            Key::Named(Named::ArrowRight) => {
                return Some(Message::Develop(DevelopMessage::Step(1)));
            }
            _ => {}
        }
    }
    // A canvas mode without a draft of its own — a pick mode — is left with Escape, which commits
    // nothing. A mode that owns a draft answered Escape above by cancelling that draft, which is
    // what returns it to the pointer.
    if context.mode_active && !context.drafting && matches!(key, Key::Named(Named::Escape)) {
        let leave = context.leave_to.as_deref().unwrap_or(POINTER_MODE);
        return Some(Message::View(ViewMessage::SetMode(leave.into())));
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
    // O is the mask overlay in Mask mode and the thirds guide elsewhere; modified, it is neither.
    if character(key, "o") {
        if modifiers.alt() || modifiers.control() || modifiers.shift() || modifiers.logo() {
            return None;
        }
        return Some(if context.mask_brush {
            Message::Mask(MaskMessage::ToggleOverlay)
        } else {
            Message::View(ViewMessage::ToggleThirds)
        });
    }
    // Both clipping overlays at once. The histogram's triangles toggle them one at a time; this
    // key and the title bar's Clipping button move the pair together.
    if character(key, "j") {
        return Some(Message::Overlay(OverlayMessage::ToggleClipping(None)));
    }
    if character(key, "v") {
        return Some(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
    }
    // M enters Mask mode. It is a host key, because a mask is a host
    // object: no module declares this mode, so no module's letter can claim them.
    if character(key, "m") {
        return (!modifiers.shift() && !modifiers.alt() && !modifiers.control())
            .then(|| Message::View(ViewMessage::SetMode(MASK_MODE.into())));
    }
    // Compare holds the Original framed as the displayed entry is framed; Shift holds the whole,
    // uncropped Original. A layout that shifts the backslash to `|` reports that character.
    if character(key, "|") || (character(key, "\\") && modifiers.shift()) {
        return Some(Message::History(HistoryMessage::CompareUncropped));
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

/// No modifier held.
fn plain(modifiers: &iced::keyboard::Modifiers) -> bool {
    !modifiers.shift() && !modifiers.alt() && !modifiers.control() && !modifiers.logo()
}

/// The Select workspace's keys: Escape closes an open menu whatever has focus; otherwise only a key
/// no text field took acts. The arrows move the active item and repeat while held, with Shift
/// extending the selection; `Cmd+A` and `Cmd+D` select all and none; `Cmd+Z` and `Shift+Cmd+Z`
/// undo and redo this desktop's library changes; `Cmd+F` puts the focus in the search field (the
/// catalog's over the catalog, the sources panel's otherwise); `Tab` toggles the side panels, and
/// `Cmd+Option+[` and `]` one each, as in Develop; `Cmd+O` adds a folder to the indexed folders;
/// `S` collapses or expands the active burst; `P`
/// picks or clears the selection; `D` develops the active frame, picking it when it is not picked
/// (over the catalog it opens Develop on the active photograph with the view as its set), and
/// `Cmd+Return` the picks in view, through Develop N's confirmation, which Escape cancels and
/// Return confirms; the loupe's keys are the loupe's.
fn select_keys(keyboard: &Keys, status: Status, context: &KeyContext) -> Option<Message> {
    let Keys::KeyPressed {
        key,
        modifiers,
        repeat,
        ..
    } = keyboard
    else {
        return None;
    };
    if context.select_menu_open && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::Select(SelectMessage::Menu(None)));
    }
    // Develop N's confirmation takes Escape whatever has focus; its name field takes Return itself.
    if context.develop_confirm && matches!(key, Key::Named(Named::Escape)) {
        return Some(Message::Develop(DevelopMessage::Cancel));
    }
    if context.loupe_open && matches!(key, Key::Named(Named::Escape)) && status == Status::Ignored {
        return Some(Message::Select(SelectMessage::Loupe(LoupeMessage::Close)));
    }
    if status != Status::Ignored {
        return None;
    }
    // The loupe's own keys, ahead of the grid's (`app/loupe.rs`).
    if context.loupe_open
        && let Some(message) = crate::app::loupe::loupe_keys(key, modifiers, *repeat)
    {
        return Some(message);
    }
    if modifiers.command() {
        if modifiers.alt() {
            if character(key, "[") {
                return Some(Message::Select(SelectMessage::TogglePanel(
                    SelectPanel::Sources,
                )));
            }
            if character(key, "]") {
                return Some(Message::Select(SelectMessage::TogglePanel(
                    SelectPanel::Info,
                )));
            }
            return None;
        }
        if *repeat {
            return None;
        }
        // Develop N: `Cmd+Return` develops the picks in view.
        if matches!(key, Key::Named(Named::Enter)) && !modifiers.shift() {
            return Some(Message::Develop(DevelopMessage::Open));
        }
        // Library undo and redo: in Select, `Cmd+Z` and `Shift+Cmd+Z` are the journal's, never
        // the photograph's history (the design's P10).
        if character(key, "z") {
            return Some(Message::Select(if modifiers.shift() {
                SelectMessage::Redo
            } else {
                SelectMessage::Undo
            }));
        }
        if modifiers.shift() {
            return None;
        }
        if character(key, "a") {
            return Some(Message::Select(SelectMessage::SelectAll));
        }
        if character(key, "d") {
            return Some(Message::Select(SelectMessage::SelectNone));
        }
        // Add a folder…; in Develop `Cmd+O` opens a single file.
        if character(key, "o") {
            return Some(Message::Select(SelectMessage::AddFolder));
        }
        if character(key, "f") {
            return Some(Message::Select(SelectMessage::Catalog(
                CatalogMessage::Act(CatalogAction::FocusSearch),
            )));
        }
        return None;
    }
    let step = match key {
        Key::Named(Named::ArrowLeft) => Some(Step::Left),
        Key::Named(Named::ArrowRight) => Some(Step::Right),
        Key::Named(Named::ArrowUp) => Some(Step::Up),
        Key::Named(Named::ArrowDown) => Some(Step::Down),
        _ => None,
    };
    if let Some(step) = step {
        if modifiers.alt() || modifiers.control() || modifiers.logo() {
            return None;
        }
        return Some(Message::Select(SelectMessage::Move {
            step,
            extend: modifiers.shift(),
        }));
    }
    if *repeat || !plain(modifiers) {
        return None;
    }
    // Return develops once the confirmation is open.
    if context.develop_confirm && matches!(key, Key::Named(Named::Enter)) {
        return Some(Message::Develop(DevelopMessage::Confirm));
    }
    // `D`, over the grid or in the loupe: develop the active frame, picking it when it is not
    // picked; over the catalog, Develop on the active photograph with the view's photographs as the
    // set.
    if character(key, "d") {
        return Some(Message::Develop(DevelopMessage::Key));
    }
    if matches!(key, Key::Named(Named::Tab)) {
        return Some(Message::Select(SelectMessage::TogglePanels));
    }
    if character(key, "s") {
        return Some(Message::Select(SelectMessage::Collapse));
    }
    // `P` picks or clears the selection over the grid; the loupe's `P` is the loupe's own.
    if !context.loupe_open && character(key, "p") {
        return Some(Message::Select(SelectMessage::Pick));
    }
    // `Delete` asks to remove the selection from the catalog; over files it does nothing.
    if !context.loupe_open && matches!(key, Key::Named(Named::Backspace | Named::Delete)) {
        return Some(Message::Select(SelectMessage::Catalog(
            CatalogMessage::Act(CatalogAction::Remove),
        )));
    }
    // `Space` or `E` shows the active frame in the loupe.
    if !context.loupe_open && (matches!(key, Key::Named(Named::Space)) || character(key, "e")) {
        return Some(Message::Select(SelectMessage::Loupe(LoupeMessage::Open)));
    }
    None
}

/// The events the keyboard table can act on. Everything else never wakes the update function, so a
/// pointer move costs nothing here.
pub(super) fn raw_event(
    event: iced::Event,
    status: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    match &event {
        iced::Event::Keyboard(_) | iced::Event::Window(iced::window::Event::CloseRequested) => {
            Some(Message::Key(event, status))
        }
        // A file or folder dropped on the window: Select adds a folder to the indexed folders,
        // Develop opens a file.
        iced::Event::Window(iced::window::Event::FileDropped(path)) => {
            Some(Message::Select(SelectMessage::Dropped(path.clone())))
        }
        // A resize changes how large a fitted photograph is drawn, and so how fine a clipping
        // overlay's cells may be. It rides the subscription that is already listening; nothing new
        // polls for it, and a resize with no overlay on starts no work.
        iced::Event::Window(iced::window::Event::Resized(size)) => {
            Some(Message::View(ViewMessage::Resized(size.width, size.height)))
        }
        _ => None,
    }
}

/// The left button coming up anywhere in the window: the end of a reorder by drag, which the
/// subscription listens for only while one is in progress.
pub(super) fn drag_release(
    event: iced::Event,
    _: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    matches!(
        event,
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
            iced::mouse::Button::Left
        ))
    )
    .then_some(Message::Mask(MaskMessage::Drag(
        crate::app::message::mask::DragEdit::End,
    )))
}

/// Every raw window and keyboard event reaches the keyboard table, always.
pub(super) fn subscription(_: &Editor) -> Subscription<Message> {
    iced::event::listen_with(raw_event)
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
            crop: false,
            slider_drafting: false,
            mask_brush: false,
            palette_open: false,
            export_menu_open: false,
            mode_active: false,
            leave_to: None,
            modes: vec![('R', "luxforge.crop".into())],
            mask_typing: false,
            mask_menu_open: false,
            kind_menu: None,
            mask_keys: false,
            select: false,
            select_menu_open: false,
            loupe_open: false,
            develop_confirm: false,
            development_set: false,
        }
    }

    #[test]
    fn o_is_the_mask_visibility_key_and_respects_field_focus_and_repeat() {
        let o = pressed(letter("o"), Modifiers::empty());
        let mask = KeyContext {
            mask_brush: true,
            ..context()
        };
        assert!(matches!(
            keymap(&o, Status::Ignored, &mask),
            Some(Message::Mask(MaskMessage::ToggleOverlay))
        ));
        assert!(matches!(
            keymap(&o, Status::Ignored, &context()),
            Some(Message::View(ViewMessage::ToggleThirds))
        ));
        assert!(keymap(&o, Status::Captured, &mask).is_none());
        assert!(
            keymap(
                &held(letter("o"), Modifiers::empty(), true),
                Status::Ignored,
                &mask
            )
            .is_none()
        );
        for modifier in [Modifiers::SHIFT, Modifiers::ALT] {
            assert!(keymap(&pressed(letter("o"), modifier), Status::Ignored, &mask).is_none());
        }
        assert!(matches!(
            keymap(
                &pressed(letter("o"), Modifiers::COMMAND),
                Status::Ignored,
                &mask
            ),
            Some(Message::Sync(SyncMessage::Open))
        ));
        assert!(
            keymap(
                &pressed(letter("m"), Modifiers::SHIFT),
                Status::Ignored,
                &mask
            )
            .is_none()
        );
    }

    /// The Masks panel's keys act only in their context: a kind menu's letters only while it is
    /// open, and then ahead of a canvas-mode letter; the selection keys only in Mask mode and never
    /// once a field has taken the key; Escape closes the panel's field or menu first.
    #[test]
    fn the_masks_panel_keys_act_only_in_their_context() {
        let r = pressed(letter("r"), Modifiers::empty());
        let x = pressed(letter("x"), Modifiers::empty());
        let escape = pressed(Key::Named(Named::Escape), Modifiers::empty());
        let delete = pressed(Key::Named(Named::Backspace), Modifiers::empty());
        let up = pressed(Key::Named(Named::ArrowUp), Modifiers::ALT);
        let plain = context();
        assert!(matches!(
            keymap(&r, Status::Ignored, &plain),
            Some(Message::View(ViewMessage::SetMode(mode))) if mode == "luxforge.crop"
        ));
        assert!(keymap(&x, Status::Ignored, &plain).is_none());
        assert!(keymap(&delete, Status::Ignored, &plain).is_none());
        let menu = KeyContext {
            kind_menu: Some((KindMenu::Add, vec![('R', "radial".into())])),
            mask_menu_open: true,
            mask_keys: true,
            ..context()
        };
        assert!(matches!(
            keymap(&r, Status::Ignored, &menu),
            Some(Message::Mask(MaskMessage::Choose { menu: KindMenu::Add, kind })) if kind == "radial"
        ));
        assert!(matches!(
            keymap(&escape, Status::Ignored, &menu),
            Some(Message::View(ViewMessage::CloseMenu))
        ));
        let keys = KeyContext {
            mask_keys: true,
            ..context()
        };
        assert!(matches!(
            keymap(&x, Status::Ignored, &keys),
            Some(Message::Mask(MaskMessage::Key(MaskKey::Invert)))
        ));
        assert!(matches!(
            keymap(&delete, Status::Ignored, &keys),
            Some(Message::Mask(MaskMessage::Key(MaskKey::Delete)))
        ));
        assert!(matches!(
            keymap(&up, Status::Ignored, &keys),
            Some(Message::Mask(MaskMessage::Key(MaskKey::Move(-1))))
        ));
        assert!(keymap(&delete, Status::Captured, &keys).is_none());
        let typing = KeyContext {
            mask_typing: true,
            ..keys
        };
        assert!(matches!(
            keymap(&escape, Status::Captured, &typing),
            Some(Message::Mask(MaskMessage::Typing(TypingEdit::Cancel)))
        ));
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
            crop: true,
            ..context()
        };
        let mask_drafting = KeyContext {
            drafting: true,
            ..context()
        };
        let palette = KeyContext {
            palette_open: true,
            ..context()
        };
        let drafting_palette = KeyContext {
            drafting: true,
            crop: true,
            palette_open: true,
            ..context()
        };
        let export_menu = KeyContext {
            export_menu_open: true,
            ..context()
        };
        let drafting_export_menu = KeyContext {
            drafting: true,
            crop: true,
            export_menu_open: true,
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
                "export",
                pressed(letter("e"), command),
                Status::Ignored,
                &plain,
                Some("Export(Start { keep_metadata: false })"),
            ),
            (
                "export keeping metadata",
                pressed(letter("E"), shift_command),
                Status::Ignored,
                &plain,
                Some("Export(Start { keep_metadata: true })"),
            ),
            (
                "a plain e is no shortcut",
                pressed(letter("e"), Modifiers::empty()),
                Status::Ignored,
                &plain,
                None,
            ),
            (
                "Escape closes the Export menu",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &export_menu,
                Some("View(CloseMenu)"),
            ),
            (
                "the Export menu's Escape beats a draft's",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &drafting_export_menu,
                Some("View(CloseMenu)"),
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
                "shift and backslash hold the whole original",
                pressed(letter("\\"), Modifiers::SHIFT),
                Status::Ignored,
                &plain,
                Some("History(CompareUncropped)"),
            ),
            (
                "the shifted backslash as a US layout reports it",
                pressed(letter("|"), Modifiers::SHIFT),
                Status::Ignored,
                &plain,
                Some("History(CompareUncropped)"),
            ),
            (
                "a repeated shifted backslash press",
                held(letter("|"), Modifiers::SHIFT, true),
                Status::Ignored,
                &plain,
                None,
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
                Some("Draft(Commit)"),
            ),
            (
                "cancel the draft",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &drafting,
                Some("Draft(Cancel)"),
            ),
            (
                "apply a mask gesture the same way",
                pressed(Key::Named(Named::Enter), Modifiers::empty()),
                Status::Ignored,
                &mask_drafting,
                Some("Draft(Commit)"),
            ),
            (
                "cancel a mask gesture the same way",
                pressed(Key::Named(Named::Escape), Modifiers::empty()),
                Status::Ignored,
                &mask_drafting,
                Some("Draft(Cancel)"),
            ),
            (
                "space pans only a crop draft",
                pressed(Key::Named(Named::Space), Modifiers::empty()),
                Status::Ignored,
                &mask_drafting,
                None,
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
            (
                "option scales only a crop draft's handles",
                Event::Keyboard(Keys::ModifiersChanged(Modifiers::ALT)),
                Status::Ignored,
                &mask_drafting,
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
        // Space released ends the pan only while a crop draft is open.
        let release = released(Key::Named(Named::Space));
        assert!(keymap(&release, Status::Ignored, &drafting).is_some());
        assert!(keymap(&release, Status::Ignored, &plain).is_none());
        assert!(keymap(&release, Status::Ignored, &mask_drafting).is_none());
        // Compare's release arrives whatever took the press, and whatever else is open.
        for (case, status, context) in [
            ("plain", Status::Ignored, &plain),
            ("a field has focus", Status::Captured, &plain),
            ("a draft is open", Status::Ignored, &drafting),
        ] {
            // Either key of either hold ends it: `\`, or `|` when Shift is still down.
            for key in ["\\", "|"] {
                let mapped = keymap(&released(letter(key)), status, context);
                assert!(
                    mapped.as_ref().is_some_and(
                        |message| format!("{message:?}").starts_with("History(CompareEnd)")
                    ),
                    "{case}, {key}: {mapped:?}"
                );
            }
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

    /// Compare is a hold, so the keyboard subscription has to forward releases as well as presses;
    /// a filter that admitted only presses would leave the original preview stuck on screen.
    #[test]
    fn the_event_filter_forwards_key_releases_as_well_as_presses() {
        let window = iced::window::Id::unique();
        let key = iced::keyboard::Key::Character("\\".into());
        let release = iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
        });
        assert!(matches!(
            raw_event(release, iced::event::Status::Ignored, window),
            Some(Message::Key(..))
        ));
        assert!(
            raw_event(
                iced::Event::Mouse(iced::mouse::Event::CursorLeft),
                iced::event::Status::Ignored,
                window
            )
            .is_none(),
            "a pointer event still never wakes the update function"
        );
        // A file or folder dropped on the window reaches Select, which adds a folder in Select and
        // opens a file in Develop.
        let dropped = raw_event(
            iced::Event::Window(iced::window::Event::FileDropped(
                "/Users/w/Card dumps".into(),
            )),
            iced::event::Status::Ignored,
            window,
        );
        assert!(matches!(
            dropped,
            Some(Message::Select(SelectMessage::Dropped(path))) if path == std::path::Path::new("/Users/w/Card dumps")
        ));
    }
}
