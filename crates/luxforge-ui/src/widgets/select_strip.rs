//! The Select workspace's floating strip, as the event and catalog boards draw it (`.strip`): Grid
//! and Loupe, a rule, the sort with its menu, a rule, and the size slider that sets the grid's cell
//! width.
//!
//! It draws what it is given and sends the caller's messages; the sort's menu is the caller's
//! [`crate::menu_list`], dropped through [`crate::popover`].

use super::icon_button::{Icon, icon};
use super::mode_strip::tooltip_text;
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Space, button, container, row, slider, text, tooltip};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

/// How wide the size slider is.
pub const STRIP_SLIDER_WIDTH: f32 = 110.0;

/// Plain data for the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectStripModel {
    /// Why the Loupe cannot be entered, as its tooltip says; `None` while it can.
    pub loupe_unavailable: Option<String>,
    /// The sort's label (`Capture time`), and whether it can be changed.
    pub sort: String,
    pub sort_enabled: bool,
    /// The cell width the slider shows, its range and its step.
    pub cell_width: f32,
    pub cell_range: (f32, f32),
    pub cell_step: f32,
}

/// Renders the strip. The Grid tool is always the selected one; `on_loupe` enters the Loupe;
/// `on_sort` opens or closes the sort's menu, which is `sort_menu` while open and is closed by a
/// press elsewhere with `on_dismiss`; the slider publishes `on_width` with each new width.
pub fn select_strip<'a, M: Clone + 'a>(
    model: &SelectStripModel,
    on_loupe: Option<M>,
    on_sort: Option<M>,
    sort_menu: Option<Element<'a, M>>,
    on_dismiss: M,
    on_width: impl Fn(f32) -> M + 'a,
) -> Element<'a, M> {
    let loupe_label = match &model.loupe_unavailable {
        Some(reason) => format!("Loupe (Space) \u{b7} {reason}"),
        None => tooltip_text("Loupe", &Some("Space".into())),
    };
    let sort_ink = if model.sort_enabled {
        theme::STRIP_ICON
    } else {
        theme::TEXT_TERTIARY
    };
    let sort = button(
        row![
            icon(Icon::Sort, theme::SMALL_ICON_SIZE, sort_ink),
            text(model.sort.clone())
                .size(theme::SIZE_CONTROL)
                .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
                .wrapping(Wrapping::None)
                .color(sort_ink),
            icon(Icon::ChevronDown, theme::DROPDOWN_CHEVRON_SIZE, sort_ink),
        ]
        .spacing(theme::BUTTON_ICON_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill),
    )
    .padding([0.0, theme::BUTTON_PADDING])
    .height(Length::Fixed(theme::STRIP_TOOL_HEIGHT))
    .style(theme::strip_tool(false))
    .on_press_maybe(on_sort.filter(|_| model.sort_enabled));
    let (low, high) = model.cell_range;
    let width = slider(low..=high, model.cell_width.clamp(low, high), on_width)
        .step(model.cell_step)
        .width(Length::Fixed(STRIP_SLIDER_WIDTH))
        .style(size_slider);
    let content = row![
        tool(Icon::Grid, "Grid".into(), true, None),
        tool(Icon::Loupe, loupe_label, false, on_loupe),
        rule(),
        crate::popover(sort, sort_menu, on_dismiss),
        rule(),
        container(width).padding([0.0, theme::SPACING]),
    ]
    .spacing(theme::STRIP_SPACING)
    .align_y(Alignment::Center);
    container(content)
        .padding(theme::STRIP_PADDING + theme::BORDER_WIDTH)
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::STRIP_RADIUS))
        .into()
}

/// One icon tool, with its tooltip. The Grid tool is selected and needs no message.
fn tool<'a, M: Clone + 'a>(
    glyph: Icon,
    label: String,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (selected, on_press.is_some()) {
        (true, _) => theme::ACCENT,
        (false, true) => theme::STRIP_ICON,
        (false, false) => theme::TEXT_TERTIARY,
    };
    let control = button(container(icon(glyph, theme::ICON_SIZE, ink)).center(Length::Fill))
        .padding(Padding::ZERO)
        .width(Length::Fixed(theme::STRIP_TOOL_WIDTH))
        .height(Length::Fixed(theme::STRIP_TOOL_HEIGHT))
        .style(theme::strip_tool(selected))
        .on_press_maybe(on_press);
    tooltip(
        control,
        container(
            text(label)
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(theme::TOOLTIP_PADDING)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}

/// The 1 × 16 pt rule between the strip's groups.
fn rule<'a, M: Clone + 'a>() -> Element<'a, M> {
    container(
        container(Space::new())
            .width(Length::Fixed(theme::BORDER_WIDTH))
            .height(Length::Fixed(theme::STRIP_RULE_HEIGHT))
            .style(|_: &Theme| container::Style::default().background(theme::STRIP_RULE)),
    )
    .padding(Padding::from([0.0, theme::STRIP_RULE_MARGIN]))
    .into()
}

/// The size slider: the panel rail with its fill up to the thumb, and the resting thumb in its dark
/// ring, the accent while dragged.
fn size_slider(_theme: &Theme, status: slider::Status) -> slider::Style {
    let dragged = matches!(status, slider::Status::Dragged);
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(theme::RAIL_FILL),
                Background::Color(theme::RAIL),
            ),
            width: theme::RAIL_WIDTH,
            border: Border::default(),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle {
                radius: theme::THUMB_RADIUS,
            },
            background: Background::Color(if dragged { theme::ACCENT } else { theme::THUMB }),
            border_color: if dragged {
                Color::TRANSPARENT
            } else {
                theme::THUMB_OUTLINE
            },
            border_width: theme::THUMB_OUTLINE_WIDTH,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        for (loupe, sort_enabled) in [(None, true), (Some("not yet".to_owned()), false)] {
            let model = SelectStripModel {
                loupe_unavailable: loupe,
                sort: "Capture time".into(),
                sort_enabled,
                cell_width: 136.0,
                cell_range: (96.0, 320.0),
                cell_step: 4.0,
            };
            let _: Element<'_, ()> = select_strip(&model, Some(()), Some(()), None, (), |_| ());
            let _: Element<'_, ()> =
                select_strip(&model, None, None, Some(text("menu").into()), (), |_| ());
        }
    }
}
