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
    Target,
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
    Return,
}

impl Icon {
    /// Every icon with its name, in the order the gallery's icon board lists them.
    pub const NAMED: [(&'static str, Icon); 27] = [
        ("rotate-left", Self::RotateLeft),
        ("rotate-right", Self::RotateRight),
        ("flip", Self::Flip),
        ("mirror", Self::Mirror),
        ("crop", Self::Crop),
        ("picker", Self::Picker),
        ("target", Self::Target),
        ("reset", Self::Reset),
        ("plus", Self::Plus),
        ("minus", Self::Minus),
        ("lock", Self::Lock),
        ("swap", Self::Swap),
        ("guide", Self::Guide),
        ("pointer", Self::Pointer),
        ("versions", Self::Versions),
        ("undo", Self::Undo),
        ("redo", Self::Redo),
        ("before", Self::Before),
        ("after", Self::After),
        ("clipping", Self::Clipping),
        ("shadow-clipping", Self::ShadowClipping),
        ("highlight-clipping", Self::HighlightClipping),
        ("state-panel", Self::StatePanel),
        ("tools-panel", Self::ToolsPanel),
        ("chevron-down", Self::ChevronDown),
        ("chevron-right", Self::ChevronRight),
        ("return", Self::Return),
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMED
            .iter()
            .find(|(named, _)| *named == name)
            .map(|&(_, icon)| icon)
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
/// its reset: a [`theme::HEADER_ICON_SIZE`] icon in the secondary text colour, or the accent when
/// selected, inside a [`theme::HEADER_BUTTON_SIZE`] square.
pub fn header_icon_button<'a, M: Clone + 'a>(
    model: &IconButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let color = match (model.enabled, model.selected) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => theme::ACCENT,
        (true, false) => theme::TEXT_SECONDARY,
    };
    // A header's selected action (a locked ratio) reads by its accent ink alone, with no surface.
    let bare = IconButtonModel {
        selected: false,
        ..model.clone()
    };
    sized_icon_button(
        &bare,
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
        .padding(theme::TOOLTIP_PADDING)
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
    arc_points(8.15, 8.2, 4.15, -115.0, 165.0)
}

/// The quarter-turn arrow's head: the same L as [`ARROW_HEAD`], on the larger circle.
const QUARTER_TURN_HEAD: [(f32, f32); 3] = [(3.2, 2.4), (3.2, 6.2), (6.9, 6.2)];

/// The quarter-turn arrow's arc: centre (8, 8), radius 4.6, from −125° clockwise round to 186°,
/// as the Transforms reference draws it, 11 pt of ink in a 16 pt icon.
fn quarter_turn_arc() -> Vec<(f32, f32)> {
    arc_points(8.0, 8.0, 4.6, -125.0, 186.0)
}

fn arc_points(cx: f32, cy: f32, radius: f32, start: f32, end: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 24;
    let (start, end) = (start.to_radians(), end.to_radians());
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
            // clockwise icons are the mirror image. A quarter turn is the transform itself rather
            // than a header's small reset, so it draws the larger arrow the Transforms row does.
            let mirror = matches!(icon, Icon::RotateRight | Icon::Redo);
            let turn = matches!(icon, Icon::RotateLeft | Icon::RotateRight);
            let x = |x: f32| if mirror { 16.0 - x } else { x };
            let (arc, head) = if turn {
                (quarter_turn_arc(), &QUARTER_TURN_HEAD)
            } else {
                (circular_arrow_arc(), &ARROW_HEAD)
            };
            let points: Vec<(f32, f32)> = arc.into_iter().map(|(px, py)| (x(px), py)).collect();
            poly(frame, &points);
            let head: Vec<(f32, f32)> = head.iter().map(|&(px, py)| (x(px), py)).collect();
            poly(frame, &head);
        }
        // A reflection: the axis, with an open chevron on either side pointing away from it.
        Icon::Mirror => {
            line(frame, (8.0, 1.5), (8.0, 14.5));
            poly(frame, &[(5.25, 5.0), (2.25, 8.0), (5.25, 11.0)]);
            poly(frame, &[(10.75, 5.0), (13.75, 8.0), (10.75, 11.0)]);
        }
        Icon::Flip => {
            line(frame, (1.75, 8.0), (14.25, 8.0));
            poly(frame, &[(5.0, 5.25), (8.0, 2.25), (11.0, 5.25)]);
            poly(frame, &[(5.0, 10.75), (8.0, 13.75), (11.0, 10.75)]);
        }
        // The Return key: down from the top right, then left to an arrowhead.
        Icon::Return => {
            poly(frame, &[(12.5, 4.0), (12.5, 8.5), (3.5, 8.5)]);
            poly(frame, &[(6.0, 6.0), (3.5, 8.5), (6.0, 11.0)]);
        }
        // Two overlapping corners, as the crop reference draws them: one down and right, one
        // right and down.
        Icon::Crop => {
            poly(frame, &[(4.6, 0.6), (4.6, 11.4), (15.4, 11.4)]);
            poly(frame, &[(0.6, 4.6), (11.4, 4.6), (11.4, 15.4)]);
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
        // A crosshair, as raw.png draws As shot: a small ring with a tick out from it on each side.
        Icon::Target => {
            frame.stroke(&canvas::Path::circle(p(8.0, 8.0), 2.5 * s), stroke);
            line(frame, (8.0, 1.4), (8.0, 4.4));
            line(frame, (8.0, 11.6), (8.0, 14.6));
            line(frame, (1.4, 8.0), (4.4, 8.0));
            line(frame, (11.6, 8.0), (14.6, 8.0));
        }
        // A padlock: a rounded body and a round shackle.
        Icon::Lock => {
            frame.stroke(
                &canvas::Path::rounded_rectangle(
                    p(3.2, 7.2),
                    iced::Size::new(9.6 * s, 7.4 * s),
                    (1.6 * s).into(),
                ),
                stroke,
            );
            let mut shackle = canvas::path::Builder::new();
            shackle.move_to(p(5.4, 7.2));
            shackle.line_to(p(5.4, 5.0));
            shackle.arc(canvas::path::Arc {
                center: p(8.0, 5.0),
                radius: 2.6 * s,
                start_angle: iced::Radians(std::f32::consts::PI),
                end_angle: iced::Radians(2.0 * std::f32::consts::PI),
            });
            shackle.line_to(p(10.6, 7.2));
            frame.stroke(&shackle.build(), stroke);
        }
        // Two opposed arrows, each with one barb, as the crop reference draws the swap.
        Icon::Swap => {
            poly(frame, &[(2.5, 5.5), (13.5, 5.5), (10.5, 2.8)]);
            poly(frame, &[(13.5, 10.5), (2.5, 10.5), (5.5, 13.2)]);
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
        for (_, icon) in Icon::NAMED {
            let _: Element<'_, ()> = super::icon(icon, 12.0, theme::TEXT_PRIMARY);
            let _: Element<'_, ()> = super::icon(icon, 16.0, theme::TEXT_PRIMARY);
        }
    }

    #[test]
    fn every_name_is_distinct_and_resolves_to_its_icon() {
        for (index, (name, icon)) in Icon::NAMED.iter().enumerate() {
            assert_eq!(Icon::from_name(name), Some(*icon));
            assert!(
                Icon::NAMED[..index]
                    .iter()
                    .all(|(other, i)| other != name && i != icon)
            );
        }
        assert_eq!(Icon::from_name("unknown"), None);
    }
}
