//! Named vector icons and square icon buttons.

use crate::theme;
use iced::widget::{button, canvas, container, tooltip};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};
use std::cell::Cell;

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
    ShadowClipping,
    HighlightClipping,
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
            "shadow-clipping" => Self::ShadowClipping,
            "highlight-clipping" => Self::HighlightClipping,
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
    let color = if model.enabled {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    sized_icon_button(
        model,
        on_press,
        theme::ICON_BUTTON_SIZE,
        theme::ICON_SIZE,
        color,
    )
}

/// The compact icon button a module band or a sub-group header carries at its right end, such as
/// its reset: a [`theme::HEADER_ICON_SIZE`] icon in the secondary text colour inside a
/// [`theme::HEADER_BUTTON_SIZE`] square.
pub fn header_icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let color = if model.enabled {
        theme::TEXT_SECONDARY
    } else {
        theme::TEXT_TERTIARY
    };
    sized_icon_button(
        model,
        on_press,
        theme::HEADER_BUTTON_SIZE,
        theme::HEADER_ICON_SIZE,
        color,
    )
}

fn sized_icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
    size: f32,
    icon_size: f32,
    color: Color,
) -> Element<'a, M> {
    let style = if model.selected {
        theme::button_selected
    } else {
        theme::button_icon
    };
    // No padding: Iced's default button padding would squeeze the icon's canvas below its
    // declared size and draw the glyph shrunk into the top-left corner.
    let control = button(container(icon::<M>(model.icon, icon_size, color)).center(Length::Fill))
        .padding(0)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
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

/// The circular arrow's head on the 16-unit icon grid: an L whose corner sits on the circle in the
/// arc's open upper-left gap, one arm up and one along, as the module references draw it.
const ARROW_HEAD: [(f32, f32); 3] = [(3.7, 3.4), (3.7, 7.0), (7.2, 7.0)];

/// The circular arrow's arc on the 16-unit icon grid, as a polyline: centre (8.15, 8.2), radius
/// 4.15, from left of the top (−115°) clockwise in screen space round to the left side
/// (165°), leaving the upper-left gap the head sits in. A polyline rather than a canvas arc so
/// the mirrored icons are a plain reflection.
fn circular_arrow_arc() -> Vec<(f32, f32)> {
    const STEPS: usize = 24;
    let (cx, cy, radius) = (8.15_f32, 8.2_f32, 4.15_f32);
    let (start, end) = (-115.0_f32.to_radians(), 165.0_f32.to_radians());
    (0..=STEPS)
        .map(|step| {
            let angle = start + (end - start) * step as f32 / STEPS as f32;
            (cx + radius * angle.cos(), cy + radius * angle.sin())
        })
        .collect()
}

fn draw_path(frame: &mut canvas::Frame, icon: Icon, color: Color) {
    let s = frame.width().min(frame.height()) / 16.0;
    let p = |x: f32, y: f32| Point::new(x * s, y * s);
    let stroke = canvas::Stroke::default()
        .with_color(color)
        .with_width(theme::ICON_STROKE_WIDTH);
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
            // A circular arrow: an open circle from the top, round through the right and the
            // bottom to the left, with an L-shaped head in the gap at its upper left. The
            // clockwise icons are the mirror image.
            let mirror = matches!(icon, Icon::RotateRight | Icon::Redo);
            let x = |x: f32| if mirror { 16.0 - x } else { x };
            let points: Vec<(f32, f32)> = circular_arrow_arc()
                .into_iter()
                .map(|(px, py)| (x(px), py))
                .collect();
            poly(frame, &points);
            let head: Vec<(f32, f32)> = ARROW_HEAD.iter().map(|&(px, py)| (x(px), py)).collect();
            poly(frame, &head);
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
        Icon::ShadowClipping | Icon::HighlightClipping => {
            let mut path = canvas::path::Builder::new();
            let x = if icon == Icon::ShadowClipping {
                3.0
            } else {
                13.0
            };
            path.move_to(p(x, 3.0));
            path.line_to(p(x, 13.0));
            path.line_to(p(16.0 - x, 13.0));
            path.close();
            frame.fill(&path.build(), color);
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
            Icon::ShadowClipping,
            Icon::HighlightClipping,
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
