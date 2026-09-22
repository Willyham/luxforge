//! A colour swatch that publishes a press without interpreting the colour.

use crate::theme;
use iced::{
    Color, Element, Length, Renderer, Size, Theme,
    widget::{button, canvas},
};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSwatchModel {
    pub rgb: [u8; 3],
    pub enabled: bool,
    pub open: bool,
}

pub fn color_swatch<'a, M: Clone + 'a>(model: &ColorSwatchModel, on_press: M) -> Element<'a, M> {
    button(
        canvas::Canvas::new(Swatch { model: *model })
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(20.0)),
    )
    .padding(0)
    .style(if model.open {
        theme::button_selected
    } else {
        theme::button_plain
    })
    .on_press_maybe(model.enabled.then_some(on_press))
    .into()
}

struct Swatch {
    model: ColorSwatchModel,
}

#[derive(Default)]
struct SwatchState {
    cache: canvas::Cache,
    rgb: Cell<Option<[u8; 3]>>,
}

impl<M> canvas::Program<M> for Swatch {
    type State = SwatchState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        if state.rgb.get() != Some(self.model.rgb) {
            state.cache.clear();
            state.rgb.set(Some(self.model.rgb));
        }
        let color = Color::from_rgb8(self.model.rgb[0], self.model.rgb[1], self.model.rgb[2]);
        vec![
            state
                .cache
                .draw(renderer, Size::new(bounds.width, bounds.height), |frame| {
                    frame.fill_rectangle(iced::Point::ORIGIN, frame.size(), color);
                }),
        ]
    }
}
