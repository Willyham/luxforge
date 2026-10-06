//! The thumbnail grid's layout: pure geometry from the view's grouping, computed once per layout
//! change and queried in O(log n).
//!
//! The input is the view's structure as blocks — day and camera headings, moments with their frame
//! counts and runs of single frames — which consume the view's items (positions `0..n`) in order.
//! Within a day, a camera, or the whole view when it is not grouped, blocks flow into lines left to
//! right with a gap between them, as the boards' `.wrap` does. A single frame is one cell. A moment
//! is a framed block: a header over its cells in a row. A moment that does not fit in what is left
//! of a line starts a new line; one wider than the content takes whole lines of its own, its cells
//! wrapping inside one frame, and the block after it starts a new line. A collapsed moment is one
//! ordinary cell covering all its frames. Headings are lines of their own.
//!
//! A line that holds a moment is the moment's header, a cell and the moment's bottom padding tall;
//! the singles on it align their tops with the moment's cells. The boards never put a single beside
//! a moment on one line, so this is the layout's own choice: it keeps every cell on a line on one
//! row, so moving up and down and reading across stay on a straight line.
//!
//! Storage is proportional to the cells, lines and headings: a cell is its first item, its span,
//! its x and its line; a line is its band, its cells' top and index ranges into the cells and the
//! moment frames. Lines are sorted and do not overlap, so the visible range, a hit test and an
//! item's cell are binary searches, and nothing done per frame is proportional to the item count.

use crate::theme;
use iced::{Point, Rectangle, Size};
use std::ops::Range;

/// A day or camera heading: its title and the detail after it ("Saturday 12 September 2026",
/// "618 photographs · 9 picked"; "Leica Q2", "318").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridHeading {
    pub title: String,
    pub detail: String,
}

/// What a moment is, which picks its header's icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MomentKind {
    Burst,
    Bracket,
}

/// A moment row's header: its kind, its title and detail on the left ("Burst", "6 frames in
/// 1.4 s"), and on the right how many are picked, the evidence of a bracket as a neutral tag
/// ("from metadata", "from previews") and a small action button ("Pick all 3"). Text that does not
/// fit ends in an ellipsis; when even the right-hand parts do not fit, the tag goes first, then the
/// pick count, then the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentHeader {
    pub kind: MomentKind,
    pub title: String,
    pub detail: String,
    pub evidence: Option<String>,
    pub picked: Option<String>,
    pub action: Option<String>,
}

/// One piece of the view's structure, in order. Moments and singles consume items; headings
/// consume none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridBlock {
    Day(GridHeading),
    /// A camera heading. Its title is drawn capitalised, as a section label is.
    Camera(GridHeading),
    /// A moment of `frames` consecutive items. Collapsed, it is one ordinary cell covering them all,
    /// whose item is the moment's first.
    Moment {
        header: MomentHeader,
        frames: u32,
        collapsed: bool,
    },
    /// This many single frames, one cell each.
    Singles(u32),
}

/// The grid's sizes. Each preset takes the cell width a size slider asks for; the photograph's
/// box grows by the width change and its height, and the cell's, by the preset's image ratio, while
/// padding, the footer, the badges and the gaps keep their sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    /// A cell's size.
    pub cell: Size,
    /// The photograph's box inside a cell; the photograph is fitted within `image_max` and centred
    /// in it.
    pub image: Rectangle,
    pub image_max: Size,
    /// The footer's top and height inside a cell, the label's inset and size.
    pub footer_top: f32,
    pub footer_height: f32,
    pub footer_inset: f32,
    pub label_size: f32,
    /// How far the badges sit from a cell's corner.
    pub badge_inset: f32,
    /// Between cells, blocks and lines.
    pub gap: f32,
    /// A moment row's padding at its sides and bottom, and its header's height.
    pub moment_padding: f32,
    pub moment_header: f32,
    /// A day heading's and a camera heading's line.
    pub day_heading: f32,
    pub camera_heading: f32,
    /// The content's inset at the sides and under the last line, and over a first line of cells.
    pub side_inset: f32,
    pub bottom_inset: f32,
    pub top_inset: f32,
}

impl GridMetrics {
    /// The Select grid's cells (the event board's 136 × 122 pt `.cc`) at `cell_width`.
    pub fn select(cell_width: f32) -> Self {
        Self::preset(
            cell_width,
            theme::CELL_SIZE,
            theme::CELL_IMAGE,
            theme::CELL_IMAGE_MAX,
            theme::CELL_FOOTER_TOP,
            theme::CELL_FOOTER_HEIGHT,
            theme::CELL_FOOTER_INSET,
            theme::SIZE_CELL_LABEL,
            theme::CELL_BADGE_INSET,
        )
    }

    /// The catalog's larger cells (the catalog board's 168 × 176 pt `.cell`) at `cell_width`.
    pub fn catalog(cell_width: f32) -> Self {
        Self::preset(
            cell_width,
            theme::CATALOG_CELL_SIZE,
            theme::CATALOG_CELL_IMAGE,
            theme::CATALOG_CELL_IMAGE_MAX,
            theme::CATALOG_CELL_FOOTER_TOP,
            theme::CATALOG_CELL_FOOTER_HEIGHT,
            theme::CATALOG_CELL_FOOTER_INSET,
            theme::SIZE_CATALOG_CELL_LABEL,
            theme::CATALOG_CELL_BADGE_INSET,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn preset(
        cell_width: f32,
        cell: Size,
        image: Rectangle,
        image_max: Size,
        footer_top: f32,
        footer_height: f32,
        footer_inset: f32,
        label_size: f32,
        badge_inset: f32,
    ) -> Self {
        let width = if cell_width.is_finite() {
            cell_width.max(theme::CELL_MIN_WIDTH)
        } else {
            cell.width
        };
        let wider = width - cell.width;
        let taller = wider * image_max.height / image_max.width;
        Self {
            cell: Size::new(width, cell.height + taller),
            image: Rectangle {
                width: image.width + wider,
                height: image.height + taller,
                ..image
            },
            image_max: Size::new(image_max.width + wider, image_max.height + taller),
            footer_top: footer_top + taller,
            footer_height,
            footer_inset,
            label_size,
            badge_inset,
            gap: theme::THUMB_GRID_GAP,
            moment_padding: theme::MOMENT_PADDING,
            moment_header: theme::MOMENT_HEADER_HEIGHT,
            day_heading: theme::DAY_HEADING_TOP
                + theme::DAY_HEADING_LINE
                + theme::DAY_HEADING_BOTTOM,
            camera_heading: theme::CAMERA_HEADING_TOP
                + theme::CAMERA_HEADING_LINE
                + theme::CAMERA_HEADING_BOTTOM,
            side_inset: theme::THUMB_GRID_SIDE_INSET,
            bottom_inset: theme::THUMB_GRID_BOTTOM_INSET,
            top_inset: theme::THUMB_GRID_TOP_INSET,
        }
    }

    /// A moment row's width for `frames` cells in a row.
    fn moment_width(&self, frames: u32) -> f32 {
        2.0 * self.moment_padding + self.row_width(frames)
    }

    /// `cells` cells side by side with gaps between them.
    fn row_width(&self, cells: u32) -> f32 {
        if cells == 0 {
            0.0
        } else {
            cells as f32 * self.cell.width + (cells - 1) as f32 * self.gap
        }
    }
}

impl Default for GridMetrics {
    fn default() -> Self {
        Self::select(theme::CELL_SIZE.width)
    }
}

/// One cell: its index in reading order, the first item it shows, how many items it covers (a
/// collapsed moment's frame count, otherwise 1) and its rectangle in content coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridCell {
    pub cell: u32,
    pub item: u32,
    pub span: u32,
    pub rect: Rectangle,
}

/// What a point in the content is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridHit {
    /// A cell, with its first item and span.
    Cell { cell: u32, item: u32, span: u32 },
    /// A moment row's header. `moment` counts the view's moments in order, collapsed ones
    /// included. The header's action button is inside it; the widget, which measures the header's
    /// text, tells a press on the button apart ([`crate::ThumbnailGrid::on_moment_action`]).
    MomentHeader { moment: u32 },
}

/// A direction to move the active cell in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A cell's place: its first item, span, x and line.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Slot {
    item: u32,
    span: u32,
    x: f32,
    line: u32,
}

/// What a line holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// Cells, from singles, collapsed moments and moment rows.
    Cells,
    /// The heading of this block.
    Heading(u32),
}

/// One horizontal band of the content: a heading, or a line of cells. `top..bottom` is the band;
/// its cells' tops are at `cell_top`, under the header of a moment row that starts on it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub(crate) top: f32,
    pub(crate) bottom: f32,
    pub(crate) cell_top: f32,
    pub(crate) cells: Range<u32>,
    /// The moment frames the band crosses.
    pub(crate) frames: Range<u32>,
    pub(crate) kind: LineKind,
}

/// An expanded moment's frame: which moment it is, its block and its rectangle, header included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Frame {
    pub(crate) moment: u32,
    pub(crate) block: u32,
    pub(crate) rect: Rectangle,
}

impl Frame {
    /// The header strip at the frame's top.
    pub(crate) fn header(&self, metrics: &GridMetrics) -> Rectangle {
        Rectangle {
            height: metrics.moment_header,
            ..self.rect
        }
    }
}

/// The grid's geometry for one view, one width and one cell size.
///
/// The app builds it when the view's grouping, the width or the cell size changes, holds it, and
/// lends it to [`crate::thumbnail_grid`] each frame; nothing in it changes while scrolling.
#[derive(Debug, Clone)]
pub struct GridLayout {
    blocks: Vec<GridBlock>,
    metrics: GridMetrics,
    width: f32,
    height: f32,
    items: u32,
    cells: Vec<Slot>,
    lines: Vec<Line>,
    frames: Vec<Frame>,
}

impl GridLayout {
    /// Lays out `blocks` at `width` (the grid's whole width, insets included).
    pub fn new(mut blocks: Vec<GridBlock>, metrics: GridMetrics, width: f32) -> Self {
        for block in &mut blocks {
            if let GridBlock::Camera(heading) = block {
                heading.title = heading.title.to_uppercase();
            }
        }
        let mut layout = Self {
            blocks,
            metrics,
            width: 0.0,
            height: 0.0,
            items: 0,
            cells: Vec::new(),
            lines: Vec::new(),
            frames: Vec::new(),
        };
        layout.relayout(metrics, width);
        layout
    }

    /// Lays the same blocks out again for a new width or cell size.
    pub fn relayout(&mut self, metrics: GridMetrics, width: f32) {
        self.metrics = metrics;
        self.width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        Flow::run(self);
    }

    /// The width it was laid out for.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// The content's height, insets included; 0 for an empty view.
    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn metrics(&self) -> &GridMetrics {
        &self.metrics
    }

    /// How many items (view positions) the blocks cover.
    pub fn item_count(&self) -> u32 {
        self.items
    }

    /// How many cells: an item each, except a collapsed moment's one.
    pub fn cell_count(&self) -> u32 {
        self.cells.len() as u32
    }

    /// Cell `cell`, which must be under [`Self::cell_count`].
    pub fn cell(&self, cell: u32) -> GridCell {
        let slot = self.cells[cell as usize];
        GridCell {
            cell,
            item: slot.item,
            span: slot.span,
            rect: Rectangle::new(
                Point::new(slot.x, self.lines[slot.line as usize].cell_top),
                self.metrics.cell,
            ),
        }
    }

    /// The cell showing `item`: a collapsed moment's cell for every item it covers.
    pub fn cell_of_item(&self, item: u32) -> Option<u32> {
        if item >= self.items {
            return None;
        }
        let after = self.cells.partition_point(|slot| slot.item <= item);
        Some(after as u32 - 1)
    }

    /// `item`'s cell's rectangle, in content coordinates.
    pub fn item_rect(&self, item: u32) -> Option<Rectangle> {
        self.cell_of_item(item).map(|cell| self.cell(cell).rect)
    }

    /// The cells on the lines that cross `scroll - margin .. scroll + viewport + margin`, in reading
    /// order. The margin lets the caller ask for previews of the lines just off screen.
    pub fn visible_cells(&self, scroll: f32, viewport: f32, margin: f32) -> Range<u32> {
        let lines = self.lines_between(
            finite(scroll) - finite(margin).max(0.0),
            finite(scroll) + finite(viewport).max(0.0) + finite(margin).max(0.0),
        );
        if lines.is_empty() {
            let at = self
                .lines
                .get(lines.start)
                .map_or(self.cells.len() as u32, |line| line.cells.start);
            return at..at;
        }
        self.lines[lines.start].cells.start..self.lines[lines.end - 1].cells.end
    }

    /// What `point` (content coordinates) is over: a cell or a moment's header. Headings, gaps and
    /// a frame's padding are nothing.
    pub fn hit(&self, point: Point) -> Option<GridHit> {
        match self.hit_detail(point)? {
            Target::Cell(cell) => {
                let slot = self.cells[cell as usize];
                Some(GridHit::Cell {
                    cell,
                    item: slot.item,
                    span: slot.span,
                })
            }
            Target::Header(frame) => Some(GridHit::MomentHeader {
                moment: self.frames[frame as usize].moment,
            }),
        }
    }

    /// The scroll offset closest to `scroll` that shows `item`'s cell whole — with its moment's
    /// header when the cell is on the moment's first line — clamped to the content. A cell taller
    /// than the viewport shows its top.
    pub fn reveal(&self, item: u32, scroll: f32, viewport: f32) -> f32 {
        let scroll = self.clamp_scroll(scroll, viewport);
        let Some(cell) = self.cell_of_item(item) else {
            return scroll;
        };
        let rect = self.cell(cell).rect;
        let line = &self.lines[self.cells[cell as usize].line as usize];
        let top = self.frames[line.frames.start as usize..line.frames.end as usize]
            .iter()
            .find(|frame| {
                frame.rect.y == line.top
                    && rect.x >= frame.rect.x
                    && rect.x < frame.rect.x + frame.rect.width
            })
            .map_or(rect.y, |frame| frame.rect.y);
        let bottom = rect.y + rect.height;
        let viewport = finite(viewport).max(0.0);
        let target = if top < scroll || bottom - top > viewport {
            top
        } else if bottom > scroll + viewport {
            bottom - viewport
        } else {
            scroll
        };
        self.clamp_scroll(target, viewport)
    }

    /// The item of the cell next to `item`'s in `direction`: the previous or next cell in reading
    /// order, or on the previous or next line of cells (headings skipped) the cell nearest in x.
    pub fn neighbour(&self, item: u32, direction: GridDirection) -> Option<u32> {
        let cell = self.cell_of_item(item)? as usize;
        let target = match direction {
            GridDirection::Left => cell.checked_sub(1)?,
            GridDirection::Right => (cell + 1 < self.cells.len()).then_some(cell + 1)?,
            GridDirection::Up | GridDirection::Down => {
                let slot = self.cells[cell];
                let mut line = slot.line as usize;
                let line = loop {
                    line = match direction {
                        GridDirection::Up => line.checked_sub(1)?,
                        _ => line + 1,
                    };
                    let candidate = self.lines.get(line)?;
                    if !candidate.cells.is_empty() {
                        break candidate;
                    }
                };
                let row = &self.cells[line.cells.start as usize..line.cells.end as usize];
                let at = row.partition_point(|other| other.x < slot.x);
                let nearest = [at.checked_sub(1), (at < row.len()).then_some(at)]
                    .into_iter()
                    .flatten()
                    .min_by(|&a, &b| {
                        (row[a].x - slot.x)
                            .abs()
                            .total_cmp(&(row[b].x - slot.x).abs())
                            .then(a.cmp(&b))
                    })?;
                line.cells.start as usize + nearest
            }
        };
        Some(self.cells[target].item)
    }

    /// `scroll` clamped to the content: from 0 to the height less the viewport.
    pub fn clamp_scroll(&self, scroll: f32, viewport: f32) -> f32 {
        let most = (self.height - finite(viewport)).max(0.0);
        finite(scroll).clamp(0.0, most)
    }

    /// One line of cells and the gap after it: what a wheel's line or an arrow key scrolls.
    pub fn row_step(&self) -> f32 {
        self.metrics.cell.height + self.metrics.gap
    }

    // -- For the widget and its drawing ------------------------------------------------------------

    /// The lines whose bands cross `top..bottom`.
    pub(crate) fn lines_between(&self, top: f32, bottom: f32) -> Range<usize> {
        let first = self.lines.partition_point(|line| line.bottom <= top);
        let end = self.lines.partition_point(|line| line.top < bottom);
        first..end.max(first)
    }

    pub(crate) fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub(crate) fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub(crate) fn block(&self, block: u32) -> &GridBlock {
        &self.blocks[block as usize]
    }

    /// What `point` is over, with the frame for a header.
    pub(crate) fn hit_detail(&self, point: Point) -> Option<Target> {
        let after = self.lines.partition_point(|line| line.top <= point.y);
        let line = &self.lines[after.checked_sub(1)?];
        if point.y >= line.bottom || line.kind != LineKind::Cells {
            return None;
        }
        for frame in line.frames.clone() {
            if self.frames[frame as usize]
                .header(&self.metrics)
                .contains(point)
            {
                return Some(Target::Header(frame));
            }
        }
        let cell = self.metrics.cell;
        if point.y < line.cell_top || point.y >= line.cell_top + cell.height {
            return None;
        }
        let row = &self.cells[line.cells.start as usize..line.cells.end as usize];
        let at = row
            .partition_point(|slot| slot.x <= point.x)
            .checked_sub(1)?;
        (point.x < row[at].x + cell.width).then_some(Target::Cell(line.cells.start + at as u32))
    }
}

/// What a point is over, as the widget needs it: a cell, or the header of a frame (an index into
/// the frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Cell(u32),
    Header(u32),
}

fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// The line being filled, left to right.
struct Open {
    top: f32,
    /// The right edge of its last block.
    end: f32,
    cells: u32,
    frames: u32,
    moment: bool,
}

/// The flow of blocks into lines: one pass over the blocks.
struct Flow<'l> {
    layout: &'l mut GridLayout,
    left: f32,
    right: f32,
    /// Where the next line starts, before any gap.
    y: f32,
    /// Whether the last line was a line of cells, so the next one follows a gap.
    after_cells: bool,
    open: Option<Open>,
    item: u32,
}

impl<'l> Flow<'l> {
    fn run(layout: &'l mut GridLayout) {
        layout.cells.clear();
        layout.lines.clear();
        layout.frames.clear();
        let metrics = layout.metrics;
        let left = metrics.side_inset;
        let right = left.max(layout.width - metrics.side_inset);
        let blocks = std::mem::take(&mut layout.blocks);
        let mut flow = Flow {
            layout,
            left,
            right,
            y: 0.0,
            after_cells: false,
            open: None,
            item: 0,
        };
        let mut moment = 0;
        for (index, block) in blocks.iter().enumerate() {
            match block {
                GridBlock::Day(_) => flow.heading(index as u32, metrics.day_heading),
                GridBlock::Camera(_) => flow.heading(index as u32, metrics.camera_heading),
                GridBlock::Singles(count) => {
                    for _ in 0..*count {
                        flow.single(1);
                    }
                }
                GridBlock::Moment {
                    frames, collapsed, ..
                } => {
                    if *frames > 0 {
                        if *collapsed {
                            flow.single(*frames);
                        } else {
                            flow.moment(moment, index as u32, *frames);
                        }
                    }
                    moment += 1;
                }
            }
        }
        flow.close();
        let bottom = flow.layout.lines.last().map(|line| line.bottom);
        flow.layout.items = flow.item;
        flow.layout.height = bottom.map_or(0.0, |bottom| bottom + metrics.bottom_inset);
        flow.layout.blocks = blocks;
    }

    fn metrics(&self) -> GridMetrics {
        self.layout.metrics
    }

    /// Where a new line of cells starts: after a gap under another line of cells, under the top
    /// inset when it is the view's first line.
    fn next_top(&self) -> f32 {
        if self.layout.lines.is_empty() {
            self.metrics().top_inset
        } else if self.after_cells {
            self.y + self.metrics().gap
        } else {
            self.y
        }
    }

    fn heading(&mut self, block: u32, height: f32) {
        self.close();
        let cells = self.layout.cells.len() as u32;
        let frames = self.layout.frames.len() as u32;
        self.layout.lines.push(Line {
            top: self.y,
            bottom: self.y + height,
            cell_top: self.y,
            cells: cells..cells,
            frames: frames..frames,
            kind: LineKind::Heading(block),
        });
        self.y += height;
        self.after_cells = false;
    }

    /// Places a block `width` wide on the open line if it fits there, or first starts a new line,
    /// and returns its x.
    fn place(&mut self, width: f32) -> f32 {
        let gap = self.metrics().gap;
        if let Some(open) = &mut self.open
            && open.end + gap + width <= self.right
        {
            let x = open.end + gap;
            open.end = x + width;
            return x;
        }
        self.close();
        let top = self.next_top();
        self.open = Some(Open {
            top,
            end: self.left + width,
            cells: self.layout.cells.len() as u32,
            frames: self.layout.frames.len() as u32,
            moment: false,
        });
        self.left
    }

    fn line_index(&self) -> u32 {
        self.layout.lines.len() as u32
    }

    fn single(&mut self, span: u32) {
        let x = self.place(self.metrics().cell.width);
        let line = self.line_index();
        self.layout.cells.push(Slot {
            item: self.item,
            span,
            x,
            line,
        });
        self.item += span;
    }

    fn moment(&mut self, moment: u32, block: u32, frames: u32) {
        let metrics = self.metrics();
        let width = metrics.moment_width(frames);
        if width > self.right - self.left {
            self.wrapped_moment(moment, block, frames);
            return;
        }
        let x = self.place(width);
        let line = self.line_index();
        if let Some(open) = &mut self.open {
            open.moment = true;
        }
        self.layout.frames.push(Frame {
            moment,
            block,
            // The top and height are set when the line closes.
            rect: Rectangle::new(Point::new(x, 0.0), Size::new(width, 0.0)),
        });
        let first = x + metrics.moment_padding;
        for frame in 0..frames {
            self.layout.cells.push(Slot {
                item: self.item + frame,
                span: 1,
                x: first + frame as f32 * (metrics.cell.width + metrics.gap),
                line,
            });
        }
        self.item += frames;
    }

    /// A moment wider than the content: whole lines of its own, its cells wrapping in one frame.
    fn wrapped_moment(&mut self, moment: u32, block: u32, frames: u32) {
        self.close();
        let metrics = self.metrics();
        let inner = self.right - self.left - 2.0 * metrics.moment_padding;
        let columns = (((inner + metrics.gap) / (metrics.cell.width + metrics.gap)).floor() as u32)
            .clamp(1, frames);
        let rows = frames.div_ceil(columns);
        let top = self.next_top();
        let frame = self.layout.frames.len() as u32;
        let height = metrics.moment_header
            + rows as f32 * metrics.cell.height
            + (rows - 1) as f32 * metrics.gap
            + metrics.moment_padding;
        self.layout.frames.push(Frame {
            moment,
            block,
            rect: Rectangle::new(
                Point::new(self.left, top),
                Size::new(metrics.moment_width(columns), height),
            ),
        });
        let mut line_top = top;
        for row in 0..rows {
            let cell_top = if row == 0 {
                top + metrics.moment_header
            } else {
                line_top
            };
            let last = row + 1 == rows;
            let bottom =
                cell_top + metrics.cell.height + if last { metrics.moment_padding } else { 0.0 };
            let line = self.line_index();
            let start = self.layout.cells.len() as u32;
            for column in 0..columns.min(frames - row * columns) {
                self.layout.cells.push(Slot {
                    item: self.item + row * columns + column,
                    span: 1,
                    x: self.left
                        + metrics.moment_padding
                        + column as f32 * (metrics.cell.width + metrics.gap),
                    line,
                });
            }
            self.layout.lines.push(Line {
                top: line_top,
                bottom,
                cell_top,
                cells: start..self.layout.cells.len() as u32,
                frames: frame..frame + 1,
                kind: LineKind::Cells,
            });
            line_top = bottom + metrics.gap;
        }
        self.item += frames;
        self.y = self.layout.lines.last().map_or(top, |line| line.bottom);
        self.after_cells = true;
    }

    /// Ends the open line: its cells and frames now know their tops.
    fn close(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        let metrics = self.metrics();
        let header = if open.moment {
            metrics.moment_header
        } else {
            0.0
        };
        let padding = if open.moment {
            metrics.moment_padding
        } else {
            0.0
        };
        let cell_top = open.top + header;
        let bottom = cell_top + metrics.cell.height + padding;
        for frame in &mut self.layout.frames[open.frames as usize..] {
            frame.rect.y = open.top;
            frame.rect.height = bottom - open.top;
        }
        self.layout.lines.push(Line {
            top: open.top,
            bottom,
            cell_top,
            cells: open.cells..self.layout.cells.len() as u32,
            frames: open.frames..self.layout.frames.len() as u32,
            kind: LineKind::Cells,
        });
        self.y = bottom;
        self.after_cells = true;
    }
}

#[cfg(test)]
mod tests;
