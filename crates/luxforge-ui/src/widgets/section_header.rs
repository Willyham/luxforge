//! A module section: its band (the header row on the Bar surface) and, when expanded, its body.
//!
//! The band is the module level of the tools panel's hierarchy: a 1 px border above, then a row of
//! [`theme::MODULE_HEADER_HEIGHT`] on the Bar surface carrying the disclosure, the title, the
//! accent dot for a non-neutral module, and either the hint (collapsed) or the module reset
//! (expanded). It is the same height expanded or collapsed, so the panel does not jump. An
//! unavailable module's band shows its reason in the clipping red and does not expand. A band bound
//! to a mask carries that mask's name as an accent scope chip before its reset, collapsed or
//! expanded, so a slider under it never reads as a global one.

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
    /// The name of the mask this section edits through, drawn as an accent chip after the hint and
    /// before the reset. `None` for a section bound to the global layer.
    pub scope: Option<String>,
    pub enabled: bool,
}

/// The scope chip's label size: the small caption size, semibold.
const SCOPE_SIZE: f32 = theme::SIZE_SMALL_CAPTION;
/// The scope chip's padding, vertical then horizontal.
const SCOPE_PADDING: [f32; 2] = [1.0, 6.0];
/// The scope chip's corner radius.
const SCOPE_RADIUS: f32 = 4.0;

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

    if model.unavailable.is_none()
        && let Some(scope) = &model.scope
    {
        header = header.push(scope_chip(scope));
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

/// A panel's own band drawn as a module band, as the Masks panel draws its band: the border above,
/// the disclosure, the title, the accent dot when `active`, and `hint` right-aligned whether the
/// band is expanded or not. It has no reset, no scope and no unavailable state, because it is not
/// a module; a press anywhere on it publishes `on_toggle`.
pub fn band_header<'a, M: Clone + 'a>(
    title: &str,
    active: bool,
    hint: Option<String>,
    expanded: bool,
    on_toggle: M,
) -> Element<'a, M> {
    let chevron = if expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let mut leading = row![
        icon(chevron, theme::DISCLOSURE_SIZE, theme::TEXT_SECONDARY),
        text(title.to_owned())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_PRIMARY),
    ]
    .spacing(theme::MODULE_HEADER_SPACING)
    .align_y(Alignment::Center);
    if active {
        leading = leading.push(accent_dot());
    }
    let trailing: Element<'a, M> = match hint {
        Some(hint) => container(
            truncated_text(hint, theme::SIZE_CAPTION, theme::FONT, theme::TEXT_TERTIARY)
                .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into())),
        )
        .width(Length::Fill)
        .align_x(Horizontal::Right)
        .into(),
        None => Space::new().width(Length::Fill).into(),
    };
    let header = row![container(leading).width(Length::Shrink), trailing]
        .spacing(theme::MODULE_HEADER_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
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
        .on_press(on_toggle);
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

/// The scope chip: the bound mask's name in the accent, semibold, on the accent tint the band's own
/// Bar surface takes it at ([`theme::STRIP_SELECTED`], the accent at 16% over the Bar). It is not a
/// button — the band's press still toggles the section — and it never wraps: the hint before it is
/// what gives way.
fn scope_chip<'a, M: 'a>(scope: &str) -> Element<'a, M> {
    container(
        text(scope.to_owned())
            .size(SCOPE_SIZE)
            .font(theme::FONT_SEMIBOLD)
            .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(theme::ACCENT),
    )
    .padding(SCOPE_PADDING)
    .style(|_theme: &Theme| {
        container::Style::default()
            .background(theme::STRIP_SELECTED)
            .border(Border {
                radius: SCOPE_RADIUS.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            })
    })
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

    /// The scope chip is the board's `.scope`: 10.5 pt semibold accent on the accent at 16% over
    /// the Bar, radius 4, padding 1 × 6. It is 16 pt tall, so it fits the 32 pt band and leaves the
    /// band's height, collapsed or expanded, what it was.
    #[test]
    fn the_scope_chip_is_the_boards_accent_chip_inside_the_band() {
        assert_eq!(SCOPE_SIZE, 10.5);
        assert_eq!((SCOPE_PADDING, SCOPE_RADIUS), ([1.0, 6.0], 4.0));
        const {
            assert!(
                theme::CAPTION_LINE_HEIGHT + 2.0 * SCOPE_PADDING[0] <= theme::MODULE_HEADER_HEIGHT
            );
        }
        for (expanded, reset) in [(false, true), (true, true), (true, false)] {
            let _: Element<'_, ()> = section_header(
                &SectionHeaderModel {
                    title: "Basic".into(),
                    expanded,
                    active: true,
                    hint: Some("Exposure +0.60 · Shadows +20".into()),
                    unavailable: None,
                    reset,
                    status: None,
                    scope: Some("Face".into()),
                    enabled: true,
                },
                (),
                (),
            );
        }
        assert_eq!(collapsed_section_height(), 33.0);
    }

    /// The Masks band is a module band: every state builds, and its hint is drawn expanded too.
    #[test]
    fn the_masks_band_builds_expanded_and_collapsed() {
        for (active, expanded, hint) in [
            (
                true,
                true,
                Some("3 masks · Esc leaves Mask mode".to_owned()),
            ),
            (false, false, None),
        ] {
            let _: Element<'_, ()> = band_header("Masks", active, hint, expanded, ());
        }
    }

    /// The Module panels design's resulting heights at 300 pt: a collapsed section is 33 pt, and
    /// an expanded section is its band plus padding, its rows and the gaps between them.
    #[test]
    fn section_heights_follow_the_density() {
        assert_eq!(collapsed_section_height(), 33.0);
        // A module whose controls are one group draws them with no sub-group header, so
        // Presence, Vignette, Transforms and RAW are their rows alone.
        // Presence: three sliders.
        let group = theme::GROUP_MARGIN + theme::GROUP_HEADER_HEIGHT;
        let presence = expanded_section_height(3.0 * theme::SLIDER_ROW_HEIGHT, 3);
        // Vignette: four sliders.
        let vignette = expanded_section_height(4.0 * theme::SLIDER_ROW_HEIGHT, 4);
        assert_eq!(presence, 135.0);
        assert_eq!(vignette, 165.0);
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
        // Transforms: its icon row alone, the first and last row of the body, as Crop's idle
        // button row is.
        let transforms = expanded_section_height(
            super::super::button_row_height(
                super::super::ButtonSize::Regular,
                super::super::RowPlacement::default(),
            ),
            1,
        );
        assert_eq!(transforms, 77.0);
        // RAW: three sliders and the picker row ending the section.
        let raw = expanded_section_height(
            3.0 * theme::SLIDER_ROW_HEIGHT
                + super::super::button_row_height(
                    super::super::ButtonSize::Compact,
                    super::super::RowPlacement::default(),
                ),
            4,
        );
        assert_eq!(raw, 163.0);
    }
}
