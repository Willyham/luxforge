//! The Select workspace's thumbnail grid: a virtualized, grouped grid of cells drawn by one widget.
//!
//! The app holds a [`GridLayout`] (rebuilt only when the view's grouping, the width or the cell size
//! changes), the scroll offset and the viewport size it last knew, and the previews as image
//! handles made once. Each frame it lends them to [`thumbnail_grid`] with a closure that describes
//! one cell as plain data ([`CellView`]); the widget asks the closure only for the cells in the
//! visible range and draws everything — moment rows, headers, headings, cells, photographs, badges
//! and the scrollbar — with renderer primitives, not an element per cell. It holds no
//! authoritative state: a press, a scroll, a moment's action and a new viewport size are published
//! as messages, and the caller decides.
//!
//! What it keeps between frames is bounded and derived: each glyph it draws, tessellated once per
//! icon, size, colour and placement in a frame; the widths of the strings it has measured (at most
//! [`TEXT_WIDTH_CAPACITY`], cleared when full); the keyboard modifiers, the last click, a
//! scrollbar drag and the pointer's hover target.

mod input;
mod layout;
mod paint;

pub(crate) use layout::LineKind;
pub use layout::{
    GridBlock, GridCell, GridDirection, GridHeading, GridHit, GridLayout, GridMetrics,
    MomentHeader, MomentKind,
};
#[cfg(test)]
pub(crate) use paint::asked_cells as paint_for_tests;
pub use paint::{CellAvailability, CellView};

use crate::Icon;
use crate::widgets::icon_button::draw_icon;
use iced::advanced::Renderer as _;
use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::image::Renderer as _;
use iced::advanced::text::{self as core_text, Paragraph as _, Renderer as _};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout as node, mouse, renderer};
use iced::widget::canvas;
use iced::widget::image::Handle;
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{
    Border, Color, Element, Event, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector,
    alignment,
};
use input::Input;
use paint::{Align, Fill, Hover, Measure, Painter, Scene, ScrollbarColours, TextStyle};
use std::cell::RefCell;
use std::collections::HashMap;

/// How many measured strings the grid keeps before it forgets them all and measures again.
pub(crate) const TEXT_WIDTH_CAPACITY: usize = 4096;

/// The layout width a drawn line of text is given: wider than any line the grid draws.
const DRAWN_TEXT_WIDTH: f32 = 4096.0;

/// Which modifiers were held with a press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PressModifiers {
    pub shift: bool,
    /// Command on macOS, Control elsewhere.
    pub command: bool,
}

/// A press on a cell: which cell, its first item and span, the modifiers held, and whether it
/// was the second press of a double click on the same cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPress {
    pub cell: u32,
    pub item: u32,
    pub span: u32,
    pub modifiers: PressModifiers,
    pub double: bool,
}

/// The grid widget; build it with [`thumbnail_grid`].
pub struct ThumbnailGrid<'a, M> {
    layout: &'a GridLayout,
    scroll: f32,
    cell: Box<dyn Fn(GridCell) -> CellView<'a> + 'a>,
    on_press: Option<Box<dyn Fn(GridPress) -> M + 'a>>,
    on_scroll: Option<Box<dyn Fn(f32) -> M + 'a>>,
    on_moment_action: Option<Box<dyn Fn(u32) -> M + 'a>>,
    viewport: Size,
    on_viewport: Option<Box<dyn Fn(Size) -> M + 'a>>,
    width: Length,
    height: Length,
}

/// The grid of `layout` scrolled to `scroll` (content y at the widget's top; the caller holds it),
/// with `cell` describing each visible cell.
pub fn thumbnail_grid<'a, M>(
    layout: &'a GridLayout,
    scroll: f32,
    cell: impl Fn(GridCell) -> CellView<'a> + 'a,
) -> ThumbnailGrid<'a, M> {
    ThumbnailGrid {
        layout,
        scroll,
        cell: Box::new(cell),
        on_press: None,
        on_scroll: None,
        on_moment_action: None,
        viewport: Size::ZERO,
        on_viewport: None,
        width: Length::Fill,
        height: Length::Fill,
    }
}

impl<'a, M> ThumbnailGrid<'a, M> {
    /// Publishes a press on a cell.
    pub fn on_press(mut self, on_press: impl Fn(GridPress) -> M + 'a) -> Self {
        self.on_press = Some(Box::new(on_press));
        self
    }

    /// Publishes the new, clamped scroll offset when the wheel, a trackpad or the scrollbar moves
    /// it. The caller applies it; until it does, further scrolling in the same frame continues from
    /// the offset published.
    pub fn on_scroll(mut self, on_scroll: impl Fn(f32) -> M + 'a) -> Self {
        self.on_scroll = Some(Box::new(on_scroll));
        self
    }

    /// Publishes a press on a moment header's action button, with the moment's number (the view's
    /// moments counted in order, collapsed ones included).
    pub fn on_moment_action(mut self, on_action: impl Fn(u32) -> M + 'a) -> Self {
        self.on_moment_action = Some(Box::new(on_action));
        self
    }

    /// The viewport size the caller last knew (and laid out for).
    pub fn viewport(mut self, viewport: Size) -> Self {
        self.viewport = viewport;
        self
    }

    /// Publishes the widget's real size, once per size, when it differs from [`Self::viewport`], so
    /// the caller can lay out again for a new width. It is checked on every event, including the
    /// redraw request Iced sends each widget before drawing a frame, so the new layout arrives in
    /// the same frame.
    pub fn on_viewport(mut self, on_viewport: impl Fn(Size) -> M + 'a) -> Self {
        self.on_viewport = Some(Box::new(on_viewport));
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }
}

/// Measured string widths, per text style.
#[derive(Default)]
struct TextWidths {
    styles: Vec<((u32, bool), HashMap<String, f32>)>,
    entries: usize,
}

impl Measure for TextWidths {
    fn measure(&mut self, content: &str, style: TextStyle) -> f32 {
        let key = (style.size.to_bits(), style.semibold);
        let index = match self.styles.iter().position(|(other, _)| *other == key) {
            Some(index) => index,
            None => {
                self.styles.push((key, HashMap::new()));
                self.styles.len() - 1
            }
        };
        if let Some(&width) = self.styles[index].1.get(content) {
            return width;
        }
        if self.entries >= TEXT_WIDTH_CAPACITY {
            for (_, widths) in &mut self.styles {
                widths.clear();
            }
            self.entries = 0;
        }
        let width = Paragraph::with_text(text(content, style)).min_width();
        self.styles[index].1.insert(content.to_owned(), width);
        self.entries += 1;
        width
    }
}

type Paragraph = <Renderer as core_text::Renderer>::Paragraph;

fn font(style: TextStyle) -> iced::Font {
    if style.semibold {
        crate::theme::FONT_SEMIBOLD
    } else {
        crate::theme::FONT
    }
}

/// One line of `content` in `style`, as both the measurement and the drawing lay it out.
fn text<C>(content: C, style: TextStyle) -> core_text::Text<C> {
    core_text::Text {
        content,
        bounds: Size::INFINITE,
        size: Pixels(style.size),
        line_height: LineHeight::default(),
        font: font(style),
        align_x: core_text::Alignment::Left,
        align_y: alignment::Vertical::Center,
        shaping: Shaping::default(),
        wrapping: Wrapping::None,
    }
}

/// One glyph's tessellations, each drawn once and placed wherever it is needed.
///
/// Iced's renderer keeps one placement per cached mesh in a frame — a second translation of the
/// same cache moves the first — so the n-th placement of a glyph in a frame draws the n-th of
/// identical caches. The pool grows to the most placements one frame has needed, and each cache is
/// tessellated once.
struct Glyph {
    icon: Icon,
    size: f32,
    color: Color,
    caches: Vec<canvas::Cache>,
    /// How many of the caches this frame has placed.
    placed: usize,
}

/// What the drawing keeps between frames: the measured widths and the glyphs' tessellations.
#[derive(Default)]
struct Caches {
    widths: RefCell<TextWidths>,
    glyphs: RefCell<Vec<Glyph>>,
}

#[derive(Default)]
struct State {
    caches: Caches,
    input: Input,
}

impl<M> Widget<M, Theme, Renderer> for ThumbnailGrid<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &node::Limits,
    ) -> node::Node {
        node::Node::new(limits.resolve(self.width, self.height, Size::ZERO))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let State { caches, input } = tree.state.downcast_mut::<State>();
        let mut widths = caches.widths.borrow_mut();
        let response = {
            let mut publish = |message| shell.publish(message);
            self.respond(
                input,
                &mut *widths,
                event,
                layout.bounds(),
                cursor,
                &mut publish,
            )
        };
        if response.capture {
            shell.capture_event();
        }
        if response.redraw {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        let scroll = self.layout.clamp_scroll(self.scroll, bounds.height);
        let hover = cursor.position_in(bounds).map_or(Hover::None, |position| {
            let mut widths = state.caches.widths.borrow_mut();
            self.hover(position, bounds.size(), scroll, &mut *widths)
        });
        let palette = theme.extended_palette();
        let scene = Scene {
            layout: self.layout,
            scroll,
            size: bounds.size(),
            hover,
            dragging: state.input.drag.is_some(),
            colours: ScrollbarColours {
                rail: palette.background.weak.color,
                scroller: palette.background.strongest.color,
                active: palette.primary.strong.color,
            },
        };
        for glyph in state.caches.glyphs.borrow_mut().iter_mut() {
            glyph.placed = 0;
        }
        renderer.with_layer(clip, |renderer| {
            let mut painter = RendererPainter {
                renderer,
                caches: &state.caches,
                origin: Vector::new(bounds.x, bounds.y),
                clip,
            };
            paint::paint(&scene, &*self.cell, &mut painter);
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let Some(position) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        let state = tree.state.downcast_ref::<State>();
        let scroll = self.layout.clamp_scroll(self.scroll, bounds.height);
        let mut widths = state.caches.widths.borrow_mut();
        match self.hover(position, bounds.size(), scroll, &mut *widths) {
            Hover::Action(_) => mouse::Interaction::Pointer,
            _ => mouse::Interaction::Idle,
        }
    }
}

impl<'a, M: 'a> From<ThumbnailGrid<'a, M>> for Element<'a, M> {
    fn from(grid: ThumbnailGrid<'a, M>) -> Self {
        Element::new(grid)
    }
}

/// Plays the plan on Iced's renderer, in the widget's coordinates.
struct RendererPainter<'r> {
    renderer: &'r mut Renderer,
    caches: &'r Caches,
    origin: Vector,
    clip: Rectangle,
}

impl Measure for RendererPainter<'_> {
    fn measure(&mut self, content: &str, style: TextStyle) -> f32 {
        self.caches.widths.borrow_mut().measure(content, style)
    }
}

impl<'a> Painter<'a> for RendererPainter<'_> {
    fn fill(&mut self, fill: Fill) {
        let (width, color) = fill.border.unwrap_or((0.0, Color::TRANSPARENT));
        self.renderer.fill_quad(
            renderer::Quad {
                bounds: fill.rect + self.origin,
                border: Border {
                    color,
                    width,
                    radius: fill.radius.into(),
                },
                shadow: fill.shadow.unwrap_or_default(),
                ..renderer::Quad::default()
            },
            fill.color,
        );
    }

    fn image(&mut self, handle: &'a Handle, rect: Rectangle, opacity: f32) {
        self.renderer.draw_image(
            iced::advanced::image::Image::new(handle.clone())
                .opacity(opacity)
                .snap(true),
            rect + self.origin,
            self.clip,
        );
    }

    fn image_size(&mut self, handle: &Handle) -> Option<Size> {
        match handle {
            Handle::Rgba { width, height, .. } => Some(Size::new(*width as f32, *height as f32)),
            other => self
                .renderer
                .measure_image(other)
                .map(|size| Size::new(size.width as f32, size.height as f32)),
        }
    }

    fn text(
        &mut self,
        content: &str,
        at: Point,
        style: TextStyle,
        color: Color,
        align: Align,
        clip: Rectangle,
    ) {
        let Some(clip) = (clip + self.origin).intersection(&self.clip) else {
            return;
        };
        let mut line = text(content.to_owned(), style);
        // Finite bounds: the renderer scales them by a matrix, which turns an infinite width into
        // NaN and wraps the line after its first glyph. The plan fits every string first, so the
        // width only has to exceed any one line's.
        line.bounds = Size::new(DRAWN_TEXT_WIDTH, line.line_height.to_absolute(line.size).0);
        line.align_x = match align {
            Align::Left => core_text::Alignment::Left,
            Align::Right => core_text::Alignment::Right,
        };
        self.renderer.fill_text(line, at + self.origin, color, clip);
    }

    fn icon(&mut self, icon: Icon, size: f32, color: Color, at: Point) {
        let geometry = {
            let mut glyphs = self.caches.glyphs.borrow_mut();
            let index = match glyphs
                .iter()
                .position(|glyph| glyph.icon == icon && glyph.size == size && glyph.color == color)
            {
                Some(index) => index,
                None => {
                    glyphs.push(Glyph {
                        icon,
                        size,
                        color,
                        caches: Vec::new(),
                        placed: 0,
                    });
                    glyphs.len() - 1
                }
            };
            let glyph = &mut glyphs[index];
            if glyph.caches.len() == glyph.placed {
                glyph.caches.push(canvas::Cache::new());
            }
            let cache = &glyph.caches[glyph.placed];
            glyph.placed += 1;
            cache.draw(self.renderer, Size::new(size, size), |frame| {
                draw_icon(frame, icon, size, color);
            })
        };
        self.renderer
            .with_translation(Vector::new(at.x, at.y) + self.origin, |renderer| {
                renderer.draw_geometry(geometry);
            });
    }

    fn overlay(&mut self, start: bool) {
        if start {
            self.renderer.start_layer(self.clip);
        } else {
            self.renderer.end_layer();
        }
    }
}
