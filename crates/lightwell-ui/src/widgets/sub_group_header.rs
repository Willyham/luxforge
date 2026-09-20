//! A sub-group label within a module (White balance, Tone, Color) with an optional reset.

use super::icon_button::{IconButtonModel, icon_button};
use super::text::section_label;
use crate::theme;
use iced::widget::{container, row};
use iced::{Alignment, Element, Length};

/// Plain data for one sub-group header.
#[derive(Debug, Clone, PartialEq)]
pub struct SubGroupHeaderModel {
    pub label: String,
    pub reset: bool,
    pub enabled: bool,
}

/// Renders one sub-group header row.
pub fn sub_group_header<'a, M: Clone + 'a>(
    model: &SubGroupHeaderModel,
    on_reset: M,
) -> Element<'a, M> {
    let mut header = row![container(section_label(model.label.clone())).width(Length::Fill)]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);

    if model.reset {
        header = header.push(icon_button(
            &IconButtonModel {
                glyph: "\u{21ba}".to_string(),
                tooltip: "Reset".to_string(),
                enabled: model.enabled,
                selected: false,
            },
            Some(on_reset),
        ));
    }

    header.into()
}
