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
            .width(Length::Fixed(theme::SWATCH_SIZE))
            .height(Length::Fixed(theme::SWATCH_SIZE)),
    )
    .padding(0)
    .style(if model.open {
        theme::swatch_open
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
                    // The dark ring separates any colour, even the panel's own, from the panel.
                    let size = frame.size();
                    let ring = canvas::Path::rounded_rectangle(
                        iced::Point::ORIGIN,
                        size,
                        theme::SWATCH_RADIUS.into(),
                    );
                    frame.fill(&ring, theme::THUMB_OUTLINE);
                    let inset = theme::BORDER_WIDTH;
                    frame.fill(
                        &canvas::Path::rounded_rectangle(
                            iced::Point::new(inset, inset),
                            Size::new(size.width - 2.0 * inset, size.height - 2.0 * inset),
                            (theme::SWATCH_RADIUS - inset).into(),
                        ),
                        color,
                    );
                }),
        ]
    }
}
