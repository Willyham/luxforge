//! A small outlined tag beside a row's label, with the detail it stands for in a tooltip.

use crate::theme;
use iced::widget::{container, text, tooltip};
use iced::{Border, Element, Theme};

/// Plain data for one badge: a short word and, optionally, what it summarizes.
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeModel {
    pub label: String,
    /// Shown on hover, e.g. the counts a "Partial" badge stands for.
    pub tooltip: Option<String>,
}

/// Renders one badge. It publishes nothing: a badge describes the row it sits in.
pub fn badge<'a, M: 'a>(model: &BadgeModel) -> Element<'a, M> {
    let tag = container(
        text(model.label.clone())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_SECONDARY),
    )
    .padding([1.0, 6.0])
    .style(|_theme: &Theme| {
        container::Style::default()
            .background(theme::CONTROL)
            .border(Border {
                radius: theme::RADIUS.into(),
                width: theme::BORDER_WIDTH,
                color: theme::BORDER,
            })
    });
    match &model.tooltip {
        Some(detail) => tooltip(
            tag,
            container(
                text(detail.clone())
                    .size(theme::SIZE_CAPTION)
                    .color(theme::TEXT_PRIMARY),
            )
            .padding(6.0)
            .style(theme::bar_surface),
            tooltip::Position::Top,
        )
        .into(),
        None => tag.into(),
    }
}
