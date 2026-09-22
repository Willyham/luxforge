//! A themed menu choice that reports the selected option index.

use crate::theme;
use iced::widget::{container, pick_list, row, text};
use iced::{Alignment, Element, Length};

#[derive(Debug, Clone, PartialEq)]
pub struct MenuChoiceModel {
    pub label: String,
    pub options: Vec<String>,
    pub selected: usize,
    pub enabled: bool,
}

pub fn menu_choice<'a, M: Clone + 'a>(
    model: &MenuChoiceModel,
    on_select: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let selected = model.options.get(model.selected).cloned();
    let options = model.options.clone();
    if !model.enabled {
        return row![
            text(model.label.clone())
                .size(theme::SIZE_CONTROL)
                .width(Length::Fill),
            container(
                text(selected.unwrap_or_default())
                    .size(theme::SIZE_CONTROL)
                    .color(theme::TEXT_TERTIARY)
            )
            .padding(6.0)
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center)
        .into();
    }
    let menu = pick_list(options.clone(), selected, move |option| {
        let index = options
            .iter()
            .position(|candidate| candidate == &option)
            .unwrap_or(0);
        on_select(index)
    });
    row![
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill),
        menu
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .into()
}
