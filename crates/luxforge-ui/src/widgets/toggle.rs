//! A plain-data boolean control: one [`theme::TOGGLE_ROW_HEIGHT`] row, the label at the left and a
//! switch at the right, as the crop section's Straighten guide draws it.

use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{button, canvas, row, text};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Theme};

#[derive(Debug, Clone, PartialEq)]
pub struct ToggleModel {
    pub label: String,
    pub on: bool,
    pub enabled: bool,
}

pub fn toggle<'a, M: Clone + 'a>(
    model: &ToggleModel,
    on_toggle: impl Fn(bool) -> M + 'a,
) -> Element<'a, M> {
    row![label(model).width(Length::Fill), switch(model, on_toggle)]
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::TOGGLE_ROW_HEIGHT))
        .into()
}

/// A toggle as the Masks panel draws its rows (Invert mask, Erase, Limit to colour): one
/// [`theme::COMPACT_TOGGLE_ROW_HEIGHT`] row, with `hint` (`hold ⌥`, `refine 50`) in tertiary text
/// [`theme::TOGGLE_HINT_SPACING`] after the label.
pub fn compact_toggle<'a, M: Clone + 'a>(
    model: &ToggleModel,
    hint: Option<String>,
    on_toggle: impl Fn(bool) -> M + 'a,
) -> Element<'a, M> {
    let mut labelled = row![label(model)]
        .spacing(theme::TOGGLE_HINT_SPACING)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    if let Some(hint) = hint {
        labelled = labelled.push(
            text(hint)
                .size(theme::SIZE_SMALL_CAPTION)
                .wrapping(Wrapping::None)
                .color(theme::TEXT_TERTIARY),
        );
    }
    row![labelled, switch(model, on_toggle)]
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::COMPACT_TOGGLE_ROW_HEIGHT))
        .into()
}

fn label<'a>(model: &ToggleModel) -> iced::widget::Text<'a> {
    text(model.label.clone())
        .size(theme::SIZE_CONTROL)
        .color(if model.enabled {
            theme::TEXT_LABEL
        } else {
            theme::TEXT_TERTIARY
        })
}

fn switch<'a, M: Clone + 'a>(
    model: &ToggleModel,
    on_toggle: impl Fn(bool) -> M + 'a,
) -> Element<'a, M> {
    button(
        canvas(Switch {
            on: model.on,
            enabled: model.enabled,
        })
        .width(Length::Fixed(theme::SWITCH_WIDTH))
        .height(Length::Fixed(theme::SWITCH_HEIGHT)),
    )
    .padding(0)
    .style(theme::button_bare)
    .on_press_maybe(model.enabled.then(|| on_toggle(!model.on)))
    .into()
}

/// The knob's centre, in points from the switch's left edge: inset at the left when off and at the
/// right when on.
pub(crate) fn knob_center(width: f32, on: bool) -> f32 {
    let travel = theme::SWITCH_INSET + theme::SWITCH_KNOB / 2.0;
    if on { width - travel } else { travel }
}

struct Switch {
    on: bool,
    enabled: bool,
}

impl<M> canvas::Program<M> for Switch {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (track, knob): (Color, Color) = match (self.enabled, self.on) {
            (false, _) => (theme::CONTROL, theme::TEXT_TERTIARY),
            (true, false) => (theme::RAIL, theme::TEXT_SECONDARY),
            (true, true) => (theme::ACCENT, theme::THUMB),
        };
        let radius = bounds.height / 2.0;
        frame.fill(
            &canvas::Path::rounded_rectangle(Point::ORIGIN, bounds.size(), radius.into()),
            track,
        );
        frame.fill(
            &canvas::Path::circle(
                Point::new(knob_center(bounds.width, self.on), radius),
                theme::SWITCH_KNOB / 2.0,
            ),
            knob,
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// crop-and-straighten.png: a 26 × 14 pt track, a 10 pt knob inset 2 pt.
    #[test]
    fn the_switch_matches_the_crop_reference() {
        assert_eq!((theme::SWITCH_WIDTH, theme::SWITCH_HEIGHT), (26.0, 14.0));
        assert_eq!(knob_center(26.0, false), 7.0);
        assert_eq!(knob_center(26.0, true), 19.0);
    }

    #[test]
    fn every_state_builds() {
        for on in [false, true] {
            for enabled in [false, true] {
                let _: Element<'_, bool> = toggle(
                    &ToggleModel {
                        label: "Straighten guide".into(),
                        on,
                        enabled,
                    },
                    |on| on,
                );
                for hint in [None, Some("hold \u{2325}".to_string())] {
                    let _: Element<'_, bool> = compact_toggle(
                        &ToggleModel {
                            label: "Erase".into(),
                            on,
                            enabled,
                        },
                        hint,
                        |on| on,
                    );
                }
            }
        }
    }
}
