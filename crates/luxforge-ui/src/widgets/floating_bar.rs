//! A floating bar surface holding arbitrary children: the draft bar and the mode strip container.

use crate::theme;
use iced::widget::{Row, container};
use iced::{Alignment, Element};

/// Renders a floating, bordered bar holding `children` laid out in a row, in order.
pub fn floating_bar<'a, M: Clone + 'a>(children: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);

    for child in children {
        content = content.push(child);
    }

    container(content)
        .padding(theme::SPACING / 2.0)
        .style(theme::bar_surface)
        .into()
}
