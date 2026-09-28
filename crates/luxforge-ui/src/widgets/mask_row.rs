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
//!
//! A row being renamed draws its name as a text input in place ([`rename_input_id`] names it, so
//! the caller can focus it); Enter publishes the caller's submit message, and the caller decides
//! what Escape does.

use super::coverage_thumbnail::{CoverageThumbnailModel, coverage_thumbnail};
use super::icon_button::{Icon, IconButtonModel, icon, sized_icon_button};
use super::list_row::marker_circle;
use super::mode_control::{CombineMode, ModeControlModel, inert_mode_control, mode_control};
use super::number_field::value_input;
use super::truncated_text::truncated_text;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Id, Space, button, container, mouse_area, row, text, tooltip};
use iced::{Alignment, Color, Element, Length, Padding};

/// The identity of the one name input a row being renamed draws, so the caller can focus it as the
/// rename starts. Only one row is renamed at a time.
pub fn rename_input_id() -> Id {
    Id::new("mask-row-rename")
}

/// What a row being renamed publishes: each edit of its text, and Enter.
pub struct RenameMessages<'a, M> {
    pub on_text: Box<dyn Fn(String) -> M + 'a>,
    pub on_submit: M,
}

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
    /// The name as it is being typed while the row is renamed in place; `None` shows the name.
    pub renaming: Option<String>,
    pub enabled: bool,
}

/// What a mask row publishes. `None` leaves that part inert.
pub struct MaskRowMessages<'a, M> {
    /// A press on the row.
    pub on_select: Option<M>,
    /// A press on the eye.
    pub on_toggle_visibility: Option<M>,
    /// A press on the menu button.
    pub on_menu: Option<M>,
    /// A press on the thumbnail, the row's drag handle. `None` lets the press select the row.
    pub on_drag_start: Option<M>,
    /// The name input's messages while the row is renamed; without them the input is inert.
    pub rename: Option<RenameMessages<'a, M>>,
}

impl<M> Default for MaskRowMessages<'_, M> {
    fn default() -> Self {
        Self {
            on_select: None,
            on_toggle_visibility: None,
            on_menu: None,
            on_drag_start: None,
            rename: None,
        }
    }
}

/// Renders one mask's row: the coverage thumbnail, the name, the accent dot, the amount, the eye
/// and the menu button.
pub fn mask_row<'a, M: Clone + 'a>(
    model: &MaskRowModel,
    messages: MaskRowMessages<'a, M>,
) -> Element<'a, M> {
    let enabled = model.enabled;
    let renaming = model.renaming.is_some();
    let thumbnail = drag_handle(
        coverage_thumbnail(&model.coverage),
        messages.on_drag_start.filter(|_| enabled && !renaming),
    );
    let label = match &model.renaming {
        Some(typed) => rename_input(typed, enabled, messages.rename),
        None => name(&model.name, model.selected, enabled),
    };
    let mut content = row![thumbnail, label]
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
        messages.on_select.filter(|_| enabled && !renaming),
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
    /// The name as it is being typed while the row is renamed in place; `None` shows the name.
    pub renaming: Option<String>,
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
    /// The name input's messages while the row is renamed; without them the input is inert.
    pub rename: Option<RenameMessages<'a, M>>,
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
            rename: None,
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
    let renaming = model.renaming.is_some();
    let grip = drag_handle(
        icon(Icon::Grip, theme::GRIP_SIZE, theme::TEXT_FAINT),
        messages.on_drag_start.filter(|_| enabled && !renaming),
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
        match &model.renaming {
            Some(typed) => rename_input(typed, enabled, messages.rename),
            None => name(&model.name, model.selected, enabled),
        },
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
        messages.on_select.filter(|_| enabled && !renaming),
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

/// Where a dragged row will land relative to the row under the pointer: above it when the drag moves
/// up the list, below it when it moves down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEdge {
    Above,
    Below,
}

impl DropEdge {
    /// The edge a row at `target` shows while the row at `from` is dragged over it, or `None` for
    /// the dragged row itself.
    pub fn of(from: usize, target: usize) -> Option<Self> {
        match target.cmp(&from) {
            std::cmp::Ordering::Less => Some(Self::Above),
            std::cmp::Ordering::Greater => Some(Self::Below),
            std::cmp::Ordering::Equal => None,
        }
    }
}

/// A mask or component row while a reorder drag is in progress: the row being dragged is dimmed,
/// and the row under the pointer carries a [`theme::DROP_INDICATOR_WIDTH`] accent line at the edge
/// the dragged row will land on. Pure view: it draws over the row without changing its size, so the
/// list does not move under the pointer while it is dragged.
pub fn drop_feedback<'a, M: 'a>(
    content: Element<'a, M>,
    edge: Option<DropEdge>,
    dimmed: bool,
) -> Element<'a, M> {
    if edge.is_none() && !dimmed {
        return content;
    }
    let mut layers = iced::widget::Stack::new().push(content).width(Length::Fill);
    if dimmed {
        layers = layers.push(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(
                        Color {
                            a: theme::DRAGGED_ROW_DIM,
                            ..theme::PANEL
                        }
                        .into(),
                    ),
                    ..container::Style::default()
                }),
        );
    }
    if let Some(edge) = edge {
        let line = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::DROP_INDICATOR_WIDTH))
            .style(|_| container::Style {
                background: Some(theme::ACCENT.into()),
                ..container::Style::default()
            });
        let fill = Space::new().height(Length::Fill);
        layers = layers.push(match edge {
            DropEdge::Above => iced::widget::column![line, fill],
            DropEdge::Below => iced::widget::column![fill, line],
        });
    }
    layers.into()
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

/// A row's name as a text input, while the row is renamed in place: it takes the name's width.
fn rename_input<'a, M: Clone + 'a>(
    typed: &str,
    enabled: bool,
    messages: Option<RenameMessages<'a, M>>,
) -> Element<'a, M> {
    let input = match messages {
        Some(RenameMessages { on_text, on_submit }) => {
            value_input("Name", typed, false, enabled, on_text, on_submit)
        }
        None => iced::widget::text_input("Name", typed),
    };
    input
        .id(rename_input_id())
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(
            (theme::RENAME_INPUT_HEIGHT - 2.0 * theme::FIELD_PADDING_Y).into(),
        ))
        .padding(Padding {
            top: theme::FIELD_PADDING_Y,
            right: theme::FIELD_INSET,
            bottom: theme::FIELD_PADDING_Y,
            left: theme::FIELD_INSET,
        })
        .align_x(Horizontal::Left)
        .width(Length::Fill)
        .style(theme::field_input_style(false))
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
            renaming: None,
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
                    rename: None,
                },
            );
        }
        let _: Element<'_, u8> = mask_row(
            &MaskRowModel {
                enabled: false,
                menu_open: true,
                ..mask(false, true, true)
            },
            MaskRowMessages::default(),
        );
        for rename in [true, false] {
            let _: Element<'_, u8> = mask_row(
                &MaskRowModel {
                    renaming: Some("Face".into()),
                    ..mask(true, true, true)
                },
                MaskRowMessages {
                    rename: rename.then(|| RenameMessages {
                        on_text: Box::new(|_| 4),
                        on_submit: 5,
                    }),
                    ..MaskRowMessages::default()
                },
            );
        }
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
            renaming: None,
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
                rename: None,
            },
        );
        let _: Element<'_, u8> = component_row(
            &ComponentRowModel {
                renaming: Some("Radial".into()),
                ..model.clone()
            },
            ComponentRowMessages {
                rename: Some(RenameMessages {
                    on_text: Box::new(|_| 7),
                    on_submit: 8,
                }),
                ..ComponentRowMessages::default()
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
    fn drop_edges_follow_the_direction_of_the_drag() {
        assert_eq!(DropEdge::of(2, 0), Some(DropEdge::Above));
        assert_eq!(DropEdge::of(0, 2), Some(DropEdge::Below));
        assert_eq!(DropEdge::of(1, 1), None);
        for (edge, dimmed) in [
            (Some(DropEdge::Above), false),
            (Some(DropEdge::Below), false),
            (None, true),
            (None, false),
        ] {
            let _: Element<'_, ()> = drop_feedback(text("Sky").into(), edge, dimmed);
        }
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
