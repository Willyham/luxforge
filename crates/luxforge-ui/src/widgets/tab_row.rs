//! A segmented tab row that stands in for a module's stacked group headers when its groups are
//! parallel views of the same controls (the colour mixer's Hue, Saturation and Luminance).
//!
//! One equal-width tab per group on a [`theme::TAB_ROW_HEIGHT`] track, the selected tab raised on
//! an inset pill, an accent dot on every tab whose group is Custom so a hidden group's edits stay
//! visible, and the visible group's reset at the row's right. Which tab is selected is the
//! caller's view state; the widget only draws it.

use super::focus_control::{ControlKey, ControlKeyEvent, focus_control};
use super::icon_button::{Icon, IconButtonModel, header_icon_button};
use super::section_header::accent_dot;
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Row, button, container, row, text};
use iced::{Alignment, Border, Element, Length, Padding, Theme};

/// One tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub label: String,
    /// The tab's group differs from its defaults: an accent dot.
    pub custom: bool,
}

/// Plain data for one tab row.
#[derive(Debug, Clone, PartialEq)]
pub struct TabRowModel {
    pub tabs: Vec<Tab>,
    pub selected: usize,
    /// Whether the visible group offers a reset.
    pub reset: bool,
    pub enabled: bool,
}

/// The height a tab row takes in a section body, its margins included.
pub const fn tab_row_height() -> f32 {
    theme::TAB_ROW_MARGIN + theme::TAB_ROW_HEIGHT + theme::TAB_ROW_MARGIN
}

/// Renders one tab row. `on_select` receives the chosen tab's index; `on_reset` resets the
/// visible group.
pub fn tab_row<'a, M: Clone + 'a>(
    model: &TabRowModel,
    on_select: impl Fn(usize) -> M + 'a,
    on_reset: M,
) -> Element<'a, M> {
    let mut tabs = Row::new().spacing(0.0).height(Length::Fill);
    for (index, tab) in model.tabs.iter().enumerate() {
        let selected = index == model.selected;
        let mut label = row![
            // Semibold, as colour-mixer.png draws every tab: its stems are a semibold's width.
            text(tab.label.clone())
                .size(theme::SIZE_CONTROL)
                .font(theme::FONT_SEMIBOLD)
                .wrapping(Wrapping::None)
                .color(if selected {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_SECONDARY
                })
        ]
        .spacing(theme::MODULE_HEADER_SPACING)
        .align_y(Alignment::Center);
        if tab.custom {
            label = label.push(accent_dot());
        }
        let press = model.enabled.then(|| on_select(index));
        let key = press.clone();
        let control = button(container(label).center(Length::Fill))
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(if selected {
                tab_selected
            } else {
                theme::button_bare
            })
            .on_press_maybe(press);
        tabs = tabs.push(focus_control(control.into(), key.is_some(), move |event| {
            matches!(
                event,
                ControlKeyEvent::Pressed {
                    key: ControlKey::Space | ControlKey::Enter,
                    ..
                }
            )
            .then(|| key.clone())
            .flatten()
        }));
    }
    let track = container(tabs)
        .padding(theme::TAB_INSET)
        .width(Length::Fill)
        .height(Length::Fixed(theme::TAB_ROW_HEIGHT))
        .style(|_theme: &Theme| {
            container::Style::default()
                .background(theme::TAB_TRACK)
                .border(Border {
                    radius: theme::RADIUS.into(),
                    width: 0.0,
                    color: iced::Color::TRANSPARENT,
                })
        });
    let mut content = row![track]
        .spacing(theme::GROUP_RESET_SPACING)
        .align_y(Alignment::Center);
    if model.reset {
        content = content.push(header_icon_button(
            &IconButtonModel {
                icon: Icon::Reset,
                tooltip: "Reset".into(),
                enabled: model.enabled,
                selected: false,
            },
            Some(on_reset),
        ));
    }
    container(content)
        .padding(Padding {
            top: theme::TAB_ROW_MARGIN,
            right: 0.0,
            bottom: theme::TAB_ROW_MARGIN,
            left: 0.0,
        })
        .width(Length::Fill)
        .into()
}

fn tab_selected(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(theme::TAB_SELECTED)),
        text_color: theme::TEXT_PRIMARY,
        border: Border {
            radius: (theme::RADIUS - theme::TAB_INSET).into(),
            width: 0.0,
            color: iced::Color::TRANSPARENT,
        },
        shadow: iced::Shadow::default(),
        snap: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_row_is_its_track_and_margins() {
        assert_eq!(tab_row_height(), 32.0);
    }

    #[test]
    fn every_selection_builds() {
        let model = |selected| TabRowModel {
            tabs: ["Hue", "Saturation", "Luminance"]
                .iter()
                .enumerate()
                .map(|(index, label)| Tab {
                    label: (*label).into(),
                    custom: index < 2,
                })
                .collect(),
            selected,
            reset: true,
            enabled: true,
        };
        for selected in 0..3 {
            let _: Element<'_, usize> = tab_row(&model(selected), |index| index, 9);
        }
    }
}
