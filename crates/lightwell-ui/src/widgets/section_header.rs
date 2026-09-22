//! A module section's header row: disclosure, title, active dot and an optional reset.

use super::icon_button::{Icon, IconButtonModel, icon, icon_button};
use super::text::{caption, error_caption, title};
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::{Space, button, container, row};
use iced::{Alignment, Border, Color, Element, Length, Theme};

/// Plain data for one module section header.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionHeaderModel {
    pub title: String,
    pub expanded: bool,
    /// Whether the module has a non-neutral layer in the current recipe (an accent dot).
    pub active: bool,
    /// A short hint shown in place of the controls while collapsed.
    pub hint: Option<String>,
    /// When set, the header cannot expand and this reason is shown instead of `hint`.
    pub unavailable: Option<String>,
    /// Whether a reset action is offered.
    pub reset: bool,
    pub enabled: bool,
}

/// Renders one section header row.
pub fn section_header<'a, M: Clone + 'a>(
    model: &SectionHeaderModel,
    on_toggle: M,
    on_reset: M,
) -> Element<'a, M> {
    let can_expand = model.unavailable.is_none() && model.enabled;
    let chevron = if model.expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };

    let mut leading = row![
        icon(chevron, 12.0, theme::TEXT_SECONDARY),
        title(model.title.clone())
    ]
    .spacing(4.0)
    .align_y(Alignment::Center);

    if model.active {
        leading = leading.push(accent_dot());
    }

    // The chevron and the title keep their own width; the hint or the unavailable reason takes
    // whatever is left and wraps if it must, so a long hint can never squeeze the title out.
    let mut header = row![container(leading).width(Length::Shrink)]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);

    if let Some(reason) = &model.unavailable {
        header = header.push(
            container(error_caption(reason.clone()))
                .width(Length::Fill)
                .align_x(Horizontal::Right),
        );
    } else if !model.expanded
        && let Some(hint) = &model.hint
    {
        header = header.push(
            container(caption(hint.clone()))
                .width(Length::Fill)
                .align_x(Horizontal::Right),
        );
    } else {
        header = header.push(Space::new().width(Length::Fill));
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

    button(header)
        .padding(0)
        .style(theme::button_plain)
        .on_press_maybe(can_expand.then_some(on_toggle))
        .into()
}

/// A small filled circle in the accent colour, marking a non-neutral module.
fn accent_dot<'a, M: Clone + 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(6.0))
        .height(Length::Fixed(6.0))
        .style(|_theme: &Theme| {
            container::Style::default()
                .background(theme::ACCENT)
                .border(Border {
                    radius: 3.0.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                })
        })
        .into()
}
