use super::*;

// -- Inputs -------------------------------------------------------------------------------------

fn heading(title: &str, detail: &str) -> GridHeading {
    GridHeading {
        title: title.into(),
        detail: detail.into(),
    }
}

fn header(kind: MomentKind) -> MomentHeader {
    MomentHeader {
        kind,
        title: match kind {
            MomentKind::Burst => "Burst".into(),
            MomentKind::Bracket => "Bracket".into(),
        },
        detail: "frames".into(),
        evidence: None,
        picked: None,
        action: None,
    }
}

fn day() -> GridBlock {
    GridBlock::Day(heading("Saturday 12 September 2026", "618 photographs"))
}

fn camera() -> GridBlock {
    GridBlock::Camera(heading("Leica Q2", "318"))
}

fn burst(frames: u32) -> GridBlock {
    GridBlock::Moment {
        header: header(MomentKind::Burst),
        frames,
        collapsed: false,
    }
}

fn bracket(frames: u32) -> GridBlock {
    GridBlock::Moment {
        header: header(MomentKind::Bracket),
        frames,
        collapsed: false,
    }
}

fn collapsed(frames: u32) -> GridBlock {
    GridBlock::Moment {
        header: header(MomentKind::Burst),
        frames,
        collapsed: true,
    }
}

fn singles(count: u32) -> GridBlock {
    GridBlock::Singles(count)
}

/// The default Select metrics: 136 × 122 cells, 6 pt gaps, 16 pt sides.
fn select() -> GridMetrics {
    GridMetrics::select(136.0)
}

/// A width whose content is `cells` single cells wide, exactly.
fn width_for(cells: u32) -> f32 {
    2.0 * 16.0 + cells as f32 * 136.0 + (cells - 1) as f32 * 6.0
}

/// A small deterministic generator, so the larger inputs are the same every run.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as u32
    }
}

/// About `items` items in days and cameras, with moments of 2 to 30 frames (some collapsed) and
/// runs of singles.
fn generated(items: u32, seed: u64) -> Vec<GridBlock> {
    let mut random = Random(seed);
    let mut blocks = Vec::new();
    let mut count = 0;
    while count < items {
        blocks.push(day());
        let cameras = 1 + random.below(3);
        for _ in 0..cameras {
            if cameras > 1 {
                blocks.push(camera());
            }
            for _ in 0..(5 + random.below(40)) {
                let block = match random.below(6) {
                    0 => burst(2 + random.below(29)),
                    1 => bracket(2 + random.below(8)),
                    2 => collapsed(2 + random.below(29)),
                    _ => singles(1 + random.below(14)),
                };
                count += match &block {
                    GridBlock::Moment { frames, .. } => *frames,
                    GridBlock::Singles(count) => *count,
                    _ => 0,
                };
                blocks.push(block);
            }
        }
    }
    blocks
}

// -- An independent evaluation ------------------------------------------------------------------

/// A box in the flow: a cell (first item, span) or a moment row (moment, frames).
#[derive(Clone, Copy)]
enum Piece {
    Cell(u32, u32),
    Moment(u32, u32),
}

#[derive(Debug, PartialEq)]
struct Expected {
    /// Every cell's first item, span and rectangle, in reading order.
    cells: Vec<(u32, u32, Rectangle)>,
    /// Every expanded moment's number and frame.
    frames: Vec<(u32, Rectangle)>,
    height: f32,
}

/// The layout written the plain way: split the blocks into groups at the headings, the groups into
/// rows by adding up widths, then give each row its height from what it holds.
fn reference(blocks: &[GridBlock], m: &GridMetrics, width: f32) -> Expected {
    let (cw, ch, gap, pad, head) = (
        m.cell.width,
        m.cell.height,
        m.gap,
        m.moment_padding,
        m.moment_header,
    );
    let content = (width - 2.0 * m.side_inset).max(0.0);
    let piece_width = |piece: Piece| match piece {
        Piece::Cell(..) => cw,
        Piece::Moment(_, n) => 2.0 * pad + n as f32 * cw + (n - 1) as f32 * gap,
    };
    // Rows of pieces, each row tagged as following a heading (no gap) or not; `None` rows are
    // headings of the given height.
    let mut rows: Vec<(Option<Vec<Piece>>, f32)> = Vec::new();
    let mut current: Vec<Piece> = Vec::new();
    let (mut item, mut moment) = (0, 0);
    let flush = |current: &mut Vec<Piece>, rows: &mut Vec<(Option<Vec<Piece>>, f32)>| {
        if !current.is_empty() {
            rows.push((Some(std::mem::take(current)), 0.0));
        }
    };
    for block in blocks {
        let piece = match block {
            GridBlock::Day(_) => {
                flush(&mut current, &mut rows);
                rows.push((None, m.day_heading));
                continue;
            }
            GridBlock::Camera(_) => {
                flush(&mut current, &mut rows);
                rows.push((None, m.camera_heading));
                continue;
            }
            GridBlock::Singles(n) => {
                for _ in 0..*n {
                    let used: f32 = current.iter().map(|&p| piece_width(p) + gap).sum();
                    if !current.is_empty() && used + cw > content {
                        flush(&mut current, &mut rows);
                    }
                    current.push(Piece::Cell(item, 1));
                    item += 1;
                }
                continue;
            }
            GridBlock::Moment {
                frames, collapsed, ..
            } => {
                let this = moment;
                moment += 1;
                if *frames == 0 {
                    continue;
                }
                let start = item;
                item += frames;
                if *collapsed {
                    Piece::Cell(start, *frames)
                } else {
                    Piece::Moment(this, *frames)
                }
            }
        };
        let used: f32 = current.iter().map(|&p| piece_width(p) + gap).sum();
        let wide = piece_width(piece) > content;
        if !current.is_empty() && (wide || used + piece_width(piece) > content) {
            flush(&mut current, &mut rows);
        }
        current.push(piece);
        if wide {
            flush(&mut current, &mut rows);
        }
    }
    flush(&mut current, &mut rows);

    let mut expected = Expected {
        cells: Vec::new(),
        frames: Vec::new(),
        height: 0.0,
    };
    let (mut y, mut after_cells) = (0.0, false);
    let mut first_item = 0;
    for (index, (row, height)) in rows.iter().enumerate() {
        let Some(row) = row else {
            y += height;
            after_cells = false;
            continue;
        };
        let top = if index == 0 {
            m.top_inset
        } else if after_cells {
            y + gap
        } else {
            y
        };
        let has_moment = row.iter().any(|p| matches!(p, Piece::Moment(..)));
        let mut x = m.side_inset;
        for &piece in row {
            match piece {
                Piece::Cell(item, span) => {
                    let cell_top = if has_moment { top + head } else { top };
                    expected.cells.push((
                        item,
                        span,
                        Rectangle::new(Point::new(x, cell_top), m.cell),
                    ));
                    first_item = item + span;
                }
                Piece::Moment(moment, frames) if piece_width(piece) > content => {
                    // Whole lines of its own: as many columns as fit, at least one.
                    let columns =
                        ((content - 2.0 * pad + gap) / (cw + gap)).floor().max(1.0) as u32;
                    let columns = columns.min(frames);
                    let rows = frames.div_ceil(columns);
                    for frame in 0..frames {
                        let (r, c) = (frame / columns, frame % columns);
                        expected.cells.push((
                            first_item + frame,
                            1,
                            Rectangle::new(
                                Point::new(
                                    x + pad + c as f32 * (cw + gap),
                                    top + head + r as f32 * (ch + gap),
                                ),
                                m.cell,
                            ),
                        ));
                    }
                    first_item += frames;
                    let frame_height = head + rows as f32 * ch + (rows - 1) as f32 * gap + pad;
                    expected.frames.push((
                        moment,
                        Rectangle::new(
                            Point::new(x, top),
                            Size::new(
                                2.0 * pad + columns as f32 * cw + (columns - 1) as f32 * gap,
                                frame_height,
                            ),
                        ),
                    ));
                }
                Piece::Moment(moment, frames) => {
                    for frame in 0..frames {
                        expected.cells.push((
                            first_item + frame,
                            1,
                            Rectangle::new(
                                Point::new(x + pad + frame as f32 * (cw + gap), top + head),
                                m.cell,
                            ),
                        ));
                    }
                    first_item += frames;
                    expected.frames.push((
                        moment,
                        Rectangle::new(
                            Point::new(x, top),
                            Size::new(piece_width(piece), head + ch + pad),
                        ),
                    ));
                }
            }
            x += piece_width(piece) + gap;
        }
        let tall = |p: &Piece| match *p {
            Piece::Moment(_, frames) if piece_width(*p) > content => {
                let columns = (((content - 2.0 * pad + gap) / (cw + gap)).floor().max(1.0) as u32)
                    .min(frames);
                let rows = frames.div_ceil(columns);
                head + rows as f32 * ch + (rows - 1) as f32 * gap + pad
            }
            Piece::Moment(..) => head + ch + pad,
            Piece::Cell(..) => ch,
        };
        y = top + row.iter().map(tall).fold(0.0, f32::max);
        after_cells = true;
    }
    expected.height = if rows.is_empty() {
        0.0
    } else {
        y + m.bottom_inset
    };
    expected
}

fn actual(layout: &GridLayout) -> Expected {
    Expected {
        cells: (0..layout.cell_count())
            .map(|cell| {
                let cell = layout.cell(cell);
                (cell.item, cell.span, cell.rect)
            })
            .collect(),
        frames: layout
            .frames()
            .iter()
            .map(|frame| (frame.moment, frame.rect))
            .collect(),
        height: layout.height(),
    }
}

/// The hand-written inputs: days, cameras, bursts, a bracket, a collapsed burst, singles, a
/// moment wider than the content, nothing at all, and a width narrower than one cell.
fn hand_written() -> Vec<(Vec<GridBlock>, f32)> {
    vec![
        (
            vec![
                day(),
                camera(),
                burst(3),
                singles(3),
                bracket(3),
                collapsed(4),
            ],
            width_for(5),
        ),
        (vec![singles(9)], width_for(4)),
        (vec![day(), burst(6), singles(1)], width_for(4)),
        (
            vec![camera(), bracket(2), singles(2), burst(2)],
            width_for(5),
        ),
        (
            vec![singles(3), day(), singles(2), camera(), collapsed(3)],
            width_for(3),
        ),
        (
            vec![day(), camera(), day(), camera(), singles(1)],
            width_for(3),
        ),
        (vec![], width_for(3)),
        (vec![day()], width_for(3)),
        (vec![singles(0), burst(0), collapsed(0)], width_for(3)),
        (vec![singles(3), burst(3), collapsed(2)], 60.0),
        (vec![burst(1), burst(2), singles(2)], width_for(1)),
    ]
}

// -- Tests --------------------------------------------------------------------------------------

#[test]
fn the_layout_agrees_with_an_independent_evaluation_on_hand_written_inputs() {
    for (blocks, width) in hand_written() {
        let layout = GridLayout::new(blocks.clone(), select(), width);
        assert_eq!(
            actual(&layout),
            reference(&blocks, &select(), width),
            "{blocks:?} at {width}"
        );
    }
}

#[test]
fn the_layout_agrees_with_an_independent_evaluation_on_generated_inputs() {
    for seed in 1..40 {
        let blocks = generated(300, seed);
        let width = 200.0 + (seed as f32 * 97.0) % 1400.0;
        for metrics in [
            select(),
            // Wider cells whose heights stay whole points, so both sums agree exactly.
            GridMetrics::select(196.0),
            GridMetrics::catalog(168.0),
        ] {
            let layout = GridLayout::new(blocks.clone(), metrics, width);
            assert_eq!(
                actual(&layout),
                reference(&blocks, &metrics, width),
                "seed {seed} at {width}"
            );
        }
    }
}

/// The first input by hand: a day and a camera heading, a three-frame burst with a single beside
/// it (aligned with the burst's cells), two more singles, a bracket and a collapsed burst.
#[test]
fn a_day_of_moments_and_singles_lands_where_the_boards_css_puts_it() {
    let layout = GridLayout::new(
        vec![
            day(),
            camera(),
            burst(3),
            singles(3),
            bracket(3),
            collapsed(4),
        ],
        select(),
        width_for(5),
    );
    // Day 0..36, camera 36..61, then the burst's row: its frame 432 wide from x = 16.
    let frames = layout.frames();
    assert_eq!(
        frames[0].rect,
        Rectangle::new(Point::new(16.0, 61.0), Size::new(432.0, 156.0))
    );
    // Its cells under the 28 pt header, 6 pt in.
    assert_eq!(
        layout.item_rect(0).unwrap().position(),
        Point::new(22.0, 89.0)
    );
    assert_eq!(
        layout.item_rect(2).unwrap().position(),
        Point::new(306.0, 89.0)
    );
    // A single fits beside it, its top on the burst's cells.
    assert_eq!(
        layout.item_rect(3).unwrap().position(),
        Point::new(454.0, 89.0)
    );
    // The next two start a line 6 pt under the row; the bracket does not fit after them.
    assert_eq!(
        layout.item_rect(4).unwrap().position(),
        Point::new(16.0, 223.0)
    );
    assert_eq!(
        layout.item_rect(5).unwrap().position(),
        Point::new(158.0, 223.0)
    );
    assert_eq!(
        frames[1].rect,
        Rectangle::new(Point::new(16.0, 351.0), Size::new(432.0, 156.0))
    );
    // The collapsed burst is one cell beside the bracket, covering items 9 to 12.
    let collapsed = layout.cell_of_item(9).unwrap();
    assert_eq!(layout.cell(collapsed).span, 4);
    assert_eq!(
        layout.cell(collapsed).rect.position(),
        Point::new(454.0, 379.0)
    );
    for item in 9..13 {
        assert_eq!(layout.cell_of_item(item), Some(collapsed));
    }
    assert_eq!(layout.item_count(), 13);
    assert_eq!(layout.cell_count(), 10);
    assert_eq!(layout.height(), 351.0 + 156.0 + 80.0);
    assert_eq!(frames[1].moment, 1);
}

#[test]
fn a_moment_wider_than_the_content_wraps_inside_one_frame_and_the_next_block_starts_a_line() {
    // Content 562 pt: 4 singles, but a frame holds (562 − 12 + 6) / 142 = 3 cells a row.
    let layout = GridLayout::new(vec![day(), burst(7), singles(1)], select(), width_for(4));
    let frames = layout.frames();
    assert_eq!(frames.len(), 1);
    let frame = frames[0].rect;
    assert_eq!(frame.position(), Point::new(16.0, 36.0));
    assert_eq!(frame.width, 12.0 + 3.0 * 136.0 + 2.0 * 6.0);
    assert_eq!(frame.height, 28.0 + 3.0 * 122.0 + 2.0 * 6.0 + 6.0);
    let rows: Vec<f32> = (0..7)
        .map(|item| layout.item_rect(item).unwrap().y)
        .collect();
    assert_eq!(rows, [64.0, 64.0, 64.0, 192.0, 192.0, 192.0, 320.0]);
    // The single after it starts its own line under the frame.
    assert_eq!(
        layout.item_rect(7).unwrap().position(),
        Point::new(16.0, frame.y + frame.height + 6.0)
    );
    // Each of its rows is a line: the frame crosses all three.
    let lines: Vec<_> = layout
        .lines()
        .iter()
        .filter(|line| line.frames == (0..1))
        .collect();
    assert_eq!(lines.len(), 3);
}

#[test]
fn nothing_lays_out_as_nothing() {
    for blocks in [vec![], vec![singles(0), burst(0), collapsed(0)]] {
        let layout = GridLayout::new(blocks, select(), 800.0);
        assert_eq!(
            (layout.item_count(), layout.cell_count(), layout.height()),
            (0, 0, 0.0)
        );
        assert_eq!(layout.visible_cells(0.0, 900.0, 200.0), 0..0);
        assert_eq!(layout.hit(Point::new(40.0, 40.0)), None);
        assert_eq!(layout.cell_of_item(0), None);
        assert_eq!(layout.reveal(0, 50.0, 900.0), 0.0);
        assert_eq!(layout.neighbour(0, GridDirection::Right), None);
        assert_eq!(layout.clamp_scroll(300.0, 900.0), 0.0);
    }
    // Headings alone still take their lines.
    let layout = GridLayout::new(vec![day(), camera()], select(), 800.0);
    assert_eq!(layout.item_count(), 0);
    assert_eq!(layout.height(), 36.0 + 25.0 + 80.0);
}

#[test]
fn a_width_narrower_than_one_cell_lays_out_one_column() {
    for width in [0.0, 60.0, 150.0, f32::NAN] {
        let layout = GridLayout::new(vec![singles(3), burst(3), collapsed(2)], select(), width);
        let xs: Vec<f32> = (0..layout.cell_count())
            .map(|cell| layout.cell(cell).rect.x)
            .collect();
        assert_eq!(xs, [16.0, 16.0, 16.0, 22.0, 22.0, 22.0, 16.0], "{width}");
        assert_eq!(layout.frames()[0].rect.width, 148.0);
        // One cell a line, top to bottom.
        let ys: Vec<f32> = (0..layout.cell_count())
            .map(|cell| layout.cell(cell).rect.y)
            .collect();
        assert!(ys.windows(2).all(|pair| pair[0] < pair[1]), "{ys:?}");
    }
}

/// Every cell's item range partitions the items, and every item's rectangle hit-tests back to its
/// cell; a header hit-tests to its moment.
#[test]
fn cells_partition_the_items_and_every_rect_hits_back() {
    let mut inputs = hand_written();
    inputs.extend((1..6).map(|seed| (generated(2000, seed), 300.0 + 211.0 * seed as f32)));
    for (blocks, width) in inputs {
        let layout = GridLayout::new(blocks, select(), width);
        let mut next = 0;
        for cell in 0..layout.cell_count() {
            let grid = layout.cell(cell);
            assert_eq!(grid.item, next, "cells cover the items in order");
            assert!(grid.span >= 1);
            next += grid.span;
            for item in grid.item..grid.item + grid.span {
                assert_eq!(layout.cell_of_item(item), Some(cell));
                assert_eq!(layout.item_rect(item), Some(grid.rect));
            }
            for point in [
                grid.rect.position(),
                grid.rect.center(),
                Point::new(
                    grid.rect.x + grid.rect.width - 0.01,
                    grid.rect.y + grid.rect.height - 0.01,
                ),
            ] {
                assert_eq!(
                    layout.hit(point),
                    Some(GridHit::Cell {
                        cell,
                        item: grid.item,
                        span: grid.span
                    })
                );
            }
        }
        assert_eq!(next, layout.item_count());
        assert_eq!(layout.cell_of_item(next), None);
        for frame in layout.frames() {
            let header = frame.header(layout.metrics());
            assert_eq!(
                layout.hit(header.center()),
                Some(GridHit::MomentHeader {
                    moment: frame.moment
                })
            );
            // The frame's padding under its cells is nothing.
            let under = Point::new(
                frame.rect.center_x(),
                frame.rect.y + frame.rect.height - 1.0,
            );
            assert_eq!(layout.hit(under), None);
        }
    }
}

#[test]
fn gaps_and_headings_hit_nothing() {
    let layout = GridLayout::new(vec![day(), camera(), singles(6)], select(), width_for(4));
    assert_eq!(layout.hit(Point::new(30.0, 10.0)), None, "the day heading");
    assert_eq!(
        layout.hit(Point::new(30.0, 50.0)),
        None,
        "the camera heading"
    );
    assert_eq!(
        layout.hit(Point::new(154.0, 100.0)),
        None,
        "between two cells"
    );
    assert_eq!(
        layout.hit(Point::new(30.0, 61.0 + 124.0)),
        None,
        "between two lines"
    );
    assert_eq!(layout.hit(Point::new(8.0, 100.0)), None, "the side inset");
    assert_eq!(layout.hit(Point::new(30.0, -5.0)), None);
    assert_eq!(layout.hit(Point::new(30.0, 5000.0)), None);
}

/// A 10,000-item view answers every visible range as a linear scan does, and never more cells
/// than the lines that fit in the viewport and its margins hold.
#[test]
fn ten_thousand_items_answer_visible_ranges_as_a_linear_scan_does() {
    let layout = GridLayout::new(generated(10_000, 7), select(), 1180.0);
    assert!(layout.item_count() >= 10_000);
    let (viewport, margin) = (820.0_f32, 260.0_f32);
    let per_line = ((1180.0 - 32.0 + 6.0) / 142.0_f32).floor() as u32;
    let most_lines = ((viewport + 2.0 * margin) / (122.0 + 6.0)).ceil() as u32 + 2;
    let mut scroll = -300.0;
    let mut checked = 0;
    while scroll < layout.height() + 300.0 {
        let range = layout.visible_cells(scroll, viewport, margin);
        let (top, bottom) = (scroll - margin, scroll + viewport + margin);
        let scanned: Vec<u32> = (0..layout.cell_count())
            .filter(|&cell| {
                let slot = layout.cells[cell as usize];
                let line = &layout.lines()[slot.line as usize];
                line.bottom > top && line.top < bottom
            })
            .collect();
        let expected = scanned.first().map_or(range.start..range.start, |&first| {
            first..scanned.last().unwrap() + 1
        });
        assert_eq!(range, expected, "at {scroll}");
        assert_eq!(scanned.len(), range.len(), "the range is contiguous");
        assert!(
            range.len() as u32 <= per_line * most_lines,
            "{} cells at {scroll}",
            range.len()
        );
        scroll += 173.0;
        checked += 1;
    }
    assert!(checked > 200, "{checked} offsets");
}

#[test]
fn reveal_moves_the_least_and_shows_a_moments_header() {
    let layout = GridLayout::new(generated(2000, 3), select(), width_for(6));
    let viewport = 600.0;
    for item in (0..layout.item_count()).step_by(7) {
        let scroll = layout.clamp_scroll(item as f32 * 11.0, viewport);
        let revealed = layout.reveal(item, scroll, viewport);
        let rect = layout.item_rect(item).unwrap();
        assert!(
            rect.y >= revealed - 0.001 && rect.y + rect.height <= revealed + viewport + 0.001,
            "item {item} at {revealed}"
        );
        // Already visible: nothing moves.
        assert_eq!(layout.reveal(item, revealed, viewport), revealed);
        assert_eq!(revealed, layout.clamp_scroll(revealed, viewport));
    }
    // A moment's first-line cell brings its header; below it, the smallest move shows the header's
    // top at the viewport's top.
    let layout = GridLayout::new(
        vec![singles(20), burst(3), singles(20)],
        select(),
        width_for(4),
    );
    let frame = layout.frames()[0].rect;
    assert_eq!(layout.reveal(20, frame.y + 50.0, 300.0), frame.y);
    // Above it, the cell's bottom meets the viewport's bottom.
    let cell = layout.item_rect(21).unwrap();
    assert_eq!(layout.reveal(21, 0.0, 300.0), cell.y + cell.height - 300.0);
    // A cell taller than the viewport shows its top.
    assert_eq!(layout.reveal(0, 200.0, 50.0), 10.0);
    // Past the end and before the start, the scroll is clamped.
    assert_eq!(layout.reveal(999, 1.0e6, 300.0), layout.height() - 300.0);
}

#[test]
fn neighbours_follow_reading_order_and_the_nearest_cell_across_headings_and_moments() {
    // Content 4 cells: line A singles 0..4; day heading; line B burst(2) = items 4, 5 and a
    // single 6; line C singles 7, 8; camera heading; line D collapsed 9..12 and single 12.
    let layout = GridLayout::new(
        vec![
            singles(4),
            day(),
            burst(2),
            singles(3),
            camera(),
            collapsed(3),
            singles(1),
        ],
        select(),
        width_for(4),
    );
    use GridDirection::{Down, Left, Right, Up};
    assert_eq!(layout.neighbour(0, Left), None);
    assert_eq!(layout.neighbour(0, Right), Some(1));
    assert_eq!(layout.neighbour(3, Right), Some(4), "across the heading");
    assert_eq!(layout.neighbour(4, Left), Some(3));
    assert_eq!(
        layout.neighbour(8, Right),
        Some(9),
        "into the collapsed burst"
    );
    assert_eq!(layout.neighbour(10, Right), Some(12), "from inside it");
    assert_eq!(layout.neighbour(11, Left), Some(8));
    assert_eq!(layout.neighbour(12, Right), None);
    // Down from line A crosses the day heading to the burst's line: the burst's cells sit 6 pt in,
    // at 22 and 164; the single beside it at 312.
    assert_eq!(layout.neighbour(0, Down), Some(4));
    assert_eq!(layout.neighbour(1, Down), Some(5));
    assert_eq!(layout.neighbour(2, Down), Some(6));
    assert_eq!(layout.neighbour(3, Down), Some(6), "the nearest in x");
    assert_eq!(layout.neighbour(6, Up), Some(2));
    assert_eq!(layout.neighbour(4, Up), Some(0));
    // Down again, then across the camera heading into the collapsed burst.
    assert_eq!(layout.neighbour(5, Down), Some(8));
    assert_eq!(layout.neighbour(7, Down), Some(9));
    assert_eq!(layout.neighbour(8, Down), Some(12));
    assert_eq!(layout.neighbour(11, Up), Some(7));
    assert_eq!(layout.neighbour(12, Down), None);
    assert_eq!(layout.neighbour(0, Up), None);
    assert_eq!(layout.neighbour(99, Left), None);
    // Inside a wrapped moment, rows are lines too.
    let layout = GridLayout::new(vec![burst(7)], select(), width_for(4));
    assert_eq!(layout.neighbour(1, Down), Some(4));
    assert_eq!(
        layout.neighbour(5, Down),
        Some(6),
        "the last row's one cell"
    );
    assert_eq!(layout.neighbour(6, Up), Some(3));
}

#[test]
fn scroll_is_clamped_to_the_content() {
    let layout = GridLayout::new(vec![singles(40)], select(), width_for(4));
    // Ten lines: 10 + 10 × 122 + 9 × 6 + 80.
    assert_eq!(layout.height(), 10.0 + 1220.0 + 54.0 + 80.0);
    let most = layout.height() - 500.0;
    assert_eq!(layout.clamp_scroll(-20.0, 500.0), 0.0);
    assert_eq!(layout.clamp_scroll(100.0, 500.0), 100.0);
    assert_eq!(layout.clamp_scroll(1.0e9, 500.0), most);
    assert_eq!(layout.clamp_scroll(f32::NAN, 500.0), 0.0);
    assert_eq!(layout.clamp_scroll(50.0, 1.0e5), 0.0, "all of it fits");
    assert_eq!(layout.row_step(), 128.0);
}

#[test]
fn a_relayout_keeps_the_items_and_changes_the_lines() {
    let blocks = generated(3000, 11);
    let mut layout = GridLayout::new(blocks.clone(), select(), 900.0);
    let (items, cells, lines) = (
        layout.item_count(),
        layout.cell_count(),
        layout.lines().len(),
    );
    layout.relayout(select(), 1500.0);
    assert_eq!((layout.item_count(), layout.cell_count()), (items, cells));
    assert!(layout.lines().len() < lines, "wider, fewer lines");
    assert_eq!(layout.width(), 1500.0);
    assert_eq!(actual(&layout), reference(&blocks, &select(), 1500.0));
    // A larger cell size: the same items, taller cells, more lines.
    layout.relayout(GridMetrics::select(200.0), 900.0);
    assert_eq!(layout.item_count(), items);
    assert!(layout.lines().len() > lines);
    assert_eq!(layout.cell(0).rect.size(), GridMetrics::select(200.0).cell);
}

#[test]
fn camera_titles_are_capitalised_like_a_section_label() {
    let layout = GridLayout::new(vec![camera()], select(), 800.0);
    let GridBlock::Camera(heading) = layout.block(0) else {
        panic!("a camera heading");
    };
    assert_eq!(heading.title, "LEICA Q2");
}

#[test]
fn the_presets_scale_the_image_box_by_its_ratio() {
    let select = GridMetrics::select(136.0);
    assert_eq!(select.cell, theme::CELL_SIZE);
    assert_eq!(select.image, theme::CELL_IMAGE);
    let wide = GridMetrics::select(196.0);
    assert_eq!(wide.image_max, Size::new(180.0, 86.0 + 60.0 * 86.0 / 120.0));
    assert_eq!(wide.cell.height, 122.0 + 60.0 * 86.0 / 120.0);
    assert_eq!(wide.footer_top, 98.0 + 60.0 * 86.0 / 120.0);
    assert_eq!(wide.footer_height, 24.0);
    let catalog = GridMetrics::catalog(168.0);
    assert_eq!(catalog.cell, theme::CATALOG_CELL_SIZE);
    assert_eq!(catalog.image_max, theme::CATALOG_CELL_IMAGE_MAX);
    let small = GridMetrics::catalog(10.0);
    assert_eq!(small.cell.width, theme::CELL_MIN_WIDTH);
    assert_eq!(GridMetrics::select(f32::NAN), GridMetrics::select(136.0));
    // Both presets share the flow's sizes.
    assert_eq!(
        (select.gap, select.moment_header, select.day_heading),
        (6.0, 28.0, 36.0)
    );
    assert_eq!(catalog.camera_heading, 25.0);
}
