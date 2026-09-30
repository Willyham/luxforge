use super::*;
use crate::widgets::thumbnail_grid::layout::{GridHeading, MomentHeader};
use std::cell::{Cell, RefCell};

// -- A painter that records -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Op {
    Fill(Fill),
    Image {
        size: (u32, u32),
        rect: Rectangle,
        opacity: f32,
    },
    Text {
        content: String,
        at: Point,
        style: TextStyle,
        color: Color,
        align: Align,
    },
    Icon {
        icon: Icon,
        size: f32,
        color: Color,
        at: Point,
    },
}

/// Records every draw with the layer it went to (0 the cells, 1 the overlay). Text is measured as
/// half its size per character, the ellipsis included.
#[derive(Default)]
struct Recorder {
    ops: Vec<(u32, Op)>,
    layer: u32,
    measured: usize,
}

fn width_of(content: &str, style: TextStyle) -> f32 {
    content.chars().count() as f32 * style.size * 0.5
}

impl Measure for Recorder {
    fn measure(&mut self, content: &str, style: TextStyle) -> f32 {
        self.measured += 1;
        width_of(content, style)
    }
}

impl<'a> Painter<'a> for Recorder {
    fn fill(&mut self, fill: Fill) {
        self.ops.push((self.layer, Op::Fill(fill)));
    }

    fn image(&mut self, handle: &'a Handle, rect: Rectangle, opacity: f32) {
        let Handle::Rgba { width, height, .. } = handle else {
            panic!("the tests draw RGBA handles");
        };
        self.ops.push((
            self.layer,
            Op::Image {
                size: (*width, *height),
                rect,
                opacity,
            },
        ));
    }

    fn image_size(&mut self, handle: &Handle) -> Option<Size> {
        match handle {
            Handle::Rgba { width, height, .. } => Some(Size::new(*width as f32, *height as f32)),
            _ => None,
        }
    }

    fn text(
        &mut self,
        content: &str,
        at: Point,
        style: TextStyle,
        color: Color,
        align: Align,
        _clip: Rectangle,
    ) {
        self.ops.push((
            self.layer,
            Op::Text {
                content: content.to_owned(),
                at,
                style,
                color,
                align,
            },
        ));
    }

    fn icon(&mut self, icon: Icon, size: f32, color: Color, at: Point) {
        self.ops.push((
            self.layer,
            Op::Icon {
                icon,
                size,
                color,
                at,
            },
        ));
    }

    fn overlay(&mut self, start: bool) {
        self.layer = if start { 1 } else { 0 };
    }
}

impl Recorder {
    fn texts(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|(_, op)| match op {
                Op::Text { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect()
    }

    fn fills_in(&self, area: Rectangle) -> Vec<Fill> {
        self.ops
            .iter()
            .filter_map(|(_, op)| match op {
                Op::Fill(fill) if area.contains(fill.rect.center()) => Some(*fill),
                _ => None,
            })
            .collect()
    }
}

// -- Inputs -------------------------------------------------------------------------------------

/// A 240 × 160 photograph: the gallery's first stand-in, a handle made once.
fn landscape() -> Handle {
    let handle = crate::gallery_thumbnails::thumbnail(0);
    let Handle::Rgba { width, height, .. } = &handle else {
        panic!("the stand-ins are RGBA");
    };
    assert_eq!((*width, *height), (240, 160));
    handle
}

fn colours() -> ScrollbarColours {
    ScrollbarColours {
        rail: Color::from_rgb8(1, 1, 1),
        scroller: Color::from_rgb8(2, 2, 2),
        active: Color::from_rgb8(3, 3, 3),
    }
}

fn scene(layout: &GridLayout, scroll: f32, size: Size) -> Scene<'_> {
    Scene {
        layout,
        scroll,
        size,
        hover: Hover::None,
        dragging: false,
        colours: colours(),
    }
}

fn moment_header(kind: MomentKind) -> MomentHeader {
    MomentHeader {
        kind,
        title: "Bracket".into(),
        detail: "3 exposures · −2 · 0 · +2 EV".into(),
        evidence: Some("from metadata".into()),
        picked: Some("1 picked".into()),
        action: Some("Pick all 3".into()),
    }
}

/// Plenty of days, cameras, moments and singles.
fn many(items: u32) -> Vec<GridBlock> {
    let mut blocks = Vec::new();
    let mut count = 0;
    let mut n = 0;
    while count < items {
        n += 1;
        blocks.push(GridBlock::Day(GridHeading {
            title: format!("Day {n}"),
            detail: "photographs".into(),
        }));
        for block in [
            GridBlock::Singles(7),
            GridBlock::Moment {
                header: moment_header(MomentKind::Burst),
                frames: 2 + n % 29,
                collapsed: n % 3 == 0,
            },
            GridBlock::Singles(13),
            GridBlock::Camera(GridHeading {
                title: "Nikon Z 8".into(),
                detail: "40".into(),
            }),
            GridBlock::Moment {
                header: moment_header(MomentKind::Bracket),
                frames: 3,
                collapsed: false,
            },
            GridBlock::Singles(9),
        ] {
            count += match &block {
                GridBlock::Moment { frames, .. } => *frames,
                GridBlock::Singles(count) => *count,
                _ => 0,
            };
            blocks.push(block);
        }
    }
    blocks
}

// -- Tests --------------------------------------------------------------------------------------

/// The closure is asked for each cell in the visible range once, and for no other, wherever the
/// grid is scrolled.
#[test]
fn the_closure_is_asked_only_for_visible_cells() {
    let layout = GridLayout::new(many(10_000), GridMetrics::select(136.0), 1100.0);
    let size = Size::new(1100.0, 760.0);
    let image = landscape();
    for scroll in [0.0, 333.0, layout.height() / 2.0, layout.height() - 760.0] {
        let asked = RefCell::new(Vec::new());
        let cell = |grid: GridCell| {
            asked.borrow_mut().push(grid.cell);
            CellView {
                image: Some(&image),
                ..CellView::default()
            }
        };
        let mut recorder = Recorder::default();
        paint(&scene(&layout, scroll, size), &cell, &mut recorder);
        let visible: Vec<u32> = layout.visible_cells(scroll, size.height, 0.0).collect();
        assert!(!visible.is_empty());
        assert_eq!(*asked.borrow(), visible, "at {scroll}");
        let images = recorder
            .ops
            .iter()
            .filter(|(_, op)| matches!(op, Op::Image { .. }))
            .count();
        assert_eq!(images, visible.len());
    }
}

/// The cells board's states: resting, selected, active, picked, in the catalog, a collapsed burst
/// holding a pick with its count, offline, unreadable and loading, each drawn as the CSS does.
#[test]
fn each_cell_state_draws_as_the_board() {
    let metrics = GridMetrics::select(136.0);
    // One line of nine: the content is 9 cells wide.
    let layout = GridLayout::new(
        vec![
            GridBlock::Singles(5),
            GridBlock::Moment {
                header: moment_header(MomentKind::Burst),
                frames: 4,
                collapsed: true,
            },
            GridBlock::Singles(3),
        ],
        metrics,
        32.0 + 9.0 * 136.0 + 8.0 * 6.0,
    );
    let landscape = landscape();
    let cell = |grid: GridCell| {
        let mut view = CellView {
            image: Some(&landscape),
            ..CellView::default()
        };
        match grid.cell {
            1 => view.selected = true,
            2 => view.active = true,
            3 => view.picked = true,
            4 => view.in_catalog = true,
            5 => {
                view.picked = true;
                view.count = Some(grid.span);
            }
            6 => view.availability = CellAvailability::Offline,
            7 => view.availability = CellAvailability::Unreadable,
            8 => {
                view.image = None;
                view.aspect = Some(2.0 / 3.0);
            }
            _ => {}
        }
        view
    };
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(layout.width(), 400.0)),
        &cell,
        &mut recorder,
    );
    let area = |cell: u32| layout.cell(cell).rect;
    let ground = |cell: u32| recorder.fills_in(area(cell))[0];
    assert_eq!(ground(0).color, theme::CELL_SURFACE);
    assert_eq!(ground(0).border, None);
    assert_eq!(ground(0).rect, area(0));
    assert_eq!(ground(0).radius, theme::RADIUS);
    assert_eq!(ground(1).color, theme::CELL_SELECTED);
    assert_eq!(ground(2).color, theme::CELL_SELECTED);
    assert_eq!(ground(2).border, Some((1.5, theme::ACCENT)));

    // A 3:2 photograph is 120 × 80 in the 120 × 88 box 8 pt in: 12 pt from the cell's top, over a
    // shadowed rectangle of the cell's ground.
    let origin = area(0).position();
    let photo = Rectangle::new(
        Point::new(origin.x + 8.0, origin.y + 12.0),
        Size::new(120.0, 80.0),
    );
    assert!(recorder.ops.contains(&(
        0,
        Op::Image {
            size: (240, 160),
            rect: photo,
            opacity: 1.0
        }
    )));
    let shadow = recorder.fills_in(area(0))[1];
    assert_eq!(shadow.rect, photo);
    assert_eq!(shadow.color, theme::CELL_SURFACE);
    assert_eq!(shadow.shadow, Some(theme::CELL_IMAGE_SHADOW));

    // Picks: an accent disc in its ring at 11/11, the check in the ink, over the photograph.
    let pick_at = |cell: u32| {
        let origin = area(cell).position();
        Rectangle::new(
            Point::new(origin.x + 11.0, origin.y + 11.0),
            Size::new(18.0, 18.0),
        )
    };
    for cell in [3, 5] {
        let disc = Fill::flat(pick_at(cell), theme::ACCENT, 9.0);
        assert!(recorder.ops.contains(&(1, Op::Fill(disc))), "cell {cell}");
        assert!(recorder.ops.iter().any(|(layer, op)| *layer == 1
            && matches!(op, Op::Icon { icon: Icon::Check, color, .. } if *color == theme::PRIMARY_INK)));
    }
    let picks = recorder
        .ops
        .iter()
        .filter(|(_, op)| matches!(op, Op::Fill(fill) if fill.color == theme::ACCENT))
        .count();
    assert_eq!(
        picks, 2,
        "only the picked cells, and nothing else accent-filled"
    );

    let texts = recorder.texts();
    assert_eq!(
        texts,
        ["In the catalog", "4", "Offline", "Unreadable"],
        "the badges' words, in cell order"
    );
    // The count badge sits at the top right, 11 pt in, 16 pt tall.
    let count = recorder
        .fills_in(area(5))
        .into_iter()
        .find(|fill| fill.color == theme::CELL_BADGE_SURFACE)
        .unwrap();
    let right = area(5).x + 136.0 - 11.0;
    assert_eq!(count.rect.x + count.rect.width, right);
    assert_eq!(count.rect.height, 16.0);
    // 5 + icon 10 + 3 + "4" (5.25) + 5.
    assert_eq!(count.rect.width, 28.25);

    // Offline: the photograph dimmed, its shadow with it, and the red badge.
    let offline = recorder
        .ops
        .iter()
        .find_map(|(_, op)| match op {
            Op::Image { rect, opacity, .. } if area(6).contains(rect.center()) => Some(*opacity),
            _ => None,
        })
        .unwrap();
    assert_eq!(offline, theme::CELL_OFFLINE_OPACITY);
    assert!(recorder.ops.iter().any(|(layer, op)| *layer == 1
        && matches!(op, Op::Text { content, color, .. }
            if content == "Offline" && *color == theme::CLIPPING_HIGHLIGHT)));

    // Unreadable and loading: the placeholder, no photograph; loading has the header's shape.
    for cell in [7, 8] {
        assert!(
            !recorder.ops.iter().any(
                |(_, op)| matches!(op, Op::Image { rect, .. } if area(cell).contains(rect.center()))
            ),
            "cell {cell}"
        );
    }
    let placeholder = recorder
        .fills_in(area(8))
        .into_iter()
        .find(|fill| fill.color == theme::CELL_PLACEHOLDER)
        .unwrap();
    assert_eq!(placeholder.rect.size(), Size::new(57.0, 86.0));
    assert_eq!(placeholder.rect.y, area(8).y + 9.0);

    // Every badge is in the overlay; every photograph under it.
    for (layer, op) in &recorder.ops {
        match op {
            Op::Image { .. } => assert_eq!(*layer, 0),
            Op::Icon { .. } => assert_eq!(*layer, 1),
            Op::Text { .. } => assert_eq!(*layer, 1),
            Op::Fill(_) => {}
        }
    }
}

#[test]
fn the_catalog_badge_follows_the_pick_and_keeps_its_icon_when_crowded() {
    let metrics = GridMetrics::select(136.0);
    let layout = GridLayout::new(vec![GridBlock::Singles(3)], metrics, 600.0);
    let cell = |grid: GridCell| CellView {
        picked: grid.cell > 0,
        in_catalog: true,
        count: (grid.cell == 2).then_some(12),
        ..CellView::default()
    };
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(600.0, 300.0)),
        &cell,
        &mut recorder,
    );
    let badges = |cell: u32| {
        recorder
            .fills_in(layout.cell(cell).rect)
            .into_iter()
            .filter(|fill| fill.color == theme::CELL_BADGE_SURFACE)
            .map(|fill| fill.rect)
            .collect::<Vec<_>>()
    };
    // Alone: 11 pt in, 5 + 10 + 4 + 14 characters at 5.25 + 5 wide.
    let x = |cell: u32| layout.cell(cell).rect.x;
    assert_eq!(
        badges(0),
        [Rectangle::new(
            Point::new(x(0) + 11.0, 11.0 + 10.0),
            Size::new(97.5, 16.0)
        )]
    );
    // After the check (11 + 18 + 4) its words, measured here at 5.25 pt a character, would pass
    // the photograph's box (8 pt in), so it keeps its icon.
    assert_eq!(
        badges(1),
        [Rectangle::new(
            Point::new(x(1) + 33.0, 21.0),
            Size::new(20.0, 16.0)
        )]
    );
    let third = badges(2);
    assert_eq!(third.len(), 2, "the count and the icon");
    assert_eq!(
        third[1],
        Rectangle::new(Point::new(x(2) + 33.0, 21.0), Size::new(20.0, 16.0))
    );
    assert_eq!(recorder.texts(), ["In the catalog", "12"]);
}

#[test]
fn the_footer_draws_its_label_and_the_edited_dot() {
    let metrics = GridMetrics::catalog(168.0);
    let layout = GridLayout::new(vec![GridBlock::Singles(2)], metrics, 600.0);
    let name = "L1003206-a-very-long-file-name.DNG";
    let cell = |grid: GridCell| CellView {
        label: Some(if grid.cell == 0 { "−2 EV" } else { name }),
        edited: true,
        ..CellView::default()
    };
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(600.0, 300.0)),
        &cell,
        &mut recorder,
    );
    let origin = layout.cell(0).rect.position();
    let dot = Fill::flat(
        Rectangle::new(
            Point::new(origin.x + 168.0 - 10.0 - 6.0, origin.y + 142.0 + 15.0 - 3.0),
            Size::new(6.0, 6.0),
        ),
        theme::ACCENT,
        3.0,
    );
    assert!(recorder.ops.contains(&(0, Op::Fill(dot))));
    let texts = recorder.texts();
    assert_eq!(texts[0], "−2 EV");
    // 168 − 2 × 10 − 6 − 6 = 136 pt for the label, at 5.5 pt a character: 23 characters and the
    // ellipsis.
    assert_eq!(texts[1], "L1003206-a-very-long-fi\u{2026}");
    assert!(recorder.ops.contains(&(
        0,
        Op::Text {
            content: "−2 EV".into(),
            at: Point::new(origin.x + 10.0, origin.y + 157.0),
            style: regular(11.0),
            color: theme::TEXT_TERTIARY,
            align: Align::Left
        }
    )));
}

/// A header lays out its icon, title and detail from the left and the pick count, tag and button
/// from the right; narrower, the detail ends in an ellipsis, then the tag, the count and the button
/// give way in that order, and the title ends in an ellipsis last.
#[test]
fn a_moment_header_truncates_and_gives_way_in_order() {
    let metrics = GridMetrics::select(136.0);
    let header = moment_header(MomentKind::Bracket);
    let plan = |width: f32| {
        let strip = Rectangle::new(Point::new(100.0, 50.0), Size::new(width, 28.0));
        let mut recorder = Recorder::default();
        let plan = plan_header(&header, strip, &metrics, &mut recorder);
        (
            plan.title.map(|(text, at)| (text.into_owned(), at)),
            plan.detail.map(|(text, _)| text.into_owned()),
            plan.picked,
            plan.tag,
            plan.action,
        )
    };
    // Wide: everything, whole. The button's right edge is 10 pt in; the tag 13 pt before it, the
    // count 7 pt before the tag.
    let (title, detail, picked, tag, action) = plan(600.0);
    let (_, button) = action.unwrap();
    assert_eq!(button.x + button.width, 700.0 - 10.0);
    // "Pick all 3": 10 characters at 5.5, and 8 pt each side.
    assert_eq!(button.width, 71.0);
    assert_eq!((button.y, button.height), (53.0, 22.0));
    let (_, tag) = tag.unwrap();
    assert_eq!(tag.x + tag.width, button.x - 13.0);
    assert_eq!(
        (tag.y, tag.height, tag.width),
        (57.0, 14.0, 13.0 * 5.0 + 12.0)
    );
    let (_, picked_at) = picked.unwrap();
    assert_eq!(picked_at, Point::new(tag.x - 7.0, 64.0));
    let (title, title_at) = title.unwrap();
    assert_eq!(title, "Bracket");
    assert_eq!(title_at, Point::new(100.0 + 10.0 + 12.0 + 7.0, 64.0));
    assert_eq!(detail.unwrap(), header.detail);

    // Narrower: the detail ends in an ellipsis before it reaches the right-hand parts.
    let (_, detail, picked, tag, action) = plan(400.0);
    let detail = detail.unwrap();
    assert!(detail.ends_with(ELLIPSIS), "{detail}");
    assert!(picked.is_some() && tag.is_some() && action.is_some());

    // Then the tag goes, then the count, then the action; the title shortens last.
    let parts = |width: f32| {
        let (title, _, picked, tag, action) = plan(width);
        (
            title.map(|(text, _)| text),
            picked.is_some(),
            tag.is_some(),
            action.is_some(),
        )
    };
    let mut seen = Vec::new();
    for width in (40..600).rev().step_by(2) {
        let (title, picked, tag, action) = parts(width as f32);
        let state = (picked, tag, action);
        if seen.last() != Some(&state) {
            seen.push(state);
        }
        if !action {
            // With nothing on the right, the title shortens, but never overlaps anything.
            if let Some(title) = title {
                assert!(width_of(&title, MOMENT_TITLE) <= width as f32 - 20.0 - 19.0);
            }
        }
    }
    assert_eq!(
        seen,
        [
            (true, true, true),
            (true, false, true),
            (false, false, true),
            (false, false, false)
        ]
    );
}

#[test]
fn a_frame_draws_its_surface_outline_icon_and_header() {
    let metrics = GridMetrics::select(136.0);
    let layout = GridLayout::new(
        vec![GridBlock::Moment {
            header: moment_header(MomentKind::Bracket),
            frames: 4,
            collapsed: false,
        }],
        metrics,
        800.0,
    );
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(800.0, 400.0)),
        &|_| CellView::default(),
        &mut recorder,
    );
    let frame = layout.frames()[0].rect;
    assert_eq!(
        recorder.ops[0],
        (
            0,
            Op::Fill(Fill {
                border: Some((1.0, theme::MOMENT_OUTLINE)),
                ..Fill::flat(frame, theme::MOMENT_SURFACE, 9.0)
            })
        )
    );
    assert!(recorder.ops.contains(&(
        0,
        Op::Icon {
            icon: Icon::Bracket,
            size: 12.0,
            color: theme::TEXT_IDENTITY,
            at: Point::new(frame.x + 10.0, frame.y + 8.0)
        }
    )));
    assert_eq!(
        recorder.texts()[..5],
        [
            "Bracket",
            "3 exposures · −2 · 0 · +2 EV",
            "1 picked",
            "from metadata",
            "Pick all 3"
        ]
    );
    // The tag on the control surface, the button on the labelled button's.
    assert!(recorder.ops.iter().any(|(_, op)| matches!(op,
        Op::Fill(fill) if fill.color == theme::CONTROL && fill.radius == 4.0)));
    assert!(recorder.ops.iter().any(|(_, op)| matches!(op,
        Op::Fill(fill) if fill.color == theme::CONTROL && fill.radius == 6.0)));
}

#[test]
fn a_day_heading_puts_its_title_and_detail_on_one_baseline() {
    let metrics = GridMetrics::select(136.0);
    let layout = GridLayout::new(
        vec![
            GridBlock::Day(GridHeading {
                title: "Saturday 12 September 2026".into(),
                detail: "618 photographs · 9 picked".into(),
            }),
            GridBlock::Camera(GridHeading {
                title: "Leica Q2".into(),
                detail: "318".into(),
            }),
        ],
        metrics,
        800.0,
    );
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(800.0, 400.0)),
        &|_| CellView::default(),
        &mut recorder,
    );
    let texts: Vec<(String, Point, TextStyle)> = recorder
        .ops
        .iter()
        .filter_map(|(_, op)| match op {
            Op::Text {
                content, at, style, ..
            } => Some((content.clone(), *at, *style)),
            _ => None,
        })
        .collect();
    let (title, at, style) = &texts[0];
    assert_eq!(title, "Saturday 12 September 2026");
    assert_eq!(*at, Point::new(18.0, 22.0));
    assert_eq!(*style, semibold(13.0));
    let (detail, detail_at, detail_style) = &texts[1];
    assert_eq!(detail, "618 photographs · 9 picked");
    assert_eq!(detail_at.x, 18.0 + 26.0 * 6.5 + 10.0);
    let baseline = |at: &Point, style: &TextStyle| at.y + BASELINE_BELOW_CENTRE * style.size;
    assert!((baseline(at, style) - baseline(detail_at, detail_style)).abs() < 1e-4);
    // The camera heading, capitalised, 4 pt into its line, its count 8 pt after it.
    let (camera, camera_at, _) = &texts[2];
    assert_eq!(camera, "LEICA Q2");
    assert_eq!(*camera_at, Point::new(18.0, 36.0 + 4.0 + 6.5));
    assert_eq!(texts[3].1.x, 18.0 + 8.0 * 5.25 + 8.0);
}

#[test]
fn the_scrollbar_shows_only_when_the_content_is_taller_and_maps_its_travel() {
    let layout = GridLayout::new(vec![GridBlock::Singles(400)], GridMetrics::default(), 600.0);
    let size = Size::new(600.0, 500.0);
    assert!(scrollbar(400.0, 0.0, size).is_none());
    let bar = scrollbar(layout.height(), 0.0, size).unwrap();
    assert_eq!(
        bar.rail,
        Rectangle::new(Point::new(595.0, 0.0), Size::new(4.0, 500.0))
    );
    assert_eq!(bar.scroller.y, 0.0);
    let length = (500.0 * 500.0 / layout.height()).max(24.0);
    assert_eq!(bar.scroller.height, length);
    let range = layout.height() - 500.0;
    let end = scrollbar(layout.height(), range, size).unwrap();
    assert_eq!(end.scroller.y + end.scroller.height, 500.0);
    assert_eq!(bar.scroll_at(end.scroller.y), range);
    assert_eq!(bar.scroll_at(-40.0), 0.0);
    assert!((bar.scroll_at(bar.travel / 2.0) - range / 2.0).abs() < 0.01);

    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, size),
        &|_| CellView::default(),
        &mut recorder,
    );
    let (layer, last) = recorder.ops.last().unwrap();
    assert_eq!(*layer, 1);
    assert_eq!(
        *last,
        Op::Fill(Fill::flat(bar.scroller, colours().scroller, 2.0))
    );
    let mut recorder = Recorder::default();
    let mut dragged = scene(&layout, 0.0, size);
    dragged.dragging = true;
    paint(&dragged, &|_| CellView::default(), &mut recorder);
    assert_eq!(
        recorder.ops.last().unwrap().1,
        Op::Fill(Fill::flat(bar.scroller, colours().active, 2.0))
    );
}

#[test]
fn scrolled_content_draws_at_its_offset() {
    let layout = GridLayout::new(vec![GridBlock::Singles(40)], GridMetrics::default(), 600.0);
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 400.0, Size::new(600.0, 300.0)),
        &|_| CellView::default(),
        &mut recorder,
    );
    let Op::Fill(first) = &recorder.ops[0].1 else {
        panic!("a cell's ground first");
    };
    let cell = layout.cell(layout.visible_cells(400.0, 300.0, 0.0).start);
    assert_eq!(first.rect.y, cell.rect.y - 400.0);
}

#[test]
fn photographs_fit_their_box_on_whole_points() {
    let metrics = GridMetrics::select(136.0);
    let at = Point::new(100.0, 200.0);
    assert_eq!(
        photo_rect(at, 1.5, &metrics),
        Rectangle::new(Point::new(108.0, 212.0), Size::new(120.0, 80.0))
    );
    // Portrait: 86 pt tall, centred across.
    assert_eq!(
        photo_rect(at, 2.0 / 3.0, &metrics),
        Rectangle::new(Point::new(139.0, 209.0), Size::new(57.0, 86.0))
    );
    // Square and panoramic.
    assert_eq!(photo_rect(at, 1.0, &metrics).size(), Size::new(86.0, 86.0));
    assert_eq!(photo_rect(at, 4.0, &metrics).size(), Size::new(120.0, 30.0));
    // Nonsense falls back to 3:2.
    assert_eq!(
        photo_rect(at, f32::NAN, &metrics),
        photo_rect(at, 1.5, &metrics)
    );
    assert_eq!(photo_rect(at, 0.0, &metrics), photo_rect(at, 1.5, &metrics));
    // The catalog's box: 148 × 124 in 148 × 128, 10 pt in.
    let catalog = GridMetrics::catalog(168.0);
    assert_eq!(
        photo_rect(Point::ORIGIN, 1.5, &catalog),
        Rectangle::new(Point::new(10.0, 25.0), Size::new(148.0, 99.0))
    );
}

#[test]
fn measuring_is_asked_only_for_what_is_drawn() {
    // Cells without labels or badges measure nothing; a frame's header a handful of strings.
    let layout = GridLayout::new(vec![GridBlock::Singles(400)], GridMetrics::default(), 900.0);
    let count = Cell::new(0);
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 1000.0, Size::new(900.0, 700.0)),
        &|_| {
            count.set(count.get() + 1);
            CellView::default()
        },
        &mut recorder,
    );
    assert_eq!(recorder.measured, 0);
    assert!(count.get() > 0);
}
