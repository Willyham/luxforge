//! Named vector icons and square icon buttons.

use crate::theme;
use iced::widget::{button, canvas, container, tooltip};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};
use std::cell::Cell;

const BUTTON_SIZE: f32 = 28.0;

/// Icons exposed to module action controls and the desktop shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    RotateLeft,
    RotateRight,
    Flip,
    Mirror,
    Crop,
    Picker,
    Reset,
    Plus,
    Minus,
    Lock,
    Swap,
    Guide,
    Pointer,
    Versions,
    Undo,
    Redo,
    Before,
    After,
    Clipping,
    StatePanel,
    ToolsPanel,
    ChevronDown,
    ChevronRight,
}

impl Icon {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rotate-left" => Self::RotateLeft,
            "rotate-right" => Self::RotateRight,
            "flip" => Self::Flip,
            "mirror" => Self::Mirror,
            "crop" => Self::Crop,
            "picker" => Self::Picker,
            "reset" => Self::Reset,
            "plus" => Self::Plus,
            "minus" => Self::Minus,
            "lock" => Self::Lock,
            "swap" => Self::Swap,
            "guide" => Self::Guide,
            "pointer" => Self::Pointer,
            "versions" => Self::Versions,
            "undo" => Self::Undo,
            "redo" => Self::Redo,
            "before" => Self::Before,
            "after" => Self::After,
            "clipping" => Self::Clipping,
            "state-panel" => Self::StatePanel,
            "tools-panel" => Self::ToolsPanel,
            "chevron-down" => Self::ChevronDown,
            "chevron-right" => Self::ChevronRight,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IconButtonModel {
    pub icon: Icon,
    pub tooltip: String,
    pub enabled: bool,
    pub selected: bool,
}

/// Draw a named path at a requested point size, without a glyph font dependency.
pub fn icon<'a, M: 'a>(icon: Icon, size: f32, color: Color) -> Element<'a, M> {
    canvas(IconDrawing { icon, color })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

pub fn icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::button_selected
    } else {
        theme::button_icon
    };
    let color = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    let control = button(container(icon::<M>(model.icon, 16.0, color)).center(Length::Fill))
        .width(Length::Fixed(BUTTON_SIZE))
        .height(Length::Fixed(BUTTON_SIZE))
        .style(style)
        .on_press_maybe(if model.enabled { on_press } else { None });
    tooltip(
        control,
        container(
            iced::widget::text(model.tooltip.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(6.0)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}

struct IconDrawing {
    icon: Icon,
    color: Color,
}

#[derive(Default)]
struct IconState {
    cache: canvas::Cache,
    key: Cell<Option<(Icon, Color)>>,
}

impl<M> canvas::Program<M> for IconDrawing {
    type State = IconState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        if state.key.get() != Some((self.icon, self.color)) {
            state.cache.clear();
            state.key.set(Some((self.icon, self.color)));
        }
        vec![state.cache.draw(renderer, bounds.size(), |frame| {
            draw_path(frame, self.icon, self.color)
        })]
    }
}

fn draw_path(frame: &mut canvas::Frame, icon: Icon, color: Color) {
    let s = frame.width().min(frame.height()) / 16.0;
    let p = |x: f32, y: f32| Point::new(x * s, y * s);
    let stroke = canvas::Stroke::default()
        .with_color(color)
        .with_width(1.55 * s);
    let line = |frame: &mut canvas::Frame, a: (f32, f32), b: (f32, f32)| {
        frame.stroke(&canvas::Path::line(p(a.0, a.1), p(b.0, b.1)), stroke);
    };
    let poly = |frame: &mut canvas::Frame, points: &[(f32, f32)]| {
        let mut path = canvas::path::Builder::new();
        if let Some(&(x, y)) = points.first() {
            path.move_to(p(x, y));
            for &(x, y) in &points[1..] {
                path.line_to(p(x, y));
            }
            frame.stroke(&path.build(), stroke);
        }
    };
    match icon {
        Icon::Plus => {
            line(frame, (8.0, 3.0), (8.0, 13.0));
            line(frame, (3.0, 8.0), (13.0, 8.0));
        }
        Icon::Minus => line(frame, (3.0, 8.0), (13.0, 8.0)),
        Icon::Reset | Icon::RotateLeft | Icon::RotateRight | Icon::Undo | Icon::Redo => {
            let right = matches!(icon, Icon::RotateRight | Icon::Redo);
            let x = if right { 12.5 } else { 3.5 };
            let arc = if right {
                [
                    (4.0, 5.0),
                    (6.0, 3.5),
                    (9.0, 3.5),
                    (12.0, 5.0),
                    (13.0, 8.0),
                    (11.5, 11.0),
                    (8.0, 12.5),
                    (5.0, 11.0),
                ]
            } else {
                [
                    (12.0, 5.0),
                    (10.0, 3.5),
                    (7.0, 3.5),
                    (4.0, 5.0),
                    (3.0, 8.0),
                    (4.5, 11.0),
                    (8.0, 12.5),
                    (11.0, 11.0),
                ]
            };
            poly(frame, &arc);
            poly(frame, &[(x - 2.0, 5.0), (x, 5.0), (x, 3.0)]);
        }
        Icon::Flip | Icon::Mirror => {
            if icon == Icon::Flip {
                line(frame, (2.0, 8.0), (14.0, 8.0));
                poly(frame, &[(4.0, 3.0), (12.0, 3.0), (8.0, 6.5), (4.0, 3.0)]);
                poly(frame, &[(4.0, 13.0), (12.0, 13.0), (8.0, 9.5), (4.0, 13.0)]);
            } else {
                line(frame, (8.0, 2.0), (8.0, 14.0));
                poly(frame, &[(3.0, 4.0), (3.0, 12.0), (6.5, 8.0), (3.0, 4.0)]);
                poly(frame, &[(13.0, 4.0), (13.0, 12.0), (9.5, 8.0), (13.0, 4.0)]);
            }
        }
        Icon::Crop => {
            poly(frame, &[(3.0, 2.0), (3.0, 11.0), (13.0, 11.0)]);
            poly(frame, &[(13.0, 14.0), (13.0, 5.0), (3.0, 5.0)]);
        }
        Icon::Picker => {
            poly(
                frame,
                &[
                    (3.0, 12.5),
                    (10.5, 5.0),
                    (12.0, 6.5),
                    (4.5, 14.0),
                    (3.0, 12.5),
                ],
            );
            line(frame, (9.5, 4.0), (12.0, 6.5));
            line(frame, (11.0, 2.5), (13.5, 5.0));
        }
        Icon::Lock => {
            frame.stroke(
                &canvas::Path::rectangle(p(4.0, 7.0), iced::Size::new(8.0 * s, 7.0 * s)),
                stroke,
            );
            poly(
                frame,
                &[
                    (5.5, 7.0),
                    (5.5, 4.5),
                    (7.0, 3.0),
                    (9.0, 3.0),
                    (10.5, 4.5),
                    (10.5, 7.0),
                ],
            );
        }
        Icon::Swap => {
            poly(frame, &[(2.0, 5.0), (13.0, 5.0), (10.5, 2.5)]);
            poly(frame, &[(14.0, 11.0), (3.0, 11.0), (5.5, 13.5)]);
        }
        Icon::Guide => {
            frame.stroke(
                &canvas::Path::rectangle(p(2.0, 2.0), iced::Size::new(12.0 * s, 12.0 * s)),
                stroke,
            );
            line(frame, (8.0, 2.0), (8.0, 14.0));
            line(frame, (2.0, 8.0), (14.0, 8.0));
        }
        Icon::Pointer => poly(
            frame,
            &[
                (3.0, 2.0),
                (3.0, 13.0),
                (6.5, 10.0),
                (8.5, 14.0),
                (10.0, 13.2),
                (8.0, 9.5),
                (13.0, 9.0),
                (3.0, 2.0),
            ],
        ),
        Icon::Versions => {
            frame.stroke(
                &canvas::Path::rectangle(p(4.0, 2.0), iced::Size::new(9.0 * s, 10.0 * s)),
                stroke,
            );
            poly(frame, &[(2.0, 5.0), (2.0, 14.0), (11.0, 14.0)]);
        }
        Icon::Before | Icon::After => {
            frame.stroke(
                &canvas::Path::rectangle(p(2.0, 2.0), iced::Size::new(12.0 * s, 12.0 * s)),
                stroke,
            );
            line(frame, (8.0, 2.0), (8.0, 14.0));
            if icon == Icon::Before {
                line(frame, (3.0, 4.0), (6.0, 4.0));
            } else {
                line(frame, (10.0, 4.0), (13.0, 4.0));
            }
        }
        Icon::Clipping => {
            poly(frame, &[(2.0, 12.5), (8.0, 3.0), (14.0, 12.5), (2.0, 12.5)]);
            line(frame, (8.0, 6.0), (8.0, 9.5));
        }
        Icon::StatePanel | Icon::ToolsPanel => {
            frame.stroke(
                &canvas::Path::rectangle(p(2.0, 2.0), iced::Size::new(12.0 * s, 12.0 * s)),
                stroke,
            );
            let x = if icon == Icon::StatePanel { 6.0 } else { 10.0 };
            line(frame, (x, 2.0), (x, 14.0));
        }
        Icon::ChevronDown => poly(frame, &[(3.5, 6.0), (8.0, 10.5), (12.5, 6.0)]),
        Icon::ChevronRight => poly(frame, &[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_icon_builds_at_both_sizes() {
        for icon in [
            Icon::RotateLeft,
            Icon::RotateRight,
            Icon::Flip,
            Icon::Mirror,
            Icon::Crop,
            Icon::Picker,
            Icon::Reset,
            Icon::Plus,
            Icon::Minus,
            Icon::Lock,
            Icon::Swap,
            Icon::Guide,
            Icon::Pointer,
            Icon::Versions,
            Icon::Undo,
            Icon::Redo,
            Icon::Before,
            Icon::After,
            Icon::Clipping,
            Icon::StatePanel,
            Icon::ToolsPanel,
            Icon::ChevronDown,
            Icon::ChevronRight,
        ] {
            let _: Element<'_, ()> = super::icon(icon, 12.0, theme::TEXT_PRIMARY);
            let _: Element<'_, ()> = super::icon(icon, 16.0, theme::TEXT_PRIMARY);
        }
    }
}
