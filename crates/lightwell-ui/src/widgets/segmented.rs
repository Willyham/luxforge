//! A row of mutually exclusive labelled options (e.g. crop ratio presets).

use crate::theme;
use iced::Element;
use iced::widget::{Row, button, text};

/// Plain data for a segmented control.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentedModel {
    pub options: Vec<String>,
    pub selected: usize,
    pub enabled: bool,
}

/// Renders one segmented control: a row of mutually exclusive labelled options.
pub fn segmented<'a, M: Clone + 'a>(
    model: &SegmentedModel,
    on_select: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let mut items = Row::new().spacing(4.0);

    for (index, option) in model.options.iter().enumerate() {
        let selected = index == model.selected;
        let style = if selected {
            theme::button_selected
        } else {
            theme::button_plain
        };

        items = items.push(
            button(text(option.clone()).size(theme::SIZE_CONTROL))
                .padding([4.0, 10.0])
                .style(style)
                .on_press_maybe(model.enabled.then(|| on_select(index))),
        );
    }

    items.into()
}
