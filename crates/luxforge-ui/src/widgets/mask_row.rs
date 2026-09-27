//! The Masks panel's rows, as the mask-panels board draws them: a mask's row in the list
//! (`.mrow`), a component's row under the open mask (`.crow`), a brush component's stroke rows
//! and the tertiary note a kind puts under its row.
//!
//! Each row is one button that selects it, holding the row's own small buttons; a press on one of
//! those is that button's and does not also select the row. A press on a row's drag handle (a
//! mask's thumbnail, a component's grip) publishes the caller's drag-start message instead, when
//! the caller gives one; how a drag then reorders is the caller's. A row's menu is the caller's to
//! open: wrap the row in [`crate::popover`] with a [`crate::menu_list`] and the menu drops under
//! the row, and a second press on the row's menu button closes it again.

use super::coverage_thumbnail::{CoverageThumbnailModel, coverage_thumbnail};
use super::icon_button::{Icon, IconButtonModel, icon, sized_icon_button};
use super::list_row::marker_circle;
use super::mode_control::{CombineMode, ModeControlModel, inert_mode_control, mode_control};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::text::Wrapping;
use iced::widget::{Space, button, container, mouse_area, row, text, tooltip};
use iced::{Alignment, Color, Element, Length, Padding};

/// Plain data for one mask's row.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskRowModel {
    pub name: String,
    pub coverage: CoverageThumbnailModel,
    /// A layer bound to the mask is non-neutral: the accent dot.
    pub active: bool,
    /// The amount as the row reads it, without a unit (`80`).
    pub amount: String,
    /// The mask's overlay visibility: the eye, or the closed eye when hidden.
    pub visible: bool,
    pub visibility_tooltip: String,
    pub menu_tooltip: String,
    /// The mask is the open one: the row is tinted.
    pub selected: bool,
    /// The row's menu is open: its button is drawn selected.
    pub menu_open: bool,
    pub enabled: bool,
}

/// What a mask row publishes. `None` leaves that part inert.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskRowMessages<M> {
    /// A press on the row.
    pub on_select: Option<M>,
    /// A press on the eye.
    pub on_toggle_visibility: Option<M>,
    /// A press on the menu button.
    pub on_menu: Option<M>,
    /// A press on the thumbnail, the row's drag handle. `None` lets the press select the row.
    pub on_drag_start: Option<M>,
}

/// Renders one mask's row: the coverage thumbnail, the name, the accent dot, the amount, the eye
/// and the menu button.
pub fn mask_row<'a, M: Clone + 'a>(
    model: &MaskRowModel,
    messages: MaskRowMessages<M>,
) -> Element<'a, M> {
    let enabled = model.enabled;
    let thumbnail = drag_handle(
        coverage_thumbnail(&model.coverage),
        messages.on_drag_start.filter(|_| enabled),
    );
    let mut content = row![thumbnail, name(&model.name, model.selected, enabled)]
        .spacing(theme::MASK_ROW_SPACING)
        .align_y(Alignment::Center);
    if model.active {
        content = content.push(marker_circle(Some(theme::ACCENT), None));
    }
    content = content.push(
        text(model.amount.clone())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_TERTIARY)
            .wrapping(Wrapping::None)
            .align_x(Horizontal::Right)
            .width(Length::Fixed(theme::MASK_AMOUNT_WIDTH)),
    );
    let (eye, eye_ink) = if model.visible {
        (Icon::Eye, theme::TEXT_SECONDARY)
    } else {
        (Icon::EyeOff, theme::TEXT_FAINT)
    };
    content = content.push(small_button(
        eye,
        theme::HEADER_ICON_SIZE,
        eye_ink,
        &model.visibility_tooltip,
        false,
        enabled,
        messages.on_toggle_visibility,
    ));
    content = content.push(menu_button(
        &model.menu_tooltip,
        model.menu_open,
        enabled,
        messages.on_menu,
    ));
    row_button(
        content.into(),
        theme::MASK_ROW_PADDING,
        model.selected.then_some(theme::MASK_ROW_SELECTED),
        false,
        messages.on_select.filter(|_| enabled),
    )
}

/// Plain data for one component's row.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentRowModel {
    pub name: String,
    /// The kind's icon; `None` keeps its column empty, so the names still line up.
    pub icon: Option<Icon>,
    pub mode: ModeControlModel,
    /// The component is inverted: the invert glyph is in the accent.
    pub inverted: bool,
    pub invert_tooltip: String,
    pub menu_tooltip: String,
    /// The component is the selected one: the row is tinted.
    pub selected: bool,
    /// The overlay is showing this component's own coverage because the pointer is over its row.
    pub hovered: bool,
    pub menu_open: bool,
    pub enabled: bool,
}

/// What a component row publishes. `None` leaves that part inert.
pub struct ComponentRowMessages<'a, M> {
    pub on_select: Option<M>,
    /// A press on a mode segment, with its mode.
    pub on_mode: Option<Box<dyn Fn(CombineMode) -> M + 'a>>,
    pub on_invert: Option<M>,
    pub on_menu: Option<M>,
    /// A press on the grip. `None` lets the press select the row.
    pub on_drag_start: Option<M>,
    /// The pointer entered or left the row.
    pub on_hover_enter: Option<M>,
    pub on_hover_exit: Option<M>,
}

impl<M> Default for ComponentRowMessages<'_, M> {
    fn default() -> Self {
        Self {
            on_select: None,
            on_mode: None,
            on_invert: None,
            on_menu: None,
            on_drag_start: None,
            on_hover_enter: None,
            on_hover_exit: None,
        }
    }
}

/// Renders one component's row: the grip, the kind's icon, the name, the mode control, the invert
/// glyph and the menu button.
pub fn component_row<'a, M: Clone + 'a>(
    model: &ComponentRowModel,
    messages: ComponentRowMessages<'a, M>,
) -> Element<'a, M> {
    let enabled = model.enabled;
    let grip = drag_handle(
        icon(Icon::Grip, theme::GRIP_SIZE, theme::TEXT_FAINT),
        messages.on_drag_start.filter(|_| enabled),
    );
    let kind: Element<'a, M> = match model.icon {
        Some(kind) => icon(kind, theme::HEADER_ICON_SIZE, theme::TEXT_SECONDARY),
        None => Space::new().into(),
    };
    let mode = match messages.on_mode {
        Some(on_mode) if enabled => mode_control(&model.mode, on_mode),
        _ => inert_mode_control(&model.mode),
    };
    let invert_ink = if model.inverted {
        theme::ACCENT
    } else {
        theme::TEXT_TERTIARY
    };
    let content = row![
        grip,
        container(kind)
            .center_x(Length::Fixed(theme::KIND_ICON_WIDTH))
            .center_y(Length::Shrink),
        name(&model.name, model.selected, enabled),
        mode,
        small_button(
            Icon::Invert,
            theme::SMALL_ICON_SIZE,
            invert_ink,
            &model.invert_tooltip,
            false,
            enabled,
            messages.on_invert,
        ),
        menu_button(
            &model.menu_tooltip,
            model.menu_open,
            enabled,
            messages.on_menu
        ),
    ]
    .spacing(theme::COMPONENT_ROW_SPACING)
    .align_y(Alignment::Center);
    let control = row_button(
        content.into(),
        theme::COMPONENT_ROW_PADDING,
        model.selected.then_some(theme::LIST_ROW_CURRENT),
        model.hovered,
        messages.on_select.filter(|_| enabled),
    );
    let mut area = mouse_area(control);
    if let Some(enter) = messages.on_hover_enter {
        area = area.on_enter(enter);
    }
    if let Some(exit) = messages.on_hover_exit {
        area = area.on_exit(exit);
    }
    area.into()
}

/// Plain data for one brush stroke under a brush component: its number, its caption
/// (`add · 0.06 · f50`) and its delete button, disabled with the reason as its tooltip.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeRowModel {
    pub index: String,
    pub label: String,
    pub delete_tooltip: String,
    pub delete_enabled: bool,
}

/// Renders one stroke row, inset under its component row by [`theme::COMPONENT_DETAIL_INDENT`].
pub fn stroke_row<'a, M: Clone + 'a>(
    model: &StrokeRowModel,
    on_delete: Option<M>,
) -> Element<'a, M> {
    let content = row![
        text(model.index.clone())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_TERTIARY)
            .wrapping(Wrapping::None)
            .align_x(Horizontal::Right)
            .width(Length::Fixed(theme::STROKE_INDEX_WIDTH)),
        container(truncated_text(
            model.label.clone(),
            theme::SIZE_CAPTION,
            theme::FONT,
            theme::TEXT_LABEL,
        ))
        .width(Length::Fill),
        small_button(
            Icon::Trash,
            theme::STROKE_ICON_SIZE,
            if model.delete_enabled {
                theme::TEXT_SECONDARY
            } else {
                theme::TEXT_FAINT
            },
            &model.delete_tooltip,
            false,
            model.delete_enabled,
            on_delete,
        ),
    ]
    .spacing(theme::STROKE_ROW_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fixed(theme::STROKE_ROW_HEIGHT));
    container(content)
        .padding(Padding::default().left(theme::COMPONENT_DETAIL_INDENT))
        .width(Length::Fill)
        .into()
}

/// A kind's own line under its component row (what a range does not select, a model's
/// provenance, a refusal), in secondary ink at the small caption size, inset past the grip.
pub fn component_note<'a, M: 'a>(content: impl Into<String>) -> Element<'a, M> {
    container(
        text(content.into())
            .size(theme::SIZE_SMALL_CAPTION)
            .color(theme::TEXT_SECONDARY),
    )
    .padding(theme::COMPONENT_NOTE_PADDING)
    .width(Length::Fill)
    .into()
}

/// A row's name: it takes what the row's other parts leave and ends in an ellipsis first.
fn name<'a, M: 'a>(name: &str, selected: bool, enabled: bool) -> Element<'a, M> {
    let ink = match (enabled, selected) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => theme::TEXT_CURRENT_ROW,
        (true, false) => theme::TEXT_LABEL,
    };
    container(truncated_text(
        name.to_owned(),
        theme::SIZE_CONTROL,
        theme::FONT,
        ink,
    ))
    .width(Length::Fill)
    .into()
}

/// `content`, publishing `on_press` when pressed and showing the grab cursor, or `content` alone.
fn drag_handle<'a, M: Clone + 'a>(content: Element<'a, M>, on_press: Option<M>) -> Element<'a, M> {
    match on_press {
        Some(message) => mouse_area(content)
            .on_press(message)
            .interaction(iced::mouse::Interaction::Grab)
            .into(),
        None => content,
    }
}

/// A row's small icon button (`.ib.sm`): a [`theme::HEADER_BUTTON_SIZE`] square.
fn small_button<'a, M: Clone + 'a>(
    glyph: Icon,
    size: f32,
    ink: Color,
    tooltip: &str,
    selected: bool,
    enabled: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    sized_icon_button(
        &IconButtonModel {
            icon: glyph,
            tooltip: tooltip.to_owned(),
            enabled,
            selected,
        },
        on_press,
        theme::HEADER_BUTTON_SIZE,
        size,
        ink,
        tooltip::Position::Top,
    )
}

/// A row's menu button: the three dots, selected while its menu is open.
fn menu_button<'a, M: Clone + 'a>(
    tooltip: &str,
    open: bool,
    enabled: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (enabled, open) {
        (false, _) => theme::TEXT_FAINT,
        (true, true) => theme::ACCENT,
        (true, false) => theme::TEXT_SECONDARY,
    };
    small_button(
        Icon::More,
        theme::HEADER_ICON_SIZE,
        ink,
        tooltip,
        open,
        enabled,
        on_press,
    )
}

/// The row itself: one button across the panel, tinted with `selected` when given.
fn row_button<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    padding: Padding,
    selected: Option<Color>,
    hovered: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(container(content).center_y(Length::Fill))
        .padding(padding)
        .width(Length::Fill)
        .height(Length::Fixed(theme::MASK_ROW_HEIGHT))
        .style(theme::mask_row(selected, hovered))
        .on_press_maybe(on_press)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(selected: bool, visible: bool, active: bool) -> MaskRowModel {
        MaskRowModel {
            name: "Foreground with a long name".into(),
            coverage: CoverageThumbnailModel::default(),
            active,
            amount: "100".into(),
            visible,
            visibility_tooltip: "Hide overlay".into(),
            menu_tooltip: "Mask actions".into(),
            selected,
            menu_open: false,
            enabled: true,
        }
    }

    #[test]
    fn every_mask_row_state_builds() {
        for (selected, visible, active) in [(false, true, true), (true, false, false)] {
            let _: Element<'_, u8> = mask_row(
                &mask(selected, visible, active),
                MaskRowMessages {
                    on_select: Some(0),
                    on_toggle_visibility: Some(1),
                    on_menu: Some(2),
                    on_drag_start: Some(3),
                },
            );
        }
        let _: Element<'_, u8> = mask_row(
            &MaskRowModel {
                enabled: false,
                menu_open: true,
                ..mask(false, true, true)
            },
            MaskRowMessages {
                on_select: None,
                on_toggle_visibility: None,
                on_menu: None,
                on_drag_start: None,
            },
        );
    }

    #[test]
    fn every_component_row_state_builds() {
        let model = ComponentRowModel {
            name: "Radial 1".into(),
            icon: Some(Icon::Radial),
            mode: ModeControlModel {
                selected: CombineMode::Add,
                fixed: Some("A mask's first component is always Add".into()),
                tooltip: String::new(),
                enabled: true,
            },
            inverted: true,
            invert_tooltip: "Invert".into(),
            menu_tooltip: "Component actions".into(),
            selected: true,
            hovered: false,
            menu_open: false,
            enabled: true,
        };
        let _: Element<'_, u8> = component_row(
            &model,
            ComponentRowMessages {
                on_select: Some(0),
                on_mode: Some(Box::new(|_| 1)),
                on_invert: Some(2),
                on_menu: Some(3),
                on_drag_start: Some(4),
                on_hover_enter: Some(5),
                on_hover_exit: Some(6),
            },
        );
        let _: Element<'_, u8> = component_row(
            &ComponentRowModel {
                icon: None,
                hovered: true,
                selected: false,
                enabled: false,
                ..model
            },
            ComponentRowMessages::default(),
        );
    }

    #[test]
    fn stroke_rows_and_notes_build() {
        for delete_enabled in [true, false] {
            let _: Element<'_, ()> = stroke_row(
                &StrokeRowModel {
                    index: "1".into(),
                    label: "add \u{b7} 0.06 \u{b7} f50".into(),
                    delete_tooltip: "Delete stroke".into(),
                    delete_enabled,
                },
                Some(()),
            );
        }
        let _: Element<'_, ()> = component_note("Model selection");
    }
}
