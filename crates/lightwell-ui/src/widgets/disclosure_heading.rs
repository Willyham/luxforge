//! A section heading that opens and closes its section: the capitalised label, an optional caption
//! and a chevron, [`theme::DISCLOSURE_HEADING_HEIGHT`] tall, with the whole row one button.
//!
//! It is a list heading (History, Recipe) that also discloses, so its label is drawn exactly as
//! [`super::list_heading`] draws one and starts at the same left edge, with no inset of its own.
//! Hovering lifts the label and the chevron from tertiary to secondary together; the caption keeps
//! its colour, because it reports state rather than offering the action.

use super::icon_button::{CHEVRON_DOWN, CHEVRON_RIGHT, Icon, draw_icon};
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Space, button, canvas, row, stack, text};
use iced::{Alignment, Element, Length, Rectangle, Renderer, Size, Theme, Vector};
use std::cell::Cell;

/// Renders a disclosure heading. `trailing` is a short caption right-aligned before the chevron,
/// such as how many jobs are running; the chevron points down while `expanded` and right while
/// collapsed. Pressing anywhere on the row publishes `on_toggle`.
pub fn disclosure_heading<'a, M: Clone + 'a>(
    label: &str,
    trailing: Option<String>,
    expanded: bool,
    on_toggle: M,
) -> Element<'a, M> {
    // The label names no colour, so it takes the button's text colour, which is what lets a hover
    // anywhere on the row lift it (see `theme::button_disclosure`).
    let mut content = row![
        text(label.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .wrapping(Wrapping::None),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fill);
    if let Some(trailing) = trailing {
        content = content.push(
            text(trailing)
                .size(theme::SIZE_SMALL_CAPTION)
                .wrapping(Wrapping::None)
                .color(theme::TEXT_TERTIARY),
        );
    }
    // Room for the chevron, which the layer above draws against the row's right edge.
    content = content.push(Space::new().width(Length::Fixed(
        theme::SPACING + theme::DISCLOSURE_CHEVRON_SIZE,
    )));

    // The chevron is a layer over the whole row rather than an icon in its slot, so it can tell
    // when the pointer is anywhere on the row and lift with the label.
    let chevron = canvas(Chevron { expanded })
        .width(Length::Fill)
        .height(Length::Fill);

    button(stack![content, chevron])
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fixed(theme::DISCLOSURE_HEADING_HEIGHT))
        .style(theme::button_disclosure)
        .on_press(on_toggle)
        .into()
}

/// Where the chevron's [`theme::DISCLOSURE_CHEVRON_SIZE`] square goes in a heading of `size`:
/// centred vertically, and far enough right that the chevron's ink, not its square, ends at the
/// row's right edge, where the values and captions under it end. The two chevrons' ink is not the
/// same width, so each is placed by its own.
pub(crate) fn chevron_origin(expanded: bool, size: Size) -> Vector {
    let polyline = if expanded {
        CHEVRON_DOWN
    } else {
        CHEVRON_RIGHT
    };
    let scale = theme::DISCLOSURE_CHEVRON_SIZE / 16.0;
    let ink_right = polyline.iter().map(|&(x, _)| x).fold(f32::MIN, f32::max) * scale
        + theme::ICON_STROKE_WIDTH / 2.0;
    Vector::new(
        size.width - ink_right,
        (size.height - theme::DISCLOSURE_CHEVRON_SIZE) / 2.0,
    )
}

/// The chevron layer: it draws nothing but the chevron and never captures an event, so presses
/// reach the button under it.
struct Chevron {
    expanded: bool,
}

/// The tessellated chevron and the direction and hover it was drawn for.
#[derive(Default)]
struct ChevronState {
    cache: canvas::Cache,
    key: Cell<Option<(bool, bool)>>,
}

impl<M> canvas::Program<M> for Chevron {
    type State = ChevronState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        // The layer covers the row, so the pointer over it is the pointer over the button, whose
        // own status change is what requests this redraw.
        let hovered = cursor.is_over(bounds);
        let key = (self.expanded, hovered);
        if state.key.get() != Some(key) {
            state.cache.clear();
            state.key.set(Some(key));
        }
        let icon = if self.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let origin = chevron_origin(self.expanded, bounds.size());
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            frame.with_save(|frame| {
                frame.translate(origin);
                draw_icon(
                    frame,
                    icon,
                    theme::DISCLOSURE_CHEVRON_SIZE,
                    theme::disclosure_color(hovered),
                );
            });
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each chevron's ink ends at the row's right edge, within a stroke, and its square is centred
    /// in the row.
    #[test]
    fn each_chevron_ends_at_the_right_edge_and_is_centred() {
        let size = Size::new(224.0, theme::DISCLOSURE_HEADING_HEIGHT);
        let scale = theme::DISCLOSURE_CHEVRON_SIZE / 16.0;
        for (expanded, polyline) in [(true, CHEVRON_DOWN), (false, CHEVRON_RIGHT)] {
            let origin = chevron_origin(expanded, size);
            assert_eq!(origin.y, 6.0, "a 10 pt square in a 22 pt row");
            let right = polyline
                .iter()
                .map(|&(x, _)| origin.x + x * scale)
                .fold(f32::MIN, f32::max);
            assert!(
                (size.width - right - theme::ICON_STROKE_WIDTH / 2.0).abs() < 1e-4,
                "{expanded}: ink ends at {right}"
            );
            // The square itself may overhang the row's edge, never the ink.
            assert!(origin.x + theme::DISCLOSURE_CHEVRON_SIZE > size.width - 4.0);
        }
    }

    #[test]
    fn every_state_builds() {
        for expanded in [true, false] {
            for trailing in [None, Some("2 jobs".to_string())] {
                let _: Element<'_, ()> = disclosure_heading("Performance", trailing, expanded, ());
            }
        }
    }
}
