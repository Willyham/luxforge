//! A sub-group label within a module (White balance, Tone, Color) with an optional reset.

use super::icon_button::{Icon, IconButtonModel, icon_button};
use super::text::{caption, section_label};
use crate::theme;
use iced::widget::{container, row};
use iced::{Alignment, Element, Length};

/// Plain data for one sub-group header.
#[derive(Debug, Clone, PartialEq)]
pub struct SubGroupHeaderModel {
    pub label: String,
    /// A short word for the group's own state, such as whether it is still at its defaults. The
    /// widget only draws it: what it says is decided above.
    pub state: Option<String>,
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

    if let Some(state) = &model.state {
        header = header.push(caption(state.clone()));
    }

    if model.reset {
        header = header.push(icon_button(
            &IconButtonModel {
                icon: Icon::Reset,
                tooltip: "Reset".to_string(),
                enabled: model.enabled,
                selected: false,
            },
            Some(on_reset),
        ));
    }

    header.into()
}
