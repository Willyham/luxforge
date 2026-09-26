//! A floating bar surface holding arbitrary children, and the draft bar built on it.

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, container, text, tooltip};
use iced::{Alignment, Element, Length, Theme};

/// Renders a floating, bordered bar holding `children` laid out in a row, in order: the canvas
/// chrome's surface, [`theme::DRAFT_BAR_HEIGHT`] tall.
pub fn floating_bar<'a, M: Clone + 'a>(children: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::DRAFT_BAR_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);

    for child in children {
        content = content.push(child);
    }

    container(content)
        .padding(theme::DRAFT_BAR_PADDING)
        .height(Length::Fixed(theme::DRAFT_BAR_HEIGHT))
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::CHROME_RADIUS))
        .into()
}

/// Plain data for the draft bar a canvas mode shows while its draft is open.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftBarModel {
    /// The mode's name, in the accent.
    pub title: String,
    /// One line of the draft's own numbers.
    pub readout: String,
    /// Why Apply is refused, stated on hover; `None` while Apply is enabled.
    pub apply_reason: Option<String>,
}

/// The draft bar: the mode's name in the accent, the readout, a rule, then Cancel with its `esc`
/// hint and Apply as the primary button with its return hint. Apply is disabled when `on_apply` is
/// `None`, and says why on hover when the model gives a reason.
pub fn draft_bar<'a, M: Clone + 'a>(
    model: &DraftBarModel,
    on_cancel: M,
    on_apply: Option<M>,
) -> Element<'a, M> {
    let title = text(model.title.clone())
        .size(theme::SIZE_CONTROL)
        .font(theme::FONT_SEMIBOLD)
        .color(theme::ACCENT);
    let readout = text(model.readout.clone())
        .size(theme::SIZE_CONTROL)
        .wrapping(Wrapping::None)
        .color(theme::TEXT_LABEL);
    let rule = container(Space::new())
        .width(Length::Fixed(theme::BORDER_WIDTH))
        .height(Length::Fixed(theme::STRIP_RULE_HEIGHT))
        .style(|_: &Theme| container::Style::default().background(theme::STRIP_RULE));
    let button = |label: &str, key: &str, tone: ButtonTone, enabled: bool| LabelledButtonModel {
        label: label.to_owned(),
        icon: None,
        key_hint: Some(key.to_owned()),
        tone,
        size: ButtonSize::Regular,
        fill: false,
        enabled,
    };
    let cancel = labelled_button(
        &button("Cancel", "esc", ButtonTone::Control, true),
        Some(on_cancel),
    );
    let enabled = on_apply.is_some();
    let apply = labelled_button(
        &button("Apply", "return", ButtonTone::Primary, enabled),
        on_apply,
    );
    let apply = match &model.apply_reason {
        // A refused Apply says why on hover instead of going quiet.
        Some(reason) => tooltip(
            apply,
            container(
                text(reason.clone())
                    .size(theme::SIZE_CAPTION)
                    .color(theme::TEXT_PRIMARY),
            )
            .padding(theme::TOOLTIP_PADDING)
            .style(theme::bar_surface),
            tooltip::Position::Bottom,
        )
        .into(),
        None => apply,
    };
    floating_bar(vec![
        title.into(),
        readout.into(),
        rule.into(),
        cancel,
        apply,
    ])
}
