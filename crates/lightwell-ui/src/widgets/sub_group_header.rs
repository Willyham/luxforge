//! A sub-group header within a module (White balance, Tone, Colour): the group level of the tools
//! panel's hierarchy.
//!
//! A row of [`theme::GROUP_HEADER_HEIGHT`] with [`theme::GROUP_MARGIN`] above: the disclosure and
//! the label in 11 pt semibold secondary text, then a hairline rule running to the state caption
//! (Original, or Custom in the accent) and the group's reset. A collapsed group keeps its header
//! and drops the rule. The disclosure and the reset are each focusable: Space or Enter on the
//! disclosure toggles the group, and on the reset resets it.

use super::focus_control::{ControlKey, ControlKeyEvent, focus_control};
use super::icon_button::{Icon, IconButtonModel, header_icon_button, icon};
use super::section_header::hairline;
use super::text::group_label;
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Space, button, container, row, text};
use iced::{Alignment, Element, Length, Padding};

/// Plain data for one sub-group header.
#[derive(Debug, Clone, PartialEq)]
pub struct SubGroupHeaderModel {
    pub label: String,
    /// A short word for the group's own state, such as whether it is still at its defaults. The
    /// widget only draws it: what it says is decided above.
    pub state: Option<String>,
    /// Draw the state in the accent, as a group that differs from its defaults does.
    pub state_accent: bool,
    /// Whether the group is expanded, when it can be collapsed; `None` draws no disclosure.
    pub expanded: Option<bool>,
    pub reset: bool,
    pub enabled: bool,
}

/// The height a group header takes in a section body, its margin included.
pub const fn sub_group_header_height() -> f32 {
    theme::GROUP_MARGIN + theme::GROUP_HEADER_HEIGHT
}

/// Renders one sub-group header row. `on_toggle` is published by the disclosure.
pub fn sub_group_header<'a, M: Clone + 'a>(
    model: &SubGroupHeaderModel,
    on_toggle: Option<M>,
    on_reset: M,
) -> Element<'a, M> {
    let expanded = model.expanded.unwrap_or(true);
    let leading: Element<'a, M> = match model.expanded {
        Some(open) => {
            let chevron = if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let content = row![
                icon(chevron, theme::DISCLOSURE_SIZE, theme::TEXT_SECONDARY),
                group_label(model.label.clone())
            ]
            .spacing(theme::GROUP_HEADER_SPACING)
            .align_y(Alignment::Center);
            let toggle = on_toggle.filter(|_| model.enabled);
            let key = toggle.clone();
            let control = button(content)
                .padding(0)
                .style(theme::button_bare)
                .on_press_maybe(toggle);
            focus_control(control.into(), key.is_some(), move |event| {
                activates(event).then(|| key.clone()).flatten()
            })
        }
        None => group_label(model.label.clone()),
    };

    let mut labelled = row![leading]
        .spacing(theme::GROUP_RULE_SPACING)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    labelled = if expanded {
        labelled.push(
            container(hairline(theme::rule_surface))
                .padding(Padding::default().left(theme::GROUP_RULE_LEAD))
                .width(Length::Fill)
                .center_y(Length::Shrink),
        )
    } else {
        labelled.push(Space::new().width(Length::Fill))
    };
    if let Some(state) = &model.state {
        labelled = labelled.push(
            text(state.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .color(if model.state_accent {
                    theme::ACCENT
                } else {
                    theme::TEXT_TERTIARY
                }),
        );
    }

    let mut header = row![labelled]
        .spacing(theme::GROUP_RESET_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::GROUP_HEADER_HEIGHT));
    if model.reset {
        let reset = header_icon_button(
            &IconButtonModel {
                icon: Icon::Reset,
                tooltip: format!("Reset {}", model.label),
                enabled: model.enabled,
                selected: false,
            },
            Some(on_reset.clone()),
        );
        header = header.push(focus_control(reset, model.enabled, move |event| {
            activates(event).then(|| on_reset.clone())
        }));
    }

    container(header)
        .padding(Padding::default().top(theme::GROUP_MARGIN))
        .width(Length::Fill)
        .into()
}

fn activates(event: ControlKeyEvent) -> bool {
    matches!(
        event,
        ControlKeyEvent::Pressed {
            key: ControlKey::Space | ControlKey::Enter,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_header_is_its_row_and_margin() {
        assert_eq!(sub_group_header_height(), 28.0);
    }

    #[test]
    fn every_state_builds() {
        for expanded in [None, Some(true), Some(false)] {
            for (state, accent) in [(None, false), (Some("Custom".to_string()), true)] {
                let _: Element<'_, ()> = sub_group_header(
                    &SubGroupHeaderModel {
                        label: "White balance".into(),
                        state,
                        state_accent: accent,
                        expanded,
                        reset: true,
                        enabled: true,
                    },
                    Some(()),
                    (),
                );
            }
        }
    }
}
