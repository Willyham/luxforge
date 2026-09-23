//! A module section: its band (the header row on the Bar surface) and, when expanded, its body.
//!
//! The band is the module level of the tools panel's hierarchy: a 1 px border above, then a row of
//! [`theme::MODULE_HEADER_HEIGHT`] on the Bar surface carrying the disclosure, the title, the
//! accent dot for a non-neutral module, and either the hint (collapsed) or the module reset
//! (expanded). It is the same height expanded or collapsed, so the panel does not jump. An
//! unavailable module's band shows its reason in the clipping red and does not expand.

use super::icon_button::{Icon, IconButtonModel, header_icon_button, icon};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Theme};

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
    /// Whether a reset action is offered. It is shown while the section is expanded.
    pub reset: bool,
    /// A short word for the section's own state while expanded, drawn in the accent before the
    /// reset, such as Draft while its canvas draft is open.
    pub status: Option<String>,
    pub enabled: bool,
}

/// The band's total height including the border above it: what a collapsed section occupies.
pub const fn collapsed_section_height() -> f32 {
    theme::BORDER_WIDTH + theme::MODULE_HEADER_HEIGHT
}

/// The height of an expanded section whose body rows are `rows` tall in total, `count` of them.
pub fn expanded_section_height(rows: f32, count: usize) -> f32 {
    collapsed_section_height()
        + theme::SECTION_PADDING.top
        + rows
        + theme::ROW_SPACING * count.saturating_sub(1) as f32
        + theme::SECTION_PADDING.bottom
}

/// Renders one section header: the border above and the band.
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
    let title_color = if model.unavailable.is_some() {
        theme::TEXT_SECONDARY
    } else {
        theme::TEXT_PRIMARY
    };

    let mut leading = row![
        icon(chevron, theme::DISCLOSURE_SIZE, theme::TEXT_SECONDARY),
        text(model.title.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .color(title_color),
    ]
    .spacing(theme::MODULE_HEADER_SPACING)
    .align_y(Alignment::Center);

    if model.active {
        leading = leading.push(accent_dot());
    }

    // The chevron, the title and the dot keep their own width; the hint or the unavailable reason
    // takes whatever is left on one line, ending in an ellipsis when it does not fit, so a long
    // hint can never squeeze the title out or wrap out of the band.
    let mut header = row![container(leading).width(Length::Shrink)]
        .spacing(theme::MODULE_HEADER_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);

    let one_line = |content: &String, color| {
        container(
            truncated_text(content.clone(), theme::SIZE_CAPTION, theme::FONT, color)
                .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into())),
        )
        .width(Length::Fill)
        .align_x(Horizontal::Right)
    };
    let trailing: Element<'a, M> = if let Some(reason) = &model.unavailable {
        one_line(reason, theme::CLIPPING_HIGHLIGHT).into()
    } else if !model.expanded
        && let Some(hint) = &model.hint
    {
        one_line(hint, theme::TEXT_TERTIARY).into()
    } else {
        Space::new().width(Length::Fill).into()
    };
    header = header.push(trailing);

    if model.expanded
        && model.unavailable.is_none()
        && let Some(status) = &model.status
    {
        header = header.push(
            text(status.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .color(theme::ACCENT),
        );
    }

    if model.reset && model.expanded && model.unavailable.is_none() {
        header = header.push(header_icon_button(
            &IconButtonModel {
                icon: Icon::Reset,
                tooltip: "Reset".to_string(),
                enabled: model.enabled,
                selected: false,
            },
            Some(on_reset),
        ));
    }

    let band = button(header)
        .padding(Padding {
            top: 0.0,
            right: theme::MODULE_HEADER_PADDING_RIGHT,
            bottom: 0.0,
            left: theme::MODULE_HEADER_PADDING_LEFT,
        })
        .width(Length::Fill)
        .height(Length::Fixed(theme::MODULE_HEADER_HEIGHT))
        .style(theme::button_band)
        .on_press_maybe(can_expand.then_some(on_toggle));

    column![hairline(theme::band_border_surface), band]
        .width(Length::Fill)
        .into()
}

/// Renders a whole module section: its band and, when `body` is given, the body under it with the
/// section padding and the row spacing. The caller passes `None` for a collapsed or unavailable
/// section.
pub fn module_section<'a, M: Clone + 'a>(
    model: &SectionHeaderModel,
    on_toggle: M,
    on_reset: M,
    body: Option<Vec<Element<'a, M>>>,
) -> Element<'a, M> {
    let header = section_header(model, on_toggle, on_reset);
    match body {
        Some(rows) if model.unavailable.is_none() => column![header, section_body(rows)].into(),
        _ => header,
    }
}

/// A section body: rows flush at the section padding, separated only by the row spacing.
pub fn section_body<'a, M: 'a>(rows: Vec<Element<'a, M>>) -> Element<'a, M> {
    Column::with_children(rows)
        .spacing(theme::ROW_SPACING)
        .padding(theme::SECTION_PADDING)
        .width(Length::Fill)
        .into()
}

/// A full-width 1 px line in the given surface style.
pub(crate) fn hairline<'a, M: 'a>(style: fn(&Theme) -> container::Style) -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(style)
        .into()
}

/// A small filled circle in the accent colour, marking a non-neutral module.
pub(crate) fn accent_dot<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(theme::DOT_SIZE))
        .height(Length::Fixed(theme::DOT_SIZE))
        .style(|_theme: &Theme| {
            container::Style::default()
                .background(theme::ACCENT)
                .border(Border {
                    radius: (theme::DOT_SIZE / 2.0).into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                })
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Module panels design's resulting heights at 300 pt: a collapsed section is 33 pt, and
    /// an expanded section is its band plus padding, its rows and the gaps between them.
    #[test]
    fn section_heights_follow_the_density() {
        assert_eq!(collapsed_section_height(), 33.0);
        // Presence: one group header (with its margin) and three sliders.
        let group = theme::GROUP_MARGIN + theme::GROUP_HEADER_HEIGHT;
        let presence = expanded_section_height(group + 3.0 * theme::SLIDER_ROW_HEIGHT, 4);
        // Vignette: one group header and four sliders.
        let vignette = expanded_section_height(group + 4.0 * theme::SLIDER_ROW_HEIGHT, 5);
        assert_eq!(presence, 165.0);
        assert_eq!(vignette, 195.0);
        // Basic: three group headers, ten sliders and the picker's button row.
        let basic = expanded_section_height(
            3.0 * group
                + 10.0 * theme::SLIDER_ROW_HEIGHT
                + super::super::button_row_height(
                    super::super::ButtonSize::Compact,
                    super::super::RowPlacement {
                        after_header: false,
                        followed: true,
                    },
                ),
            14,
        );
        assert_eq!(basic, 465.0);
        // Transforms: one group header and its icon row straight under it.
        let transforms = expanded_section_height(
            group
                + super::super::button_row_height(
                    super::super::ButtonSize::Regular,
                    super::super::RowPlacement {
                        after_header: true,
                        followed: false,
                    },
                ),
            2,
        );
        assert_eq!(transforms, 105.0);
        // RAW: one group header, three sliders and the picker row ending the section.
        let raw = expanded_section_height(
            group
                + 3.0 * theme::SLIDER_ROW_HEIGHT
                + super::super::button_row_height(
                    super::super::ButtonSize::Compact,
                    super::super::RowPlacement::default(),
                ),
            5,
        );
        assert_eq!(raw, 193.0);
    }
}
