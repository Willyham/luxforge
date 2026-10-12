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
    Frame, GridBlock, GridCell, GridLayout, GridMetrics, Line, LineKind, MomentHeader, MomentKind,
    PendingRow,
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
    /// The leader of a stack: its badge at the top right, with the disclosure that opens it.
    pub stack: Option<CellStack<'a>>,
    /// A short note over the photograph's bottom left: an original's exposure in an open stack,
    /// that it is outside the view's filter, or why it has no file.
    pub note: Option<&'a str>,
}

/// A stack's badge: its words ("HDR · 3 exposures"), shorter words for a narrow cell ("HDR · 3")
/// and whether the stack is open, which turns its chevron down. A press on it publishes
/// [`crate::ThumbnailGrid::on_stack`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellStack<'a> {
    pub label: &'a str,
    pub short: &'a str,
    pub expanded: bool,
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
const PENDING_TITLE: TextStyle = regular(theme::SIZE_IDENTITY);
const PENDING_DETAIL: TextStyle = regular(theme::SIZE_CAPTION);
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
    /// An action button of this frame (an index into the layout's frames): which of its header's
    /// actions.
    Action(u32, u32),
    /// The action button of the pending row of this block.
    Pending(u32),
    /// The stack badge of this cell.
    Stack(u32),
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
        match line.kind {
            LineKind::Heading(block) => paint_heading(
                layout.block(block),
                line.top - scene.scroll,
                &metrics,
                layout.width(),
                painter,
            ),
            LineKind::Pending(block) => {
                if let GridBlock::Pending(row) = layout.block(block) {
                    let hovered = scene.hover == Hover::Pending(block);
                    let plan = plan_pending(layout, line, row, scene.scroll, painter);
                    paint_pending(&plan, row, hovered, scene.theme, painter);
                }
            }
            LineKind::Cells => {}
        }
    }
    if !lines.is_empty() {
        let frames =
            layout.lines()[lines.start].frames.start..layout.lines()[lines.end - 1].frames.end;
        for index in frames {
            let hovered = match scene.hover {
                Hover::Action(frame, action) if frame == index => Some(action),
                _ => None,
            };
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
        let hovered = scene.hover == Hover::Stack(grid.cell);
        paint_badges(at(grid), view, &metrics, hovered, painter);
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
    /// The buttons kept, left to right.
    pub(crate) actions: Vec<PlannedAction<'h>>,
}

/// One of a header's buttons where it is drawn: which of the header's actions it is, its label,
/// its rectangle and whether it is the primary one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlannedAction<'h> {
    pub(crate) index: u32,
    pub(crate) label: &'h str,
    pub(crate) rect: Rectangle,
    pub(crate) primary: bool,
}

/// Lays out a moment header in `strip` (the frame's top `moment_header` points): the kind icon,
/// title and detail from the left, the pick count, evidence tag and action buttons from the right.
/// The left keeps its whole title while the right parts can give way to it: the tag goes first,
/// then the pick count, then the ordinary actions from the left. Only the primary action then
/// shortens the title, to no less than its icon and an ellipsis, before it goes too. The title and
/// detail then end in an ellipsis where they do not fit.
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
    let whole = title_x + measure.measure(&header.title, MOMENT_TITLE);

    let actions: Vec<(&str, f32, bool)> = header
        .actions
        .iter()
        .map(|action| {
            let width =
                measure.measure(&action.label, MOMENT_ACTION) + 2.0 * theme::MOMENT_ACTION_PADDING;
            (action.label.as_str(), width, action.primary)
        })
        .collect();
    let tag = header.evidence.as_deref().map(|label| {
        let width = measure.measure(label, MOMENT_TAG) + 2.0 * theme::MOMENT_TAG_PADDING;
        (label, width)
    });
    let picked = header
        .picked
        .as_deref()
        .map(|label| (label, measure.measure(label, MOMENT_PICKED)));
    // Which right-hand parts stay: all that leave the left its whole title, dropping the tag, then
    // the pick count, then the ordinary actions from the left; then all that leave it its least,
    // dropping the primary. The buttons are one part, each a margin after what is before it.
    let mut keep_picked = picked.is_some();
    let mut keep_tag = tag.is_some();
    let mut keep_actions = vec![true; actions.len()];
    let limit = |keep_picked: bool, keep_tag: bool, keep_actions: &[bool]| {
        let mut parts = Vec::with_capacity(3);
        if let Some((_, width)) = picked.filter(|_| keep_picked) {
            parts.push(width);
        }
        if let Some((_, width)) = tag.filter(|_| keep_tag) {
            parts.push(width);
        }
        let buttons = actions
            .iter()
            .zip(keep_actions)
            .filter(|(_, kept)| **kept)
            .map(|((_, width, _), _)| width + theme::MOMENT_ACTION_MARGIN)
            .sum::<f32>();
        if buttons > 0.0 {
            parts.push(buttons);
        }
        let count = parts.len();
        // The parts are a gap apart, and the left group a gap and a spring's gap before them.
        right
            - parts.iter().sum::<f32>()
            - (count as f32 - 1.0).max(0.0) * spacing
            - if count > 0 { 2.0 * spacing } else { 0.0 }
    };
    let mut ordinary = (0..actions.len()).filter(|&action| !actions[action].2);
    while limit(keep_picked, keep_tag, &keep_actions) < whole {
        if keep_tag {
            keep_tag = false;
        } else if keep_picked {
            keep_picked = false;
        } else if let Some(action) = ordinary.next() {
            keep_actions[action] = false;
        } else {
            break;
        }
    }
    let mut primary = (0..actions.len()).filter(|&action| actions[action].2);
    while limit(keep_picked, keep_tag, &keep_actions) < least {
        match primary.next() {
            Some(action) => keep_actions[action] = false,
            None => break,
        }
    }
    let limit = limit(keep_picked, keep_tag, &keep_actions);

    let mut x = right;
    let mut planned = Vec::with_capacity(actions.len());
    for index in (0..actions.len())
        .rev()
        .filter(|&action| keep_actions[action])
    {
        let (label, width, primary) = actions[index];
        let rect = Rectangle::new(
            Point::new(x - width, centre - theme::COMPACT_BUTTON_HEIGHT / 2.0),
            Size::new(width, theme::COMPACT_BUTTON_HEIGHT),
        );
        x = rect.x - theme::MOMENT_ACTION_MARGIN;
        planned.push(PlannedAction {
            index: index as u32,
            label,
            rect,
            primary,
        });
    }
    planned.reverse();
    if !planned.is_empty() {
        x -= spacing;
    }
    let tag = tag.filter(|_| keep_tag).map(|(label, width)| {
        let rect = Rectangle::new(
            Point::new(x - width, centre - theme::MOMENT_TAG_HEIGHT / 2.0),
            Size::new(width, theme::MOMENT_TAG_HEIGHT),
        );
        x = rect.x - spacing;
        (label, rect)
    });
    let picked = picked
        .filter(|_| keep_picked)
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
        actions: planned,
    }
}

/// A compact button's background and text colours in `theme`, hovered or not: the accent's for a
/// primary action, the control's otherwise.
fn button_inks(theme: &Theme, primary: bool, hovered: bool) -> (Ink, Ink) {
    let status = if hovered {
        iced::widget::button::Status::Hovered
    } else {
        iced::widget::button::Status::Active
    };
    let style = if primary {
        theme::button_accent(theme, status)
    } else {
        theme::button_control(theme, status)
    };
    let background = match style.background {
        Some(iced::Background::Color(color)) => Ink::Fixed(color),
        _ if primary => Ink::Token(Token::Accent),
        _ => Ink::Token(Token::Control),
    };
    (background, Ink::Fixed(style.text_color))
}

// -- Pending rows -------------------------------------------------------------------------------

/// Where a pending row's parts go: its surface, the kind icon, title and detail from the left, the
/// action button at the right and the progress bar's track along the bottom.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingPlan<'r> {
    pub(crate) rect: Rectangle,
    pub(crate) icon: Point,
    pub(crate) title: Option<(Cow<'r, str>, Point)>,
    pub(crate) detail: Option<(Cow<'r, str>, Point)>,
    pub(crate) action: Option<(&'r str, Rectangle)>,
    pub(crate) track: Rectangle,
}

/// Lays out the pending row `row` on `line`, scrolled into the widget's coordinates. Its action
/// keeps its place; the title, then the detail, end in an ellipsis before it.
pub(crate) fn plan_pending<'r>(
    layout: &GridLayout,
    line: &Line,
    row: &'r PendingRow,
    scroll: f32,
    measure: &mut impl Measure,
) -> PendingPlan<'r> {
    let mut rect = layout.pending_rect(line);
    rect.y -= scroll;
    let metrics = layout.metrics();
    let inset = metrics.moment_padding + theme::MOMENT_HEADER_INSET;
    let spacing = theme::MOMENT_HEADER_SPACING;
    let (left, mut right) = (rect.x + inset, rect.x + rect.width - inset);
    let centre = rect.center_y();
    let action = row.action.as_deref().map(|label| {
        let width = measure.measure(label, MOMENT_ACTION) + 2.0 * theme::MOMENT_ACTION_PADDING;
        let button = Rectangle::new(
            Point::new(right - width, centre - theme::COMPACT_BUTTON_HEIGHT / 2.0),
            Size::new(width, theme::COMPACT_BUTTON_HEIGHT),
        );
        right = button.x - spacing;
        (label, button)
    });
    let title_x = left + theme::MOMENT_ICON_SIZE + spacing;
    let title_width = measure.measure(&row.title, PENDING_TITLE);
    let (title, detail) = if title_width > right - title_x {
        let title = fitted(measure, &row.title, PENDING_TITLE, right - title_x);
        (title.map(|text| (text, Point::new(title_x, centre))), None)
    } else {
        let detail_x = title_x + title_width + spacing;
        let detail = fitted(measure, &row.detail, PENDING_DETAIL, right - detail_x);
        (
            Some((
                Cow::Borrowed(row.title.as_str()),
                Point::new(title_x, centre),
            )),
            detail.map(|text| (text, Point::new(detail_x, centre))),
        )
    };
    let radius = theme::MOMENT_RADIUS;
    PendingPlan {
        rect,
        icon: Point::new(left, centre - theme::MOMENT_ICON_SIZE / 2.0),
        title,
        detail,
        action,
        track: Rectangle::new(
            Point::new(
                rect.x + radius,
                rect.y + rect.height - theme::PENDING_BAR_HEIGHT - theme::BORDER_WIDTH,
            ),
            Size::new(
                (rect.width - 2.0 * radius).max(0.0),
                theme::PENDING_BAR_HEIGHT,
            ),
        ),
    }
}

/// A pending row: a moment's surface and outline, its icon, title and detail, its progress and its
/// button.
fn paint_pending<'a>(
    plan: &PendingPlan<'_>,
    row: &PendingRow,
    hovered: bool,
    theme: &Theme,
    painter: &mut impl Painter<'a>,
) {
    painter.fill(Fill {
        border: Some((theme::BORDER_WIDTH, Ink::Derived(Derived::MomentOutline))),
        ..Fill::flat(
            plan.rect,
            Ink::Derived(Derived::MomentSurface),
            theme::MOMENT_RADIUS,
        )
    });
    let (icon, icon_ink) = if row.failed {
        (Icon::Warning, Ink::Fixed(theme::CLIPPING_HIGHLIGHT))
    } else {
        (Icon::Bracket, Ink::Token(Token::TextIdentity))
    };
    painter.icon(icon, theme::MOMENT_ICON_SIZE, icon_ink, plan.icon);
    let clip = plan.rect;
    if let Some((text, at)) = &plan.title {
        painter.text(
            text,
            *at,
            PENDING_TITLE,
            Ink::Token(Token::TextLabel),
            Align::Left,
            clip,
        );
    }
    if let Some((text, at)) = &plan.detail {
        let ink = if row.failed {
            Ink::Fixed(theme::CLIPPING_HIGHLIGHT)
        } else {
            Ink::Token(Token::TextTertiary)
        };
        painter.text(text, *at, PENDING_DETAIL, ink, Align::Left, clip);
    }
    if !row.failed {
        let radius = theme::PENDING_BAR_HEIGHT / 2.0;
        painter.fill(Fill::flat(plan.track, Ink::Token(Token::Control), radius));
        if let Some(progress) = row.progress.filter(|progress| progress.is_finite()) {
            let done = Rectangle {
                width: (plan.track.width * progress.clamp(0.0, 1.0)).round(),
                ..plan.track
            };
            if done.width > 0.0 {
                painter.fill(Fill::flat(done, Ink::Token(Token::Accent), radius));
            }
        }
    }
    if let Some((text, rect)) = plan.action {
        let (background, ink) = button_inks(theme, false, hovered);
        painter.fill(Fill::flat(rect, background, theme::RADIUS));
        let at = Point::new(rect.x + theme::MOMENT_ACTION_PADDING, rect.center_y());
        painter.text(text, at, MOMENT_ACTION, ink, Align::Left, clip);
    }
}

// -- Moments, continued -------------------------------------------------------------------------

/// The header strip of `frame`, scrolled into the widget's coordinates.
pub(crate) fn header_strip(frame: &Frame, metrics: &GridMetrics, scroll: f32) -> Rectangle {
    let strip = frame.header(metrics);
    Rectangle {
        y: strip.y - scroll,
        ..strip
    }
}

/// A moment's row: its surface and outline, then its header; `hovered` names the action under
/// the pointer.
fn paint_frame<'a>(
    layout: &GridLayout,
    frame: &Frame,
    scroll: f32,
    hovered: Option<u32>,
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
    for action in &plan.actions {
        let (background, ink) = button_inks(theme, action.primary, hovered == Some(action.index));
        painter.fill(Fill::flat(action.rect, background, theme::RADIUS));
        let at = Point::new(
            action.rect.x + theme::MOMENT_ACTION_PADDING,
            action.rect.center_y(),
        );
        painter.text(action.label, at, MOMENT_ACTION, ink, Align::Left, clip);
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

/// One badge: a rounded plate with an icon, unless `text` is empty its text, and a trailing
/// icon (a stack's chevron).
struct Badge<'t> {
    icon: Icon,
    text: &'t str,
    style: TextStyle,
    ink: Ink,
    spacing: f32,
    trailing: Option<Icon>,
}

impl Badge<'_> {
    fn width(&self, measure: &mut impl Measure) -> f32 {
        let text = if self.text.is_empty() {
            0.0
        } else {
            self.spacing + measure.measure(self.text, self.style)
        };
        let trailing = if self.trailing.is_some() {
            self.spacing + theme::CELL_BADGE_ICON_SIZE
        } else {
            0.0
        };
        2.0 * theme::CELL_BADGE_PADDING + theme::CELL_BADGE_ICON_SIZE + text + trailing
    }

    fn paint<'a>(&self, at: Point, width: f32, surface: Ink, painter: &mut impl Painter<'a>) {
        let rect = Rectangle::new(at, Size::new(width, theme::CELL_BADGE_HEIGHT));
        painter.fill(Fill::flat(rect, surface, theme::CELL_BADGE_RADIUS));
        let icon = theme::CELL_BADGE_ICON_SIZE;
        let x = at.x + theme::CELL_BADGE_PADDING;
        let top = rect.center_y() - icon / 2.0;
        painter.icon(self.icon, icon, self.ink, Point::new(x, top));
        if !self.text.is_empty() {
            let at = Point::new(x + icon + self.spacing, rect.center_y());
            painter.text(self.text, at, self.style, self.ink, Align::Left, rect);
        }
        if let Some(trailing) = self.trailing {
            let x = rect.x + rect.width - theme::CELL_BADGE_PADDING - icon;
            painter.icon(trailing, icon, self.ink, Point::new(x, top));
        }
    }
}

/// A stack's badge on a cell at `origin`: its rectangle and the words it shows. It sits at the
/// top right and shows the stack's long words when they fit beside the pick, its short words when
/// those do, and only its icons otherwise.
pub(crate) fn plan_stack<'v>(
    origin: Point,
    view: &CellView<'v>,
    metrics: &GridMetrics,
    measure: &mut impl Measure,
) -> Option<(Rectangle, &'v str)> {
    let stack = view.stack?;
    let inset = metrics.badge_inset;
    let corner = origin.x + metrics.cell.width - inset;
    let mut left = origin.x + inset;
    if view.picked {
        left += theme::CELL_PICK_SIZE + theme::CELL_BADGE_GAP;
    }
    let mut badge = stack_badge(stack, stack.label);
    for text in [stack.label, stack.short, ""] {
        badge.text = text;
        if text.is_empty() || corner - badge.width(measure) >= left {
            break;
        }
    }
    let width = badge.width(measure);
    Some((
        Rectangle::new(
            Point::new(corner - width, origin.y + inset),
            Size::new(width, theme::CELL_BADGE_HEIGHT),
        ),
        badge.text,
    ))
}

fn stack_badge<'t>(stack: CellStack<'_>, text: &'t str) -> Badge<'t> {
    Badge {
        icon: Icon::Bracket,
        text,
        style: BADGE,
        ink: Ink::Fixed(theme::PHOTO_LABEL),
        spacing: theme::CELL_BADGE_SPACING,
        trailing: Some(if stack.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        }),
    }
}

/// A cell's badges over its photograph: the pick at the top left with the catalog badge after it;
/// at the top right a stack's badge, then the frame count and the unavailable badge before it; a
/// note at the bottom left. The catalog badge keeps only its icon when its words would reach the
/// right-hand badges, or pass the photograph's box; the note ends in an ellipsis at the cell's
/// inset. `hovered` lights the stack's badge, which is a button.
fn paint_badges<'a>(
    origin: Point,
    view: &CellView<'a>,
    metrics: &GridMetrics,
    hovered: bool,
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
    let surface = Ink::Fixed(theme::CELL_BADGE_SURFACE);
    if let (Some(stack), Some((rect, text))) =
        (view.stack, plan_stack(origin, view, metrics, painter))
    {
        let surface = if hovered {
            Ink::Fixed(theme::CELL_BADGE_SURFACE_HOVERED)
        } else {
            surface
        };
        stack_badge(stack, text).paint(rect.position(), rect.width, surface, painter);
        right = rect.x - theme::CELL_BADGE_GAP;
    }
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
            trailing: None,
        };
        let width = badge.width(painter);
        right -= width;
        badge.paint(Point::new(right, top), width, surface, painter);
        right -= theme::CELL_BADGE_GAP;
    }
    let unavailable = match view.availability {
        CellAvailability::Available => None,
        CellAvailability::Offline => Some("Offline"),
        CellAvailability::Unreadable => Some("Unreadable"),
    };
    if let Some(text) = unavailable {
        let mut badge = Badge {
            icon: Icon::Warning,
            text,
            style: UNAVAILABLE,
            ink: Ink::Fixed(theme::CLIPPING_HIGHLIGHT),
            spacing: theme::CELL_BADGE_SPACING,
            trailing: None,
        };
        // Beside a stack's badge, only its icon when its word would pass the pick.
        if view.stack.is_some() && right - badge.width(painter) < left {
            badge.text = "";
        }
        let width = badge.width(painter);
        right -= width;
        badge.paint(Point::new(right, top), width, surface, painter);
        right -= theme::CELL_BADGE_GAP;
    }
    if view.in_catalog {
        let mut badge = Badge {
            icon: Icon::Sliders,
            text: "In the catalog",
            style: BADGE,
            ink: Ink::Fixed(theme::PHOTO_LABEL),
            spacing: theme::CELL_CATALOG_BADGE_SPACING,
            trailing: None,
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
        badge.paint(Point::new(left, top), width, surface, painter);
    }
    if let Some(note) = view.note {
        let mut badge = Badge {
            icon: Icon::Bracket,
            text: "",
            style: BADGE,
            ink: Ink::Fixed(theme::PHOTO_LABEL),
            spacing: theme::CELL_BADGE_SPACING,
            trailing: None,
        };
        let room = corner - (origin.x + inset) - badge.width(painter) - badge.spacing;
        let fitted = fitted(painter, note, BADGE, room);
        badge.text = fitted.as_deref().unwrap_or("");
        let width = badge.width(painter);
        let at = Point::new(
            origin.x + inset,
            origin.y + metrics.footer_top - inset - theme::CELL_BADGE_HEIGHT,
        );
        badge.paint(at, width, surface, painter);
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
