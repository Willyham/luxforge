//! Small colour chips pressed to choose or remove a colour: the overlay's two tint swatches, and a
//! colour range's held swatches with their empty slots and how many it holds, as the mask-panels
//! board draws `.sw2` and the range-rows board the colour range's swatch line.
//!
//! Every chip is drawn inside a [`theme::OVERLAY_SWATCH_RING`] margin whether or not it is chosen,
//! so choosing one draws its accent ring without moving its neighbours; the rows' spacing takes
//! the two margins off the board's gap.

use crate::theme;
use crate::{Element, Theme, Token};
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, button, canvas, row, text};
use iced::{Alignment, Color, Length, Point, Rectangle, Renderer, Size};

/// One chip: a colour, or an empty dashed slot when `fill` is `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Chip {
    pub fill: Option<Color>,
    /// The chip's own size, without its ring margin.
    pub size: Size,
    pub radius: f32,
    /// Chosen: the accent ring outside it.
    pub ring: bool,
    /// Every colour's opacity: [`theme::SWATCH_DISABLED_OPACITY`] for a chip that cannot be
    /// chosen now.
    pub opacity: f32,
}

impl Chip {
    /// The chip drawn at its size plus its ring margin on every side.
    pub(crate) fn element<'a, M: 'a>(self) -> Element<'a, M> {
        let margin = 2.0 * theme::OVERLAY_SWATCH_RING;
        canvas(self)
            .width(Length::Fixed(self.size.width + margin))
            .height(Length::Fixed(self.size.height + margin))
            .into()
    }

    /// The chip as a button: bare, so the chip is all that shows.
    pub(crate) fn button<'a, M: Clone + 'a>(self, on_press: Option<M>) -> Element<'a, M> {
        button(self.element())
            .padding(0)
            .style(theme::button_bare)
            .on_press_maybe(on_press)
            .into()
    }
}

impl<M> canvas::Program<M, Theme> for Chip {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let fade = |color: Color| Color {
            a: color.a * self.opacity,
            ..color
        };
        let ring = theme::OVERLAY_SWATCH_RING;
        if self.ring {
            frame.fill(
                &canvas::Path::rounded_rectangle(
                    Point::ORIGIN,
                    bounds.size(),
                    (self.radius + ring).into(),
                ),
                fade(theme.palette().accent),
            );
        }
        let origin = Point::new(ring, ring);
        match self.fill {
            Some(fill) => {
                let inset = theme::BORDER_WIDTH;
                frame.fill(
                    &canvas::Path::rounded_rectangle(origin, self.size, self.radius.into()),
                    fade(theme::SWATCH_OUTLINE),
                );
                frame.fill(
                    &canvas::Path::rounded_rectangle(
                        Point::new(ring + inset, ring + inset),
                        Size::new(
                            self.size.width - 2.0 * inset,
                            self.size.height - 2.0 * inset,
                        ),
                        (self.radius - inset).max(0.0).into(),
                    ),
                    fade(fill),
                );
            }
            None => {
                let half = theme::BORDER_WIDTH / 2.0;
                let dash = [theme::SWATCH_SLOT_DASH, theme::SWATCH_SLOT_DASH];
                frame.stroke(
                    &canvas::Path::rounded_rectangle(
                        Point::new(ring + half, ring + half),
                        Size::new(self.size.width - 2.0 * half, self.size.height - 2.0 * half),
                        (self.radius - half).into(),
                    ),
                    canvas::Stroke {
                        line_dash: canvas::LineDash {
                            segments: &dash,
                            offset: 0,
                        },
                        ..canvas::Stroke::default()
                            .with_color(fade(theme.palette().text_faint))
                            .with_width(theme::BORDER_WIDTH)
                    },
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The space between two chips' canvases that leaves `gap` between the chips themselves.
pub(crate) const fn chip_spacing(gap: f32) -> f32 {
    gap - 2.0 * theme::OVERLAY_SWATCH_RING
}

/// Plain data for a colour range's swatches: the held colours, the empty slots up to the limit,
/// and the caption at the line's end.
#[derive(Debug, Clone, PartialEq)]
pub struct SwatchSlotsModel {
    /// The held swatches, in order.
    pub swatches: Vec<[u8; 3]>,
    /// How many swatches the range can hold: the slots drawn in all.
    pub limit: usize,
    /// A held swatch drawn chosen, such as the one a menu is open on.
    pub selected: Option<usize>,
    /// What the line says at its end: how many it holds (`2 of 5`), or that it holds none yet.
    pub caption: Option<String>,
    pub enabled: bool,
}

/// How many empty slots follow the held swatches.
pub(crate) fn empty_slots(held: usize, limit: usize) -> usize {
    limit.saturating_sub(held)
}

/// Renders a colour range's swatch line, inset under its component row. A press on a held swatch
/// publishes `on_swatch` with its index.
pub fn swatch_slots<'a, M: Clone + 'a>(
    model: &SwatchSlotsModel,
    on_swatch: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let size = Size::new(theme::SWATCH_SLOT_WIDTH, theme::SWATCH_SLOT_HEIGHT);
    let chip = |fill, ring| Chip {
        fill,
        size,
        radius: theme::SWATCH_SLOT_RADIUS,
        ring,
        opacity: if model.enabled {
            1.0
        } else {
            theme::SWATCH_DISABLED_OPACITY
        },
    };
    let mut slots = Row::new()
        .spacing(chip_spacing(theme::SWATCH_SLOT_SPACING))
        .align_y(Alignment::Center);
    for (index, rgb) in model.swatches.iter().enumerate() {
        let fill = Color::from_rgb8(rgb[0], rgb[1], rgb[2]);
        slots = slots.push(
            chip(Some(fill), model.selected == Some(index))
                .button(model.enabled.then(|| on_swatch(index))),
        );
    }
    for _ in 0..empty_slots(model.swatches.len(), model.limit) {
        slots = slots.push(chip(None, false).element());
    }
    let mut content = row![slots, Space::new().width(Length::Fill)]
        .align_y(Alignment::Center)
        .width(Length::Fill);
    if let Some(caption) = &model.caption {
        content = content.push(
            text(caption.clone())
                .size(theme::SIZE_SMALL_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    iced::widget::container(content)
        .padding(theme::SWATCH_LINE_PADDING)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slots_fill_the_row_up_to_the_limit() {
        assert_eq!(empty_slots(3, 5), 2);
        assert_eq!(empty_slots(5, 5), 0);
        assert_eq!(empty_slots(6, 5), 0);
    }

    /// mask-panels.html: slots 6 pt apart and tint swatches 6 pt apart, measured between the chips
    /// rather than their ring margins.
    #[test]
    fn chips_keep_the_boards_gap_between_their_margins() {
        assert_eq!(
            chip_spacing(theme::SWATCH_SLOT_SPACING) + 2.0 * theme::OVERLAY_SWATCH_RING,
            6.0
        );
    }

    #[test]
    fn every_state_builds() {
        for (caption, enabled) in [
            (Some("3 of 5".to_owned()), true),
            (None, true),
            (Some("No colour yet".to_owned()), false),
        ] {
            let _: Element<'_, usize> = swatch_slots(
                &SwatchSlotsModel {
                    swatches: vec![[111, 155, 209], [93, 139, 196], [143, 179, 224]],
                    limit: 5,
                    selected: Some(1),
                    caption,
                    enabled,
                },
                |index| index,
            );
        }
    }
}
