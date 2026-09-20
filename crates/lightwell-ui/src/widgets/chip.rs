//! A rounded, selectable label chip (versions, ratio presets, filters).

use super::text::caption;
use crate::theme;
use iced::widget::{button, mouse_area, row, text};
use iced::{Alignment, Element};

/// Plain data for one chip.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipModel {
    pub label: String,
    /// A short trailing caption, e.g. an entry number.
    pub trailing: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}

/// Renders one chip. `on_context` fires on a right-click, e.g. to open a context menu.
pub fn chip<'a, M: Clone + 'a>(
    model: &ChipModel,
    on_press: Option<M>,
    on_context: Option<M>,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::button_selected
    } else {
        theme::button_plain
    };

    let mut content = row![text(model.label.clone()).size(theme::SIZE_CONTROL)]
        .spacing(4.0)
        .align_y(Alignment::Center);

    if let Some(trailing) = &model.trailing {
        content = content.push(caption(trailing.clone()));
    }

    let control = button(content)
        .padding([4.0, 10.0])
        .style(style)
        .on_press_maybe(model.enabled.then_some(on_press).flatten());

    let area = mouse_area(control);

    match on_context.filter(|_| model.enabled) {
        Some(message) => area.on_right_press(message).into(),
        None => area.into(),
    }
}
