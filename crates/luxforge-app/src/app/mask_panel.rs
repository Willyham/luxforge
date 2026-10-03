//! The Masks panel's own gestures: the kind menus, renaming a row in place, typing a gesture's or
//! the brush's numbers, reordering by drag, and the panel's keys.
//!
//! None of these is a new command. Each resolves to a message the panel already had — a
//! [`RowEdit`] for a list edit, [`MaskMessage::New`], [`MaskMessage::Add`] or
//! [`MaskMessage::Paint`] for a kind, [`MaskMessage::Field`] or a [`BrushEdit`] for a number — so
//! the request a drag, a key or a menu sends is the request the row's Copy as JSON request shows,
//! built once by [`Editor::row_command`]. A gesture the host would refuse is stated in the status
//! line in the host's own words and sends nothing.
use crate::app::Before;
use crate::{
    app::{
        Editor,
        message::{
            Message, mask::BrushEdit, mask::DragEdit, mask::KindMenu, mask::MaskKey,
            mask::MaskMessage, mask::PaintTarget, mask::RowEdit, mask::TypingEdit,
        },
    },
    state::{
        MenuTarget,
        masks::{DragItem, MaskDrag, MaskTyping, TypingTarget, panel_menu, parse_number},
    },
};
use iced::{Subscription, Task};
use luxforge_core::mask::{commands::MaskReport, rules};

/// The identity of the typed box of one of the open gesture's fields, so Begin can focus it.
pub(crate) fn draft_field_id(name: &str) -> String {
    format!("mask-draft-{name}")
}

/// The identity of the typed box of one brush number, so Begin can focus it.
pub(crate) fn brush_field_id(name: &str) -> String {
    format!("mask-brush-{name}")
}

impl Editor {
    /// Whether one of the panel's own menus is open.
    pub(crate) fn mask_menu_open(&self) -> bool {
        self.view_state.menu.as_ref().is_some_and(panel_menu)
    }

    /// Which kind menu is open, if one is: the context in which a kind's letter starts that kind.
    pub(crate) fn kind_menu_open(&self) -> Option<KindMenu> {
        match self.view_state.menu.as_ref() {
            Some(MenuTarget::NewMask) => Some(KindMenu::New),
            Some(MenuTarget::AddComponent) => Some(KindMenu::Add),
            _ => None,
        }
    }

    /// Close the panel's menu, when one of its own is open. A choice from a menu, and any edit the
    /// panel makes, puts the menu away as a native menu does.
    pub(crate) fn close_mask_menu(&mut self) {
        if self.mask_menu_open() {
            self.view_state.menu = None;
        }
    }

    /// Drop view state that names a row the listing no longer holds: a rename or a drag of a mask or
    /// component that was undone away or deleted elsewhere.
    pub(crate) fn drop_stale_panel_state(&mut self) {
        let listing = self.document.masks.as_ref();
        let has_mask = |id: &str| {
            listing.is_some_and(|listing| listing.masks.iter().any(|mask| mask.id.as_str() == id))
        };
        let has_component = |id: &str| {
            listing.is_some_and(|listing| {
                listing.masks.iter().any(|mask| {
                    mask.components
                        .iter()
                        .any(|component| component.id.as_str() == id)
                })
            })
        };
        let stale_typing = match self.mask_panel.typing.as_ref().map(|typing| &typing.target) {
            Some(TypingTarget::RenameMask(id)) => !has_mask(id),
            Some(TypingTarget::RenameComponent(id)) => !has_component(id),
            Some(TypingTarget::DraftField(_)) => self.mask_shape().is_none(),
            Some(TypingTarget::Brush(_)) | None => false,
        };
        let stale_drag = match self.mask_panel.drag.as_ref().map(|drag| &drag.item) {
            Some(DragItem::Mask(id)) => !has_mask(id),
            Some(DragItem::Component(id)) => !has_component(id),
            None => false,
        };
        if stale_typing {
            self.mask_panel.typing = None;
        }
        if stale_drag {
            self.mask_panel.drag = None;
        }
    }

    /// The open mask's report, when one is open and listed.
    fn panel_open_mask(&self) -> Option<&MaskReport> {
        let id = self.mask_panel.selected_mask.as_ref()?;
        self.document
            .masks
            .as_ref()?
            .masks
            .iter()
            .find(|report| &report.id == id)
    }

    // ---- the kind menus ------------------------------------------------------------------------

    /// A kind chosen from the New mask or Add component menu, by a press or by its letter.
    pub(crate) fn choose_kind(&mut self, menu: KindMenu, kind: String) -> Task<Message> {
        self.close_mask_menu();
        let painted = crate::mask_draft::paintable(&kind);
        let message = match (menu, painted) {
            (KindMenu::New, true) => MaskMessage::Paint(PaintTarget::NewMask),
            (KindMenu::Add, true) => MaskMessage::Paint(PaintTarget::NewBrush),
            (KindMenu::New, false) => MaskMessage::New(kind),
            (KindMenu::Add, false) => MaskMessage::Add(kind),
        };
        self.mask_message(message)
    }

    // ---- the panel's one text field ------------------------------------------------------------

    /// One edit of the panel's text field.
    pub(crate) fn mask_typing_edit(&mut self, edit: TypingEdit) -> Task<Message> {
        match edit {
            TypingEdit::Begin(target) => {
                self.close_mask_menu();
                let Some(text) = self.typing_seed(&target) else {
                    return Task::none();
                };
                let focus = match &target {
                    TypingTarget::RenameMask(_) | TypingTarget::RenameComponent(_) => {
                        luxforge_ui::rename_input_id()
                    }
                    TypingTarget::DraftField(name) => iced::widget::Id::from(draft_field_id(name)),
                    TypingTarget::Brush(name) => iced::widget::Id::from(brush_field_id(name)),
                };
                self.mask_panel.typing = Some(MaskTyping { target, text });
                iced::widget::operation::focus(focus)
            }
            TypingEdit::Text(text) => {
                if let Some(typing) = &mut self.mask_panel.typing {
                    typing.text = text;
                }
                Task::none()
            }
            TypingEdit::Cancel => {
                self.mask_panel.typing = None;
                Task::none()
            }
            TypingEdit::Submit => self.submit_typing(),
        }
    }

    /// What a field shows as it opens: the name it renames, or the number as the panel writes it.
    fn typing_seed(&self, target: &TypingTarget) -> Option<String> {
        let model = &self.workspace.masks;
        match target {
            TypingTarget::RenameMask(id) => model
                .masks
                .iter()
                .find(|row| row.id.as_str() == id)
                .map(|row| row.name.clone()),
            TypingTarget::RenameComponent(id) => model
                .components
                .iter()
                .find(|row| row.id.as_str() == id)
                .map(|row| row.name.clone()),
            TypingTarget::DraftField(name) => model
                .draft
                .as_ref()?
                .fields
                .iter()
                .find(|field| &field.name == name)
                .map(|field| field.text.clone()),
            TypingTarget::Brush(name) => model
                .brush
                .fields
                .iter()
                .find(|field| &field.name == name)
                .map(|field| field.text.clone()),
        }
    }

    /// Enter in the panel's text field: send what it describes, or say why not and keep it open.
    fn submit_typing(&mut self) -> Task<Message> {
        let Some(MaskTyping { target, text }) = self.mask_panel.typing.clone() else {
            return Task::none();
        };
        match target {
            TypingTarget::RenameMask(mask) => {
                let name = text.trim().to_owned();
                if name.is_empty() {
                    self.status.text =
                        "A mask's name needs at least one printable character".into();
                    return Task::none();
                }
                self.mask_panel.typing = None;
                let unchanged = self
                    .document
                    .masks
                    .as_ref()
                    .and_then(|listing| listing.masks.iter().find(|m| m.id.as_str() == mask))
                    .is_some_and(|report| report.name == name);
                if unchanged {
                    return Task::none();
                }
                self.mask_message(MaskMessage::Row(RowEdit::RenameMask { mask, name }))
            }
            TypingTarget::RenameComponent(component) => {
                let name = text.trim().to_owned();
                if name.is_empty() {
                    self.status.text =
                        "A component's name needs at least one printable character".into();
                    return Task::none();
                }
                self.mask_panel.typing = None;
                let unchanged = self.panel_open_mask().is_some_and(|report| {
                    report
                        .components
                        .iter()
                        .any(|known| known.id.as_str() == component && known.name == name)
                });
                if unchanged {
                    return Task::none();
                }
                self.mask_message(MaskMessage::Row(RowEdit::RenameComponent {
                    component,
                    name,
                }))
            }
            TypingTarget::DraftField(name) => {
                let invalid = self
                    .workspace
                    .masks
                    .draft
                    .as_ref()
                    .and_then(|draft| draft.fields.iter().find(|field| field.name == name))
                    .and_then(|field| field.invalid.clone());
                match (parse_number(&text), invalid) {
                    (Some(value), None) => {
                        self.mask_panel.typing = None;
                        self.mask_message(MaskMessage::Field { name, value })
                    }
                    (_, reason) => {
                        self.status.text = reason.unwrap_or_else(|| "Type a number".into());
                        Task::none()
                    }
                }
            }
            TypingTarget::Brush(name) => {
                let invalid = self
                    .workspace
                    .masks
                    .brush
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .and_then(|field| field.invalid.clone());
                match (parse_number(&text), invalid) {
                    (Some(value), None) => {
                        self.mask_panel.typing = None;
                        self.mask_message(MaskMessage::Brush(BrushEdit::Set { name, value }))
                    }
                    (_, reason) => {
                        self.status.text = reason.unwrap_or_else(|| "Type a number".into());
                        Task::none()
                    }
                }
            }
        }
    }

    // ---- reorder by drag -----------------------------------------------------------------------

    /// One step of a reorder by drag.
    pub(crate) fn mask_drag_edit(&mut self, edit: DragEdit) -> Task<Message> {
        match edit {
            DragEdit::Start(item) => {
                self.close_mask_menu();
                self.mask_panel.drag = Some(MaskDrag { item, over: None });
                Task::none()
            }
            DragEdit::Over(over) => {
                if let Some(drag) = &mut self.mask_panel.drag {
                    drag.over = over;
                }
                Task::none()
            }
            DragEdit::End => {
                let Some(MaskDrag { item, over }) = self.mask_panel.drag.take() else {
                    return Task::none();
                };
                let Some(to) = over else {
                    return Task::none();
                };
                match item {
                    DragItem::Mask(mask) => self.move_mask_to(mask, to),
                    DragItem::Component(component) => self.move_component_to(component, to),
                }
            }
        }
    }

    /// Reorder one mask to `to`, as a drag's release or `⌥`-Up and `⌥`-Down do, or say why not.
    fn move_mask_to(&mut self, mask: String, to: usize) -> Task<Message> {
        let Some(listing) = self.document.masks.as_ref() else {
            return Task::none();
        };
        let Some(from) = listing.masks.iter().position(|m| m.id.as_str() == mask) else {
            return Task::none();
        };
        if to == from {
            return Task::none();
        }
        if let Err(error) = rules::position(to as u64, listing.masks.len(), "masks") {
            self.status.text = error.detail;
            return Task::none();
        }
        self.mask_message(MaskMessage::Row(RowEdit::MoveMask { mask, index: to }))
    }

    /// Reorder one component of the open mask to `to`, or say why the host would refuse it.
    fn move_component_to(&mut self, component: String, to: usize) -> Task<Message> {
        let Some(report) = self.panel_open_mask() else {
            return Task::none();
        };
        let Some(from) = report
            .components
            .iter()
            .position(|known| known.id.as_str() == component)
        else {
            return Task::none();
        };
        if to == from {
            return Task::none();
        }
        let modes: Vec<_> = report.components.iter().map(|known| known.mode).collect();
        if let Err(error) = rules::reorder_component(&report.name, &modes, from, to as u64) {
            self.status.text = error.detail;
            return Task::none();
        }
        self.mask_message(MaskMessage::Row(RowEdit::MoveComponent {
            component,
            index: to,
        }))
    }

    // ---- the panel's keys ----------------------------------------------------------------------

    /// One of the panel's keys, resolved against the selection into the message it sends.
    pub(crate) fn mask_key(&mut self, key: MaskKey) -> Task<Message> {
        let Some(report) = self.panel_open_mask().cloned() else {
            // With no mask open the only key that means anything is a move into the list.
            if let MaskKey::Select(_) = key
                && let Some(first) = self
                    .document
                    .masks
                    .as_ref()
                    .and_then(|listing| listing.masks.first())
            {
                let id = first.id.as_str().to_owned();
                return self.mask_message(MaskMessage::Select(id));
            }
            return Task::none();
        };
        let selected = self.mask_panel.selected_component.as_ref().and_then(|id| {
            report
                .components
                .iter()
                .position(|component| &component.id == id)
        });
        match (key, selected) {
            (MaskKey::Invert, Some(index)) => {
                let component = &report.components[index];
                self.mask_message(MaskMessage::Row(RowEdit::ComponentInvert {
                    component: component.id.as_str().to_owned(),
                    invert: !component.invert,
                }))
            }
            (MaskKey::Invert, None) => {
                self.status.text = "Select a component to invert it".into();
                Task::none()
            }
            // A mask never exists empty, so its only component goes with the mask: the key does
            // what the row's menu offers in that position, Delete mask.
            (MaskKey::Delete, Some(_)) if report.components.len() == 1 => self.mask_message(
                MaskMessage::Row(RowEdit::DeleteMask(report.id.as_str().to_owned())),
            ),
            (MaskKey::Delete, Some(index)) => {
                if let Err(error) = rules::delete_component(&report.name, report.components.len()) {
                    self.status.text = error.detail;
                    return Task::none();
                }
                let component = report.components[index].id.as_str().to_owned();
                self.mask_message(MaskMessage::Row(RowEdit::DeleteComponent(component)))
            }
            (MaskKey::Delete, None) => self.mask_message(MaskMessage::Row(RowEdit::DeleteMask(
                report.id.as_str().to_owned(),
            ))),
            (MaskKey::Select(step), Some(index)) => {
                let Some(next) = step_index(index, step, report.components.len()) else {
                    return Task::none();
                };
                let id = report.components[next].id.as_str().to_owned();
                self.mask_message(MaskMessage::SelectComponent(id))
            }
            (MaskKey::Select(step), None) => {
                let Some(listing) = self.document.masks.as_ref() else {
                    return Task::none();
                };
                let Some(next) = step_index(report.index, step, listing.masks.len()) else {
                    return Task::none();
                };
                let id = listing.masks[next].id.as_str().to_owned();
                self.mask_message(MaskMessage::Select(id))
            }
            (MaskKey::Move(step), Some(index)) => {
                let Some(to) = step_index(index, step, report.components.len()) else {
                    self.status.text = move_refusal(&report.components[index].name, step);
                    return Task::none();
                };
                let component = report.components[index].id.as_str().to_owned();
                self.move_component_to(component, to)
            }
            (MaskKey::Move(step), None) => {
                let len = self
                    .document
                    .masks
                    .as_ref()
                    .map_or(0, |listing| listing.masks.len());
                let Some(to) = step_index(report.index, step, len) else {
                    self.status.text = move_refusal(&report.name, step);
                    return Task::none();
                };
                self.move_mask_to(report.id.as_str().to_owned(), to)
            }
        }
    }

    /// One change to a brush number from its slider or its label.
    pub(crate) fn brush_number(&self, edit: &BrushEdit) -> Option<(String, f64)> {
        match edit {
            BrushEdit::Fraction { name, fraction } => {
                let spec = self
                    .workspace
                    .masks
                    .brush
                    .fields
                    .iter()
                    .find(|field| &field.name == name)
                    .and_then(|field| field.spec)?;
                Some((name.clone(), spec.at_fraction(*fraction).as_f64()?))
            }
            BrushEdit::Reset(name) => crate::mask_draft::NEUTRAL_BRUSH
                .values()
                .into_iter()
                .find(|(known, _)| known == name)
                .map(|(_, value)| (name.clone(), value)),
            _ => None,
        }
    }
}

/// The index `step` places from `index` in a list of `len`, or `None` past either end.
fn step_index(index: usize, step: i8, len: usize) -> Option<usize> {
    let next = index.checked_add_signed(isize::from(step))?;
    (next < len).then_some(next)
}

/// Why a row cannot move `step` places: it is already at that end of its list.
fn move_refusal(name: &str, step: i8) -> String {
    if step < 0 {
        format!("{name} is already at the top of the list")
    } else {
        format!("{name} is already at the bottom of the list")
    }
}

/// After every message: the panel's selection follows the stack and the mode before anything is
/// derived from it, so a section is never bound to a mask the recipe no longer holds, and the
/// brush in hand follows the stack it paints on, and the selected gradient's handles rest on it.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if editor.follow_mask_selection() {
        editor.seed_values();
    }
    let armed = editor.follow_armed_brush();
    Task::batch([armed, editor.follow_resting_handles()])
}

/// A reorder by drag ends wherever the button comes up, inside the panel or not, so its release is
/// heard window-wide — and only while a row is being dragged.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    if editor.mask_panel.drag.is_none() {
        return Subscription::none();
    }
    iced::event::listen_with(crate::app::keymap::drag_release)
}
