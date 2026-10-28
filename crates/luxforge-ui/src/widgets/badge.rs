//! A small outlined tag beside a row's label, with the detail it stands for in a tooltip.

use crate::theme;
use crate::{Element, Theme, Token};
use iced::Border;
use iced::widget::{container, text, tooltip};

/// Plain data for one badge: a short word and, optionally, what it summarizes.
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeModel {
    pub label: String,
    /// Shown on hover, e.g. the counts a "Partial" badge stands for.
    pub tooltip: Option<String>,
}

/// What a badge says by its colour: the grey of an origin or a fact, the accent of a change that
/// waits for something, or the error red of a failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeTone {
    #[default]
    Neutral,
    Accent,
    Error,
}

/// Renders one badge. It publishes nothing: a badge describes the row it sits in.
pub fn badge<'a, M: 'a>(model: &BadgeModel) -> Element<'a, M> {
    toned_badge(model, BadgeTone::Neutral)
}

/// [`badge`] with its label in `tone`'s ink: a state badge such as *Relaunch to apply* (accent) or
/// *Didn't load* (error), beside the grey origin badge.
pub fn toned_badge<'a, M: 'a>(model: &BadgeModel, tone: BadgeTone) -> Element<'a, M> {
    let ink = match tone {
        BadgeTone::Neutral => Token::TextSecondary,
        BadgeTone::Accent => Token::Accent,
        BadgeTone::Error => Token::Error,
    };
    let tag = container(
        text(model.label.clone())
            .size(theme::SIZE_CAPTION)
            .wrapping(iced::widget::text::Wrapping::None)
            .style(theme::ink(ink)),
    )
    .padding([1.0, 6.0])
    .style(|theme: &Theme| {
        container::Style::default()
            .background(theme.palette().control)
            .border(Border {
                radius: theme::RADIUS.into(),
                width: theme::BORDER_WIDTH,
                color: theme.palette().border,
            })
    });
    match &model.tooltip {
        Some(detail) => tooltip(
            tag,
            container(
                text(detail.clone())
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(Token::Text)),
            )
            .padding(6.0)
            .style(theme::bar_surface),
            tooltip::Position::Top,
        )
        .into(),
        None => tag.into(),
    }
}
