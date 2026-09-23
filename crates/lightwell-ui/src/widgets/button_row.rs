//! Labelled buttons in a module section, and the rows that hold them.
//!
//! A picker (Basic's Neutral picker, RAW's Neutral WB) and an action (As shot, Crop) are the same
//! button: an optional icon, the label, and an optional key hint in the tertiary colour, on the
//! Control surface at [`theme::BUTTON_HEIGHT`], or [`theme::COMPACT_BUTTON_HEIGHT`] in a row under
//! a group's sliders that holds a picker. A row sets buttons side by side with
//! [`theme::BUTTON_ROW_MARGIN`] above. A row of actions that each name an icon is drawn as an
//! [`icon_button_row`] instead: equal-width icon buttons across the whole row, their labels as
//! tooltips.

use super::icon_button::{Icon, IconButtonModel, icon};
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, Space, button, container, text, tooltip};
use iced::{Alignment, Element, Length, Padding};

/// How a labelled button reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonTone {
    /// The Control surface: an ordinary picker or action.
    #[default]
    Control,
    /// The accent fill: the one primary action on a surface, such as Apply.
    Primary,
    /// The accent tint: a picker whose mode is active.
    Selected,
}

/// How tall a labelled button stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    /// [`theme::BUTTON_HEIGHT`]: an action on a row of its own, such as Crop, Cancel or Apply.
    #[default]
    Regular,
    /// [`theme::COMPACT_BUTTON_HEIGHT`]: a row under a group's sliders that holds a picker.
    Compact,
}

impl ButtonSize {
    /// The button's height in points.
    pub const fn height(self) -> f32 {
        match self {
            Self::Regular => theme::BUTTON_HEIGHT,
            Self::Compact => theme::COMPACT_BUTTON_HEIGHT,
        }
    }

    /// The button's inset on either side of its content.
    pub const fn padding(self) -> f32 {
        match self {
            Self::Regular => theme::BUTTON_PADDING,
            Self::Compact => theme::COMPACT_BUTTON_PADDING,
        }
    }
}

/// Plain data for one labelled button.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelledButtonModel {
    pub label: String,
    pub icon: Option<Icon>,
    /// The key that does the same thing, such as `W` for Basic's neutral picker.
    pub key_hint: Option<String>,
    pub tone: ButtonTone,
    pub size: ButtonSize,
    /// Stretch across the share of the row it is given, the label at the left and the key hint at
    /// the right, as two buttons that share a row equally (Cancel and Apply) do. Otherwise the
    /// button hugs its content.
    pub fill: bool,
    pub enabled: bool,
}

/// The icon a labelled button carries for a named key, drawn in place of the letter when the key
/// has a symbol of its own (Return).
fn key_glyph(key: &str) -> Option<Icon> {
    (key == "return").then_some(Icon::Return)
}

/// Renders one labelled button.
pub fn labelled_button<'a, M: Clone + 'a>(
    model: &LabelledButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (model.tone, model.enabled) {
        (_, false) => theme::TEXT_TERTIARY,
        (ButtonTone::Primary, true) => theme::CANVAS,
        (ButtonTone::Selected, true) => theme::ACCENT,
        (ButtonTone::Control, true) => theme::TEXT_PRIMARY,
    };
    let hint_ink = theme::TEXT_TERTIARY;
    let mut content = Row::new().align_y(Alignment::Center);
    if let Some(glyph) = model.icon {
        content = content.push(
            container(icon(glyph, theme::BUTTON_ICON_SIZE, ink))
                .padding(Padding::default().right(theme::BUTTON_ICON_SPACING)),
        );
    }
    content = content.push(
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(ink),
    );
    if model.fill {
        content = content.push(Space::new().width(Length::Fill));
    }
    if let Some(key) = &model.key_hint {
        let hint: Element<'a, M> = match key_glyph(key) {
            Some(glyph) => icon(glyph, theme::BUTTON_ICON_SIZE, hint_ink),
            None => text(key.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .color(hint_ink)
                .into(),
        };
        content = content
            .push(container(hint).padding(Padding::default().left(theme::BUTTON_HINT_SPACING)));
    }
    let style = match model.tone {
        ButtonTone::Control => theme::button_control,
        ButtonTone::Primary => theme::button_accent,
        ButtonTone::Selected => theme::button_selected,
    };
    let inset = model.size.padding();
    button(content.height(Length::Fill))
        .padding(Padding {
            top: 0.0,
            right: inset,
            bottom: 0.0,
            left: inset,
        })
        .width(if model.fill {
            Length::Fill
        } else {
            Length::Shrink
        })
        .height(Length::Fixed(model.size.height()))
        .style(style)
        .on_press_maybe(if model.enabled { on_press } else { None })
        .into()
}

/// Where a button row sits among a section body's rows, which sets its margins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowPlacement {
    /// The row comes straight under a group header, whose own row already leaves air under its
    /// label: [`theme::HEADER_BUTTON_ROW_MARGIN`] above rather than [`theme::BUTTON_ROW_MARGIN`].
    pub after_header: bool,
    /// Another row comes after this one in the section, which keeps [`theme::BUTTON_ROW_BOTTOM`]
    /// clear under it; the last row of a section ends on the body's own bottom padding.
    pub followed: bool,
}

impl RowPlacement {
    pub(crate) const fn padding(self) -> Padding {
        Padding {
            top: if self.after_header {
                theme::HEADER_BUTTON_ROW_MARGIN
            } else {
                theme::BUTTON_ROW_MARGIN
            },
            right: 0.0,
            bottom: if self.followed {
                theme::BUTTON_ROW_BOTTOM
            } else {
                0.0
            },
            left: 0.0,
        }
    }
}

/// The height a one-line button row takes in a section body, margins included.
pub const fn button_row_height(size: ButtonSize, placement: RowPlacement) -> f32 {
    let padding = placement.padding();
    padding.top + size.height() + padding.bottom
}

/// Sets buttons side by side as one row. Buttons that do not fit the panel's width wrap onto
/// another line at the same spacing rather than being clipped.
pub fn button_row<'a, M: 'a>(
    buttons: Vec<Element<'a, M>>,
    placement: RowPlacement,
) -> Element<'a, M> {
    container(
        Row::with_children(buttons)
            .spacing(theme::BUTTON_ROW_SPACING)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(theme::BUTTON_ROW_SPACING),
    )
    .padding(placement.padding())
    .width(Length::Fill)
    .into()
}

/// Sets buttons that share a row equally, such as a draft's Cancel and Apply, built with `fill`:
/// each takes an equal share of the row's width.
pub fn equal_button_row<'a, M: 'a>(
    buttons: Vec<Element<'a, M>>,
    placement: RowPlacement,
) -> Element<'a, M> {
    container(
        Row::with_children(buttons)
            .spacing(theme::BUTTON_ROW_SPACING)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .padding(placement.padding())
    .width(Length::Fill)
    .into()
}

/// One cell of an [`icon_button_row`]: a [`theme::ICON_SIZE`] icon centred on the Control surface,
/// [`theme::BUTTON_HEIGHT`] tall, taking an equal share of the row's width, with the action's
/// label as its tooltip.
pub fn row_icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    let style = if model.selected {
        theme::button_selected
    } else {
        theme::button_control
    };
    let control =
        button(container(icon::<M>(model.icon, theme::ICON_SIZE, ink)).center(Length::Fill))
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fixed(theme::BUTTON_HEIGHT))
            .style(style)
            .on_press_maybe(if model.enabled { on_press } else { None });
    tooltip(
        control,
        container(
            text(model.tooltip.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(theme::TOOLTIP_PADDING)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}

/// A row of actions that each name an icon: one equal-width [`row_icon_button`] per action across
/// the whole row, [`theme::BUTTON_ROW_SPACING`] apart, with [`theme::BUTTON_ROW_MARGIN`] above. The
/// cells come built, so a caller can wrap each for focus or a context menu, and keep the fill
/// width [`row_icon_button`] gives them.
pub fn icon_button_row<'a, M: 'a>(
    cells: Vec<Element<'a, M>>,
    placement: RowPlacement,
) -> Element<'a, M> {
    equal_button_row(cells, placement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_row_is_its_button_and_margins() {
        let place = |after_header, followed| RowPlacement {
            after_header,
            followed,
        };
        // Basic's picker row, with the Tone group after it: 4 above, 22, 2 below.
        assert_eq!(
            button_row_height(ButtonSize::Compact, place(false, true)),
            28.0
        );
        // RAW's picker row ends its section on the body's own bottom padding.
        assert_eq!(
            button_row_height(ButtonSize::Compact, place(false, false)),
            26.0
        );
        // The idle Crop button, alone in its body: 4 above, 26.
        assert_eq!(
            button_row_height(ButtonSize::Regular, place(false, false)),
            30.0
        );
        // The Transforms icon row, straight under its group header: 2 above, 26.
        assert_eq!(
            button_row_height(ButtonSize::Regular, place(true, false)),
            28.0
        );
        assert_eq!(ButtonSize::Compact.height(), 22.0);
        assert_eq!(ButtonSize::Regular.height(), 26.0);
        // Inset: 10 pt on a regular button (Crop, Apply pixel), 7 pt on a compact one.
        assert_eq!(ButtonSize::Regular.padding(), 10.0);
        assert_eq!(ButtonSize::Compact.padding(), 7.0);
    }

    #[test]
    fn only_return_has_a_key_glyph() {
        assert_eq!(key_glyph("return"), Some(Icon::Return));
        assert_eq!(key_glyph("esc"), None);
        assert_eq!(key_glyph("W"), None);
    }

    #[test]
    fn every_tone_builds_with_and_without_its_parts() {
        for tone in [
            ButtonTone::Control,
            ButtonTone::Primary,
            ButtonTone::Selected,
        ] {
            for (icon, key_hint) in [(None, None), (Some(Icon::Picker), Some("W".to_string()))] {
                for (size, fill) in [(ButtonSize::Compact, false), (ButtonSize::Regular, true)] {
                    let _: Element<'_, ()> = labelled_button(
                        &LabelledButtonModel {
                            label: "Neutral picker".into(),
                            icon,
                            key_hint: key_hint.clone(),
                            tone,
                            size,
                            fill,
                            enabled: true,
                        },
                        Some(()),
                    );
                }
            }
        }
        let _: Element<'_, ()> = button_row(
            vec![iced::widget::row![].into()],
            RowPlacement {
                after_header: false,
                followed: true,
            },
        );
        let cell = || {
            row_icon_button(
                &IconButtonModel {
                    icon: Icon::RotateLeft,
                    tooltip: "Rotate left".into(),
                    enabled: true,
                    selected: false,
                },
                Some(()),
            )
        };
        let _: Element<'_, ()> = icon_button_row(vec![cell(), cell()], RowPlacement::default());
    }
}
