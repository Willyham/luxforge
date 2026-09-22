//! A plain-data boolean control.

use crate::theme;
use iced::widget::{checkbox, row, text};
use iced::{Alignment, Element, Length};

#[derive(Debug, Clone, PartialEq)]
pub struct ToggleModel {
    pub label: String,
    pub on: bool,
    pub enabled: bool,
}

pub fn toggle<'a, M: Clone + 'a>(
    model: &ToggleModel,
    on_toggle: impl Fn(bool) -> M + 'a,
) -> Element<'a, M> {
    let checkbox = checkbox(model.on).on_toggle_maybe(model.enabled.then_some(on_toggle));
    row![
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill),
        checkbox
    ]
    .align_y(Alignment::Center)
    .into()
}
