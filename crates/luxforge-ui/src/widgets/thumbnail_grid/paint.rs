//! Drawing the thumbnail grid: a pure plan over the visible range, played on a [`Painter`].
//!
//! The plan decides every rectangle, image, text and icon from the layout, the scroll offset, the
//! widget's size and the caller's [`CellView`]s; the widget's painter turns them into Iced renderer
//! primitives, and the tests' painter records them. The caller's closure is asked once per cell in
//! the visible range and never for any other.
//!
//! Iced draws a layer's quads, then its meshes, then its images, then its text, whatever order they
//! were issued in, so the badges over the photographs go in a second layer: the plan draws every
//! visible cell's body first, then [`Painter::overlay`] starts the layer for their badges and the
//! scrollbar.

use super::layout::{
    Frame, GridBlock, GridCell, GridLayout, GridMetrics, LineKind, MomentHeader, MomentKind,
};
use crate::widgets::truncated_text::{ELLIPSIS, Fit, fit_one_line};
use crate::{Derived, Ink, Theme, Token};
use crate::{Icon, theme};
use iced::widget::image::Handle;
use iced::{Color, Point, Rectangle, Shadow, Size};
use std::borrow::Cow;

/// What the grid draws in one visible cell: plain data from the app, asked for only while the cell
/// is on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellView<'a> {
    /// The preview to draw, held by the caller. The grid never makes a handle; one made in `view()`
    /// would upload again every frame.
    pub image: Option<&'a Handle>,
    /// The photograph's width over its height from its header, so a preview still loading already
    /// has its shape. Without either, a 3:2 placeholder.
    pub aspect: Option<f32>,
    /// The accent check.
    pub picked: bool,
    /// In the selection, and the active cell (which is drawn selected, with the accent outline).
    pub selected: bool,
    pub active: bool,
    /// Already in the catalog: a badge at the top left.
    pub in_catalog: bool,
    pub availability: CellAvailability,
    /// A collapsed moment's frame count, as a badge at the top right.
    pub count: Option<u32>,
    /// The footer's text: an exposure step, a file name.
    pub label: Option<&'a str>,
    /// Edited: the catalog cell's accent dot at the footer's right.
    pub edited: bool,
}

/// Whether a cell's file can be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CellAvailability {
    #[default]
    Available,
    /// Its volume is not mounted: the preview dimmed, with an Offline badge.
    Offline,
    /// It cannot be decoded: the placeholder, with an Unreadable badge.
    Unreadable,
}

/// A text's size and weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextStyle {
    pub(crate) size: f32,
    pub(crate) semibold: bool,
}

const fn regular(size: f32) -> TextStyle {
    TextStyle {
        size,
        semibold: false,
    }
}

const fn semibold(size: f32) -> TextStyle {
    TextStyle {
        size,
        semibold: true,
    }
}

const DAY_TITLE: TextStyle = semibold(theme::SIZE_TITLE);
const DAY_DETAIL: TextStyle = regular(theme::SIZE_CAPTION);
const CAMERA_TITLE: TextStyle = semibold(theme::SIZE_SECTION_LABEL);
const CAMERA_COUNT: TextStyle = regular(theme::SIZE_SECTION_LABEL);
const MOMENT_TITLE: TextStyle = regular(theme::SIZE_IDENTITY);
const MOMENT_DETAIL: TextStyle = regular(theme::SIZE_CAPTION);
const MOMENT_PICKED: TextStyle = semibold(theme::SIZE_CAPTION);
const MOMENT_TAG: TextStyle = regular(theme::SIZE_MOMENT_TAG);
const MOMENT_ACTION: TextStyle = regular(theme::SIZE_MOMENT_ACTION);
const BADGE: TextStyle = semibold(theme::SIZE_CELL_BADGE);
const UNAVAILABLE: TextStyle = semibold(theme::SIZE_CELL_UNAVAILABLE);

/// A text's baseline sits this many times its size under the centre of its line box: Inter's
/// ascent and descent centred in the line, as the text renderer places them. Texts of two sizes
/// share a baseline by offsetting their centres by it.
const BASELINE_BELOW_CENTRE: f32 = 0.364;

/// Where a text is anchored horizontally; it is always centred vertically on its point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Right,
}

/// A filled, optionally rounded, outlined and shadowed rectangle. The outline is drawn inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Fill {
    pub(crate) rect: Rectangle,
    pub(crate) color: Ink,
    pub(crate) radius: f32,
    pub(crate) border: Option<(f32, Ink)>,
    pub(crate) shadow: Option<Shadow>,
}

impl Fill {
    fn flat(rect: Rectangle, color: impl Into<Ink>, radius: f32) -> Self {
        Self {
            rect,
            color: color.into(),
            radius,
            border: None,
            shadow: None,
        }
    }
}

/// Measures a line of text's width.
pub(crate) trait Measure {
    fn measure(&mut self, content: &str, style: TextStyle) -> f32;
}

/// What the plan draws with. Coordinates are the widget's, from its top left; colours are inks,
/// which the painter resolves in the running theme's palette.
pub(crate) trait Painter<'a>: Measure {
    fn fill(&mut self, fill: Fill);
    fn image(&mut self, handle: &'a Handle, rect: Rectangle, opacity: f32);
    /// A handle's size in pixels, if known.
    fn image_size(&mut self, handle: &Handle) -> Option<Size>;
    /// One line of `content` anchored at `at` (vertically centred), clipped to `clip`.
    fn text(
        &mut self,
        content: &str,
        at: Point,
        style: TextStyle,
        color: Ink,
        align: Align,
        clip: Rectangle,
    );
    /// A named icon `size` points square with its top left at `at`.
    fn icon(&mut self, icon: Icon, size: f32, color: Ink, at: Point);
    /// Starts (`true`) or ends the layer drawn over everything before it.
    fn overlay(&mut self, start: bool);
}

/// What the pointer is over that draws differently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Hover {
    #[default]
    None,
    /// The action button of this frame (an index into the layout's frames).
    Action(u32),
    Scrollbar,
}

/// The scrollbar's colours, from the theme, as the panel scrollbar draws them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScrollbarColours {
    pub(crate) rail: Color,
    pub(crate) scroller: Color,
    pub(crate) active: Color,
}

/// Everything the plan draws from besides the cells' views.
pub(crate) struct Scene<'l> {
    pub(crate) layout: &'l GridLayout,
    /// The content y at the widget's top, already clamped.
    pub(crate) scroll: f32,
    pub(crate) size: Size,
    pub(crate) hover: Hover,
    pub(crate) dragging: bool,
    pub(crate) colours: ScrollbarColours,
    /// The running theme, for the moment action's control style.
    pub(crate) theme: &'l Theme,
}

/// Draws the visible part of the grid: headings, moment frames and headers, the visible cells'
/// bodies, then over them the cells' badges and the scrollbar.
pub(crate) fn paint<'a>(
    scene: &Scene<'_>,
    cell: &dyn Fn(GridCell) -> CellView<'a>,
    painter: &mut impl Painter<'a>,
) {
    let layout = scene.layout;
    let metrics = *layout.metrics();
    let lines = layout.lines_between(scene.scroll, scene.scroll + scene.size.height);
    for line in &layout.lines()[lines.clone()] {
        if let LineKind::Heading(block) = line.kind {
            paint_heading(
                layout.block(block),
                line.top - scene.scroll,
                &metrics,
                layout.width(),
                painter,
            );
        }
    }
    if !lines.is_empty() {
        let frames =
            layout.lines()[lines.start].frames.start..layout.lines()[lines.end - 1].frames.end;
        for index in frames {
            let hovered = scene.hover == Hover::Action(index);
            paint_frame(
                layout,
                &layout.frames()[index as usize],
                scene.scroll,
                hovered,
                scene.theme,
                painter,
            );
        }
    }
    let views: Vec<(GridCell, CellView<'a>)> = layout
        .visible_cells(scene.scroll, scene.size.height, 0.0)
        .map(|index| {
            let grid = layout.cell(index);
            (grid, cell(grid))
        })
        .collect();
    let at = |grid: &GridCell| Point::new(grid.rect.x, grid.rect.y - scene.scroll);
    for (grid, view) in &views {
        paint_cell(at(grid), view, &metrics, painter);
    }
    painter.overlay(true);
    for (grid, view) in &views {
        paint_badges(at(grid), view, &metrics, painter);
    }
    if let Some(bar) = scrollbar(layout.height(), scene.scroll, scene.size) {
        let active = scene.dragging || scene.hover == Hover::Scrollbar;
        let radius = theme::PANEL_SCROLLBAR_WIDTH / 2.0;
        painter.fill(Fill::flat(bar.rail, scene.colours.rail, radius));
        painter.fill(Fill::flat(
            bar.scroller,
            if active {
                scene.colours.active
            } else {
                scene.colours.scroller
            },
            radius,
        ));
    }
    painter.overlay(false);
}

// -- Text ------------------------------------------------------------------------------------------

/// `content` as it fits in `available` points: whole, cut with an ellipsis, or nothing.
fn fitted<'s>(
    measure: &mut impl Measure,
    content: &'s str,
    style: TextStyle,
    available: f32,
) -> Option<Cow<'s, str>> {
    if content.is_empty() {
        return None;
    }
    let mut scratch = String::new();
    match fit_one_line(
        content,
        available,
        |text| measure.measure(text, style),
        &mut scratch,
    ) {
        Fit::Whole => Some(Cow::Borrowed(content)),
        Fit::Prefix(end) => Some(Cow::Owned(format!("{}{ELLIPSIS}", &content[..end]))),
        Fit::Nothing => None,
    }
}

// -- Headings -----------------------------------------------------------------------------------

/// A day or camera heading at `top`: its title and, after it on one baseline, its detail, each
/// ending in an ellipsis rather than passing the content's edge.
fn paint_heading<'a>(
    block: &GridBlock,
    top: f32,
    metrics: &GridMetrics,
    width: f32,
    painter: &mut impl Painter<'a>,
) {
    let (heading, height, centre, title, detail, spacing, colour) = match block {
        GridBlock::Day(heading) => (
            heading,
            metrics.day_heading,
            top + theme::DAY_HEADING_TOP + theme::DAY_HEADING_LINE / 2.0,
            (DAY_TITLE, Ink::Token(Token::TextBright)),
            DAY_DETAIL,
            theme::DAY_HEADING_SPACING,
            Ink::Token(Token::TextTertiary),
        ),
        GridBlock::Camera(heading) => (
            heading,
            metrics.camera_heading,
            top + theme::CAMERA_HEADING_TOP + theme::CAMERA_HEADING_LINE / 2.0,
            (CAMERA_TITLE, Ink::Token(Token::TextTertiary)),
            CAMERA_COUNT,
            theme::CAMERA_HEADING_SPACING,
            Ink::Token(Token::TextTertiary),
        ),
        _ => return,
    };
    let left = metrics.side_inset + theme::HEADING_INSET;
    let right = (width - metrics.side_inset - theme::HEADING_INSET).max(left);
    let clip = Rectangle::new(Point::new(0.0, top), Size::new(width, height));
    let baseline = centre + BASELINE_BELOW_CENTRE * title.0.size;
    let (title_style, title_colour) = title;
    let title_width = painter.measure(&heading.title, title_style);
    if title_width > right - left {
        if let Some(text) = fitted(painter, &heading.title, title_style, right - left) {
            let at = Point::new(left, centre);
            painter.text(&text, at, title_style, title_colour, Align::Left, clip);
        }
        return;
    }
    let at = Point::new(left, centre);
    painter.text(
        &heading.title,
        at,
        title_style,
        title_colour,
        Align::Left,
        clip,
    );
    let x = left + title_width + spacing;
    if let Some(text) = fitted(painter, &heading.detail, detail, right - x) {
        let at = Point::new(x, baseline - BASELINE_BELOW_CENTRE * detail.size);
        painter.text(&text, at, detail, colour, Align::Left, clip);
    }
}

// -- Moments ------------------------------------------------------------------------------------

/// Where a moment header's parts go in its strip.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HeaderPlan<'h> {
    /// The kind icon's top left.
    pub(crate) icon: Point,
    /// Left-aligned at their points.
    pub(crate) title: Option<(Cow<'h, str>, Point)>,
    pub(crate) detail: Option<(Cow<'h, str>, Point)>,
    /// Right-aligned at its point.
    pub(crate) picked: Option<(&'h str, Point)>,
    pub(crate) tag: Option<(&'h str, Rectangle)>,
    pub(crate) action: Option<(&'h str, Rectangle)>,
}

/// Lays out a moment header in `strip` (the frame's top `moment_header` points): the kind icon,
/// title and detail from the left, the pick count, evidence tag and action button from the right.
/// The left keeps at least its icon and an ellipsis; while the right parts leave it less, the tag
/// goes first, then the pick count, then the action. The title and detail then end in an ellipsis
/// where they do not fit.
pub(crate) fn plan_header<'h>(
    header: &'h MomentHeader,
    strip: Rectangle,
    metrics: &GridMetrics,
    measure: &mut impl Measure,
) -> HeaderPlan<'h> {
    let inset = metrics.moment_padding + theme::MOMENT_HEADER_INSET;
    let spacing = theme::MOMENT_HEADER_SPACING;
    let (left, right) = (strip.x + inset, strip.x + strip.width - inset);
    let centre = strip.y + strip.height / 2.0;
    let title_x = left + theme::MOMENT_ICON_SIZE + spacing;
    let least = title_x + measure.measure(ELLIPSIS, MOMENT_TITLE);

    let action = header.action.as_deref().map(|label| {
        let width = measure.measure(label, MOMENT_ACTION) + 2.0 * theme::MOMENT_ACTION_PADDING;
        (label, width)
    });
    let tag = header.evidence.as_deref().map(|label| {
        let width = measure.measure(label, MOMENT_TAG) + 2.0 * theme::MOMENT_TAG_PADDING;
        (label, width)
    });
    let picked = header
        .picked
        .as_deref()
        .map(|label| (label, measure.measure(label, MOMENT_PICKED)));
    // Which right-hand parts stay: all that leave the left its least, dropping the tag, then the
    // pick count, then the action.
    let mut keep = [picked.is_some(), tag.is_some(), action.is_some()];
    let widths = [
        picked.map_or(0.0, |(_, width)| width),
        tag.map_or(0.0, |(_, width)| width),
        action.map_or(0.0, |(_, width)| width + theme::MOMENT_ACTION_MARGIN),
    ];
    let limit = |keep: &[bool; 3]| {
        let (count, sum) = keep
            .iter()
            .zip(widths)
            .filter(|(kept, _)| **kept)
            .fold((0, 0.0), |(count, sum), (_, width)| {
                (count + 1, sum + width)
            });
        // The parts are a gap apart, and the left group a gap and a spring's gap before them.
        right
            - sum
            - (count as f32 - 1.0).max(0.0) * spacing
            - if count > 0 { 2.0 * spacing } else { 0.0 }
    };
    for drop in [1, 0, 2] {
        if limit(&keep) >= least {
            break;
        }
        keep[drop] = false;
    }
    let limit = limit(&keep);

    let mut x = right;
    let action = action.filter(|_| keep[2]).map(|(label, width)| {
        let rect = Rectangle::new(
            Point::new(x - width, centre - theme::COMPACT_BUTTON_HEIGHT / 2.0),
            Size::new(width, theme::COMPACT_BUTTON_HEIGHT),
        );
        x = rect.x - theme::MOMENT_ACTION_MARGIN - spacing;
        (label, rect)
    });
    let tag = tag.filter(|_| keep[1]).map(|(label, width)| {
        let rect = Rectangle::new(
            Point::new(x - width, centre - theme::MOMENT_TAG_HEIGHT / 2.0),
            Size::new(width, theme::MOMENT_TAG_HEIGHT),
        );
        x = rect.x - spacing;
        (label, rect)
    });
    let picked = picked
        .filter(|_| keep[0])
        .map(|(label, _)| (label, Point::new(x, centre)));

    let title_width = measure.measure(&header.title, MOMENT_TITLE);
    let (title, detail) = if title_width > limit - title_x {
        let title = fitted(measure, &header.title, MOMENT_TITLE, limit - title_x);
        (title.map(|text| (text, Point::new(title_x, centre))), None)
    } else {
        let detail_x = title_x + title_width + spacing;
        let detail = fitted(measure, &header.detail, MOMENT_DETAIL, limit - detail_x);
        (
            Some((
                Cow::Borrowed(header.title.as_str()),
                Point::new(title_x, centre),
            )),
            detail.map(|text| (text, Point::new(detail_x, centre))),
        )
    };
    HeaderPlan {
        icon: Point::new(left, centre - theme::MOMENT_ICON_SIZE / 2.0),
        title,
        detail,
        picked,
        tag,
        action,
    }
}

/// The header strip of `frame`, scrolled into the widget's coordinates.
pub(crate) fn header_strip(frame: &Frame, metrics: &GridMetrics, scroll: f32) -> Rectangle {
    let strip = frame.header(metrics);
    Rectangle {
        y: strip.y - scroll,
        ..strip
    }
}

/// A moment's row: its surface and outline, then its header.
fn paint_frame<'a>(
    layout: &GridLayout,
    frame: &Frame,
    scroll: f32,
    hovered: bool,
    theme: &Theme,
    painter: &mut impl Painter<'a>,
) {
    let metrics = layout.metrics();
    let rect = Rectangle {
        y: frame.rect.y - scroll,
        ..frame.rect
    };
    painter.fill(Fill {
        border: Some((theme::BORDER_WIDTH, Ink::Derived(Derived::MomentOutline))),
        ..Fill::flat(
            rect,
            Ink::Derived(Derived::MomentSurface),
            theme::MOMENT_RADIUS,
        )
    });
    let GridBlock::Moment { header, .. } = layout.block(frame.block) else {
        return;
    };
    let strip = header_strip(frame, metrics, scroll);
    let plan = plan_header(header, strip, metrics, painter);
    let icon = match header.kind {
        MomentKind::Burst => Icon::Stack,
        MomentKind::Bracket => Icon::Bracket,
    };
    painter.icon(
        icon,
        theme::MOMENT_ICON_SIZE,
        Ink::Token(Token::TextIdentity),
        plan.icon,
    );
    let clip = strip;
    if let Some((text, at)) = &plan.title {
        painter.text(
            text,
            *at,
            MOMENT_TITLE,
            Ink::Token(Token::TextLabel),
            Align::Left,
            clip,
        );
    }
    if let Some((text, at)) = &plan.detail {
        painter.text(
            text,
            *at,
            MOMENT_DETAIL,
            Ink::Token(Token::TextTertiary),
            Align::Left,
            clip,
        );
    }
    if let Some((text, at)) = plan.picked {
        painter.text(
            text,
            at,
            MOMENT_PICKED,
            Ink::Token(Token::Accent),
            Align::Right,
            clip,
        );
    }
    if let Some((text, rect)) = plan.tag {
        painter.fill(Fill::flat(
            rect,
            Ink::Token(Token::Control),
            theme::MOMENT_TAG_RADIUS,
        ));
        let at = Point::new(rect.x + theme::MOMENT_TAG_PADDING, rect.center_y());
        painter.text(
            text,
            at,
            MOMENT_TAG,
            Ink::Token(Token::TextSecondary),
            Align::Left,
            clip,
        );
    }
    if let Some((text, rect)) = plan.action {
        let status = if hovered {
            iced::widget::button::Status::Hovered
        } else {
            iced::widget::button::Status::Active
        };
        let style = theme::button_control(theme, status);
        let color = match style.background {
            Some(iced::Background::Color(color)) => Ink::Fixed(color),
            _ => Ink::Token(Token::Control),
        };
        painter.fill(Fill::flat(rect, color, theme::RADIUS));
        let at = Point::new(rect.x + theme::MOMENT_ACTION_PADDING, rect.center_y());
        let ink = Ink::Fixed(style.text_color);
        painter.text(text, at, MOMENT_ACTION, ink, Align::Left, clip);
    }
}

// -- Cells --------------------------------------------------------------------------------------

/// The photograph's rectangle in a cell at `origin`: `aspect` fitted within the metrics' largest
/// photograph and centred in the image box, on whole points.
pub(crate) fn photo_rect(origin: Point, aspect: f32, metrics: &GridMetrics) -> Rectangle {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.5
    };
    let most = metrics.image_max;
    let size = if aspect >= most.width / most.height {
        Size::new(most.width, most.width / aspect)
    } else {
        Size::new(most.height * aspect, most.height)
    };
    let image = metrics.image;
    Rectangle {
        x: (origin.x + image.x + (image.width - size.width) / 2.0).round(),
        y: (origin.y + image.y + (image.height - size.height) / 2.0).round(),
        width: size.width.round().max(1.0),
        height: size.height.round().max(1.0),
    }
}

/// A cell's body: its ground (and the active outline), the photograph or its placeholder, and the
/// footer's label and edited dot.
fn paint_cell<'a>(
    origin: Point,
    view: &CellView<'a>,
    metrics: &GridMetrics,
    painter: &mut impl Painter<'a>,
) {
    let rect = Rectangle::new(origin, metrics.cell);
    let ground = if view.selected || view.active {
        Ink::Derived(Derived::CellSelected)
    } else {
        Ink::Derived(Derived::CellSurface)
    };
    painter.fill(Fill {
        border: view
            .active
            .then_some((theme::CELL_ACTIVE_OUTLINE, Ink::Token(Token::Accent))),
        ..Fill::flat(rect, ground, theme::RADIUS)
    });
    let image = view
        .image
        .filter(|_| view.availability != CellAvailability::Unreadable);
    let size = image.and_then(|handle| painter.image_size(handle));
    let aspect = size
        .map(|size| size.width / size.height)
        .or(view.aspect)
        .unwrap_or(1.5);
    let photo = photo_rect(origin, aspect, metrics);
    match image {
        Some(handle) => {
            let opacity = if view.availability == CellAvailability::Offline {
                theme::CELL_OFFLINE_OPACITY
            } else {
                1.0
            };
            let mut shadow = theme::CELL_IMAGE_SHADOW;
            shadow.color.a *= opacity;
            // The shadow's rectangle is the cell's ground, so a dimmed photograph shows the cell
            // through it, as the board's does.
            painter.fill(Fill {
                shadow: Some(shadow),
                ..Fill::flat(photo, ground, 0.0)
            });
            painter.image(handle, photo, opacity);
        }
        None => painter.fill(Fill::flat(
            photo,
            Ink::Derived(Derived::CellPlaceholder),
            0.0,
        )),
    }
    let footer = Rectangle::new(
        Point::new(origin.x, origin.y + metrics.footer_top),
        Size::new(metrics.cell.width, metrics.footer_height),
    );
    let centre = footer.center_y();
    let mut right = footer.x + footer.width - metrics.footer_inset;
    if view.edited {
        let dot = theme::DOT_SIZE;
        let rect = Rectangle::new(
            Point::new(right - dot, centre - dot / 2.0),
            Size::new(dot, dot),
        );
        painter.fill(Fill::flat(rect, Ink::Token(Token::Accent), dot / 2.0));
        right = rect.x - theme::CELL_FOOTER_SPACING;
    }
    if let Some(label) = view.label {
        let style = regular(metrics.label_size);
        let left = footer.x + metrics.footer_inset;
        if let Some(text) = fitted(painter, label, style, right - left) {
            let at = Point::new(left, centre);
            painter.text(
                &text,
                at,
                style,
                Ink::Token(Token::TextTertiary),
                Align::Left,
                footer,
            );
        }
    }
}

/// One badge: a rounded plate with an icon and, unless `text` is empty, its text.
struct Badge<'t> {
    icon: Icon,
    text: &'t str,
    style: TextStyle,
    ink: Ink,
    spacing: f32,
}

impl Badge<'_> {
    fn width(&self, measure: &mut impl Measure) -> f32 {
        let text = if self.text.is_empty() {
            0.0
        } else {
            self.spacing + measure.measure(self.text, self.style)
        };
        2.0 * theme::CELL_BADGE_PADDING + theme::CELL_BADGE_ICON_SIZE + text
    }

    fn paint<'a>(&self, at: Point, width: f32, painter: &mut impl Painter<'a>) {
        let rect = Rectangle::new(at, Size::new(width, theme::CELL_BADGE_HEIGHT));
        painter.fill(Fill::flat(
            rect,
            theme::CELL_BADGE_SURFACE,
            theme::CELL_BADGE_RADIUS,
        ));
        let icon = theme::CELL_BADGE_ICON_SIZE;
        let x = at.x + theme::CELL_BADGE_PADDING;
        painter.icon(
            self.icon,
            icon,
            self.ink,
            Point::new(x, rect.center_y() - icon / 2.0),
        );
        if !self.text.is_empty() {
            let at = Point::new(x + icon + self.spacing, rect.center_y());
            painter.text(self.text, at, self.style, self.ink, Align::Left, rect);
        }
    }
}

/// A cell's badges over its photograph: the pick at the top left with the catalog badge after it,
/// the frame count at the top right with the unavailable badge before it. The catalog badge keeps
/// only its icon when its words would reach the right-hand badges, or pass the photograph's box.
fn paint_badges<'a>(
    origin: Point,
    view: &CellView<'a>,
    metrics: &GridMetrics,
    painter: &mut impl Painter<'a>,
) {
    let inset = metrics.badge_inset;
    let top = origin.y + inset;
    let mut left = origin.x + inset;
    if view.picked {
        let (disc, ring) = (theme::CELL_PICK_SIZE, theme::CELL_PICK_RING_WIDTH);
        let outer = Rectangle::new(
            Point::new(left - ring, top - ring),
            Size::new(disc + 2.0 * ring, disc + 2.0 * ring),
        );
        painter.fill(Fill::flat(outer, theme::CELL_PICK_RING, disc / 2.0 + ring));
        let rect = Rectangle::new(Point::new(left, top), Size::new(disc, disc));
        painter.fill(Fill::flat(rect, Ink::Token(Token::Accent), disc / 2.0));
        let check = theme::CELL_CHECK_SIZE;
        painter.icon(
            Icon::Check,
            check,
            Ink::Token(Token::AccentInk),
            Point::new(rect.center_x() - check / 2.0, rect.center_y() - check / 2.0),
        );
        left += disc + theme::CELL_BADGE_GAP;
    }
    let corner = origin.x + metrics.cell.width - inset;
    let mut right = corner;
    let mut count_text = String::new();
    if let Some(count) = view.count {
        use std::fmt::Write as _;
        let _ = write!(count_text, "{count}");
        let badge = Badge {
            icon: Icon::Stack,
            text: &count_text,
            style: BADGE,
            ink: Ink::Fixed(theme::PHOTO_LABEL),
            spacing: theme::CELL_BADGE_SPACING,
        };
        let width = badge.width(painter);
        right -= width;
        badge.paint(Point::new(right, top), width, painter);
        right -= theme::CELL_BADGE_GAP;
    }
    let unavailable = match view.availability {
        CellAvailability::Available => None,
        CellAvailability::Offline => Some("Offline"),
        CellAvailability::Unreadable => Some("Unreadable"),
    };
    if let Some(text) = unavailable {
        let badge = Badge {
            icon: Icon::Warning,
            text,
            style: UNAVAILABLE,
            ink: Ink::Fixed(theme::CLIPPING_HIGHLIGHT),
            spacing: theme::CELL_BADGE_SPACING,
        };
        let width = badge.width(painter);
        right -= width;
        badge.paint(Point::new(right, top), width, painter);
        right -= theme::CELL_BADGE_GAP;
    }
    if view.in_catalog {
        let mut badge = Badge {
            icon: Icon::Sliders,
            text: "In the catalog",
            style: BADGE,
            ink: Ink::Fixed(theme::PHOTO_LABEL),
            spacing: theme::CELL_CATALOG_BADGE_SPACING,
        };
        // With nothing on the right, the words may reach the photograph's box, not only the
        // corner's inset: after a pick, at the default size, they need about 1 pt more.
        let limit = if right == corner {
            origin.x + metrics.cell.width - metrics.image.x
        } else {
            right
        };
        if left + badge.width(painter) > limit {
            badge.text = "";
        }
        let width = badge.width(painter);
        badge.paint(Point::new(left, top), width, painter);
    }
}

// -- The scrollbar ------------------------------------------------------------------------------

/// The scrollbar's rail, its scroller and the strip at the right edge that takes the pointer for it,
/// in the widget's coordinates; `None` when the content fits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scrollbar {
    pub(crate) rail: Rectangle,
    pub(crate) scroller: Rectangle,
    pub(crate) zone: Rectangle,
    /// How far the scroller moves, and the content's scroll range it maps onto.
    pub(crate) travel: f32,
    pub(crate) range: f32,
}

impl Scrollbar {
    /// The scroll offset that puts the scroller's top at `y`.
    pub(crate) fn scroll_at(&self, y: f32) -> f32 {
        if self.travel <= 0.0 {
            return 0.0;
        }
        (y / self.travel).clamp(0.0, 1.0) * self.range
    }
}

pub(crate) fn scrollbar(content: f32, scroll: f32, size: Size) -> Option<Scrollbar> {
    if size.height <= 0.0 || content <= size.height {
        return None;
    }
    let (width, margin) = (theme::PANEL_SCROLLBAR_WIDTH, theme::PANEL_SCROLLBAR_MARGIN);
    let rail = Rectangle::new(
        Point::new(size.width - margin - width, 0.0),
        Size::new(width, size.height),
    );
    let length = (size.height * size.height / content)
        .max(theme::THUMB_GRID_SCROLLER_MIN)
        .min(size.height);
    let travel = size.height - length;
    let range = content - size.height;
    let y = travel * (scroll / range).clamp(0.0, 1.0);
    Some(Scrollbar {
        rail,
        scroller: Rectangle::new(Point::new(rail.x, y), Size::new(width, length)),
        zone: Rectangle::new(
            Point::new(size.width - theme::THUMB_GRID_SCROLLBAR_HIT, 0.0),
            Size::new(theme::THUMB_GRID_SCROLLBAR_HIT, size.height),
        ),
        travel,
        range,
    })
}

/// Plays the plan on a painter that draws nothing, and returns the cells the closure was asked for.
#[cfg(test)]
pub(crate) fn asked_cells<'a>(
    layout: &GridLayout,
    scroll: f32,
    size: Size,
    cell: impl Fn(GridCell) -> CellView<'a>,
) -> Vec<u32> {
    struct Nothing;
    impl Measure for Nothing {
        fn measure(&mut self, content: &str, style: TextStyle) -> f32 {
            content.chars().count() as f32 * style.size * 0.5
        }
    }
    impl Painter<'_> for Nothing {
        fn fill(&mut self, _: Fill) {}
        fn image(&mut self, _: &Handle, _: Rectangle, _: f32) {}
        fn image_size(&mut self, handle: &Handle) -> Option<Size> {
            match handle {
                Handle::Rgba { width, height, .. } => {
                    Some(Size::new(*width as f32, *height as f32))
                }
                _ => None,
            }
        }
        fn text(&mut self, _: &str, _: Point, _: TextStyle, _: Ink, _: Align, _: Rectangle) {}
        fn icon(&mut self, _: Icon, _: f32, _: Ink, _: Point) {}
        fn overlay(&mut self, _: bool) {}
    }
    let asked = std::cell::RefCell::new(Vec::new());
    let scene = Scene {
        layout,
        scroll,
        size,
        hover: Hover::None,
        dragging: false,
        colours: ScrollbarColours {
            rail: Color::BLACK,
            scroller: Color::BLACK,
            active: Color::BLACK,
        },
        theme: &Theme::luxforge_dark(),
    };
    let record = |grid: GridCell| {
        asked.borrow_mut().push(grid.cell);
        cell(grid)
    };
    paint(&scene, &record, &mut Nothing);
    asked.into_inner()
}

#[cfg(test)]
mod tests;
