//! A square control-surface button for a short glyph, with a hover tooltip.

use crate::theme;
use iced::widget::{button, container, text, tooltip};
use iced::{Element, Length};

/// The side length of an icon button, on the 8 pt spacing grid.
const SIZE: f32 = 28.0;

/// Plain data for one icon button.
#[derive(Debug, Clone, PartialEq)]
pub struct IconButtonModel {
    /// The glyph shown on the button, e.g. an arrow or a short symbol.
    pub glyph: String,
    /// The tooltip text shown on hover.
    pub tooltip: String,
    pub enabled: bool,
    /// Whether this button represents the active choice, tinted with the accent.
    pub selected: bool,
}

/// Renders one square icon-glyph button with a hover tooltip. `on_press` is `None` when the
/// button has nothing to do; combined with `enabled: false` the button is also visually dimmed.
pub fn icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::button_selected
    } else {
        theme::button_icon
    };

    let glyph_color = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };

    let control = button(
        container(
            text(model.glyph.clone())
                .size(theme::SIZE_CONTROL)
                .color(glyph_color),
        )
        .center(Length::Fill),
    )
    .width(Length::Fixed(SIZE))
    .height(Length::Fixed(SIZE))
    .style(style)
    .on_press_maybe(if model.enabled { on_press } else { None });

    tooltip(
        control,
        container(
            text(model.tooltip.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(6.0)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}
