//! A colour swatch that publishes a press without interpreting the colour.

use crate::theme;
use crate::widgets::curve_editor::invalidate_on_version_change;
use crate::{Element, Theme};
use iced::{
    Color, Length, Renderer, Size,
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
    key: Cell<Option<SwatchKey>>,
}

/// What a swatch's drawing depends on: its colour and the theme's generation, which colours its
/// ring.
type SwatchKey = ([u8; 3], u64);

impl Swatch {
    /// Calls `clear` when the colour or the theme changed since the cache was drawn.
    fn refresh(&self, key: &Cell<Option<SwatchKey>>, theme: &Theme, clear: impl FnOnce()) {
        invalidate_on_version_change(key, (self.model.rgb, theme.generation()), clear);
    }
}

impl<M> canvas::Program<M, Theme> for Swatch {
    type State = SwatchState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        self.refresh(&state.key, theme, || state.cache.clear());
        let ring_colour = theme.palette().thumb_outline;
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
                    frame.fill(&ring, ring_colour);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A swatch is drawn again when the theme changes, since its ring is the theme's.
    #[test]
    fn a_theme_change_redraws_a_swatch() {
        let swatch = Swatch {
            model: ColorSwatchModel {
                rgb: [10, 20, 30],
                open: false,
                enabled: true,
            },
        };
        let (first, second) = (Theme::luxforge_dark(), Theme::luxforge_dark());
        let key = Cell::new(None);
        let mut clears = 0;
        swatch.refresh(&key, &first, || clears += 1);
        swatch.refresh(&key, &first, || clears += 1);
        assert_eq!(clears, 1, "an unchanged theme keeps the drawing");
        swatch.refresh(&key, &second, || clears += 1);
        assert_eq!(clears, 2, "a new generation redraws it");
    }
}
