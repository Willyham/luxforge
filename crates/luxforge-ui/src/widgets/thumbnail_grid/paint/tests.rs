use super::*;
use crate::widgets::thumbnail_grid::layout::{GridHeading, HeaderAction, MomentHeader, PendingRow};
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
        color: Ink,
        align: Align,
    },
    Icon {
        icon: Icon,
        size: f32,
        color: Ink,
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
        color: Ink,
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

    fn icon(&mut self, icon: Icon, size: f32, color: Ink, at: Point) {
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
        theme: Box::leak(Box::new(Theme::luxforge_dark())),
    }
}

fn moment_header(kind: MomentKind) -> MomentHeader {
    MomentHeader {
        kind,
        title: "Bracket".into(),
        detail: "3 exposures · −2 · 0 · +2 EV".into(),
        evidence: Some("from metadata".into()),
        picked: Some("1 picked".into()),
        actions: vec![HeaderAction::new("Pick all 3")],
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
    assert_eq!(ground(0).color, Ink::Derived(Derived::CellSurface));
    assert_eq!(ground(0).border, None);
    assert_eq!(ground(0).rect, area(0));
    assert_eq!(ground(0).radius, theme::RADIUS);
    assert_eq!(ground(1).color, Ink::Derived(Derived::CellSelected));
    assert_eq!(ground(2).color, Ink::Derived(Derived::CellSelected));
    assert_eq!(ground(2).border, Some((1.5, Ink::Token(Token::Accent))));

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
    assert_eq!(shadow.color, Ink::Derived(Derived::CellSurface));
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
        let disc = Fill::flat(pick_at(cell), Ink::Token(Token::Accent), 9.0);
        assert!(recorder.ops.contains(&(1, Op::Fill(disc))), "cell {cell}");
        assert!(recorder.ops.iter().any(|(layer, op)| *layer == 1
            && matches!(op, Op::Icon { icon: Icon::Check, color, .. } if *color == Ink::Token(Token::AccentInk))));
    }
    let picks = recorder
        .ops
        .iter()
        .filter(|(_, op)| matches!(op, Op::Fill(fill) if fill.color == Ink::Token(Token::Accent)))
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
        .find(|fill| fill.color == Ink::Fixed(theme::CELL_BADGE_SURFACE))
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
            if content == "Offline" && *color == Ink::Fixed(theme::CLIPPING_HIGHLIGHT))));

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
        .find(|fill| fill.color == Ink::Derived(Derived::CellPlaceholder))
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
            .filter(|fill| fill.color == Ink::Fixed(theme::CELL_BADGE_SURFACE))
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
        Ink::Token(Token::Accent),
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
            color: Ink::Token(Token::TextTertiary),
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
            plan.actions
                .first()
                .map(|action| (action.label, action.rect)),
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
                border: Some((1.0, Ink::Derived(Derived::MomentOutline))),
                ..Fill::flat(frame, Ink::Derived(Derived::MomentSurface), 9.0)
            })
        )
    );
    assert!(recorder.ops.contains(&(
        0,
        Op::Icon {
            icon: Icon::Bracket,
            size: 12.0,
            color: Ink::Token(Token::TextIdentity),
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
    // The tag on the control surface, the button on the labelled button's, in the scene's theme.
    let button = Ink::Fixed(Theme::luxforge_dark().palette().control);
    assert!(recorder.ops.iter().any(|(_, op)| matches!(op,
        Op::Fill(fill) if fill.color == Ink::Token(Token::Control) && fill.radius == 4.0)));
    assert!(recorder.ops.iter().any(|(_, op)| matches!(op,
        Op::Fill(fill) if fill.color == button && fill.radius == 6.0)));
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

/// A bracket's header with Pick all, Import merged and Pick merged keeps them left to right with
/// Pick merged at the right in the accent; narrower, Pick all goes before Import merged, and the
/// primary goes last.
#[test]
fn a_header_s_actions_give_way_from_the_left_and_the_primary_last() {
    let metrics = GridMetrics::select(136.0);
    let header = MomentHeader {
        actions: vec![
            HeaderAction::new("Pick all 3"),
            HeaderAction::new("Import merged"),
            HeaderAction::primary("Pick merged"),
        ],
        ..moment_header(MomentKind::Bracket)
    };
    let plan = |width: f32| {
        let strip = Rectangle::new(Point::new(0.0, 0.0), Size::new(width, 28.0));
        let mut recorder = Recorder::default();
        plan_header(&header, strip, &metrics, &mut recorder)
    };
    let wide = plan(900.0);
    let labels: Vec<_> = wide.actions.iter().map(|action| action.label).collect();
    assert_eq!(labels, ["Pick all 3", "Import merged", "Pick merged"]);
    assert_eq!(
        wide.actions
            .iter()
            .map(|action| (action.index, action.primary))
            .collect::<Vec<_>>(),
        [(0, false), (1, false), (2, true)]
    );
    // The buttons are 6 pt apart, the last 10 pt in.
    assert_eq!(
        wide.actions[2].rect.x + wide.actions[2].rect.width,
        900.0 - 10.0
    );
    assert_eq!(
        wide.actions[1].rect.x + wide.actions[1].rect.width + 6.0,
        wide.actions[2].rect.x
    );
    let mut seen = Vec::new();
    for width in (40..900).rev().step_by(2) {
        let plan = plan(width as f32);
        let kept: Vec<u32> = plan.actions.iter().map(|action| action.index).collect();
        if seen.last() != Some(&kept) {
            seen.push(kept);
        }
    }
    assert_eq!(seen, [vec![0, 1, 2], vec![1, 2], vec![2], vec![]]);
    // The title is whole whenever an ordinary action is drawn; only the primary shortens it.
    for width in (40..900).step_by(2) {
        let plan = plan(width as f32);
        if plan.actions.iter().any(|action| !action.primary) {
            let title = plan.title.as_ref().map(|(title, _)| title.as_ref());
            assert_eq!(title, Some(header.title.as_str()), "at {width} pt");
        }
    }
}

/// A primary action is drawn in the accent's button style, an ordinary one in the control's.
#[test]
fn a_primary_action_draws_in_the_accent() {
    let metrics = GridMetrics::select(136.0);
    let header = MomentHeader {
        actions: vec![
            HeaderAction::new("Import merged"),
            HeaderAction::primary("Pick merged"),
        ],
        ..moment_header(MomentKind::Bracket)
    };
    let layout = GridLayout::new(
        vec![GridBlock::Moment {
            header,
            frames: 4,
            collapsed: false,
        }],
        metrics,
        700.0,
    );
    let mut recorder = Recorder::default();
    let scene = scene(&layout, 0.0, Size::new(700.0, 300.0));
    paint(&scene, &|_| CellView::default(), &mut recorder);
    let strip = header_strip(&layout.frames()[0], &metrics, 0.0);
    let GridBlock::Moment { header, .. } = layout.block(0) else {
        unreachable!()
    };
    let plan = plan_header(header, strip, &metrics, &mut Recorder::default());
    let fill = |rect: Rectangle| {
        recorder
            .fills_in(rect)
            .into_iter()
            .find(|fill| fill.rect == rect)
            .unwrap()
            .color
    };
    let theme = Theme::luxforge_dark();
    let status = iced::widget::button::Status::Active;
    let accent = theme::button_accent(&theme, status).background;
    let control = theme::button_control(&theme, status).background;
    assert_ne!(accent, control);
    let ink = |background: Option<iced::Background>| match background {
        Some(iced::Background::Color(color)) => Ink::Fixed(color),
        _ => unreachable!(),
    };
    assert_eq!(fill(plan.actions[1].rect), ink(accent));
    assert_eq!(fill(plan.actions[0].rect), ink(control));
}

/// A pending row is a framed line of its own: its title and detail, its progress as an accent bar
/// along the track at its bottom, and its button at the right; a failed one names why in the
/// warning colour and has no bar.
#[test]
fn a_pending_row_draws_its_words_progress_and_button() {
    let metrics = GridMetrics::select(136.0);
    let row = PendingRow {
        icon: crate::Icon::Bracket,
        title: "Merging HDR · 3 exposures".into(),
        detail: "Merging 2 of 3".into(),
        progress: Some(0.5),
        action: Some("Cancel".into()),
        failed: false,
    };
    let layout = GridLayout::new(
        vec![GridBlock::Pending(row.clone()), GridBlock::Singles(2)],
        metrics,
        700.0,
    );
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(700.0, 300.0)),
        &|_| CellView::default(),
        &mut recorder,
    );
    let line = &layout.lines()[0];
    let plan = plan_pending(&layout, line, &row, 0.0, &mut Recorder::default());
    assert_eq!(
        plan.rect,
        Rectangle::new(Point::new(16.0, 10.0), Size::new(668.0, 40.0))
    );
    let texts = recorder.texts();
    assert!(texts.contains(&"Merging HDR · 3 exposures"), "{texts:?}");
    assert!(texts.contains(&"Merging 2 of 3"), "{texts:?}");
    assert!(texts.contains(&"Cancel"), "{texts:?}");
    let (_, button) = plan.action.unwrap();
    assert_eq!(button.x + button.width, 16.0 + 668.0 - 10.0);
    let fills = recorder.fills_in(plan.track);
    let done = fills
        .iter()
        .find(|fill| fill.color == Ink::Token(Token::Accent))
        .unwrap();
    assert_eq!(done.rect.width, (plan.track.width * 0.5).round());
    // The cells start a gap under it.
    assert_eq!(layout.cell(0).rect.y, 10.0 + 40.0 + 6.0);

    let failed = PendingRow {
        detail: "DSC_0002.NEF could not be read".into(),
        progress: None,
        action: Some("Dismiss".into()),
        failed: true,
        ..row
    };
    let layout = GridLayout::new(vec![GridBlock::Pending(failed)], metrics, 700.0);
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(700.0, 300.0)),
        &|_| CellView::default(),
        &mut recorder,
    );
    assert!(recorder.ops.iter().any(|(_, op)| matches!(op,
        Op::Text { content, color, .. }
            if content == "DSC_0002.NEF could not be read"
                && *color == Ink::Fixed(theme::CLIPPING_HIGHLIGHT))));
    assert!(
        !recorder
            .ops
            .iter()
            .any(|(_, op)| matches!(op, Op::Fill(fill) if fill.color == Ink::Token(Token::Accent))),
        "no bar"
    );
}

/// A stack's badge sits at the top right with its long words, its short words in a narrower cell
/// and only its icons in the narrowest; its chevron points right while it is closed and down
/// while it is open. A note sits at the photograph's bottom left.
#[test]
fn a_stack_badge_fits_its_words_and_shows_its_disclosure() {
    let view = |expanded| CellView {
        picked: true,
        stack: Some(CellStack {
            icon: Icon::Bracket,
            label: "HDR · 3 exposures",
            short: "HDR · 3",
            expanded,
        }),
        ..CellView::default()
    };
    let words = |width: f32| {
        let metrics = GridMetrics::select(width);
        let (rect, text) = plan_stack(
            Point::ORIGIN,
            &view(false),
            &metrics,
            &mut Recorder::default(),
        )
        .unwrap();
        assert_eq!(
            rect.x + rect.width,
            metrics.cell.width - metrics.badge_inset
        );
        text
    };
    assert_eq!(words(200.0), "HDR · 3 exposures");
    assert_eq!(words(136.0), "HDR · 3");
    assert_eq!(words(60.0), "");

    let metrics = GridMetrics::select(200.0);
    let layout = GridLayout::new(vec![GridBlock::Singles(2)], metrics, 700.0);
    let mut recorder = Recorder::default();
    let cell = |grid: GridCell| CellView {
        note: (grid.item == 1).then_some("−2 EV · outside the filter"),
        ..view(grid.item == 1)
    };
    paint(
        &scene(&layout, 0.0, Size::new(700.0, 300.0)),
        &cell,
        &mut recorder,
    );
    let chevrons: Vec<Icon> = recorder
        .ops
        .iter()
        .filter_map(|(layer, op)| match op {
            Op::Icon { icon, .. }
                if *layer == 1 && matches!(icon, Icon::ChevronDown | Icon::ChevronRight) =>
            {
                Some(*icon)
            }
            _ => None,
        })
        .collect();
    assert_eq!(chevrons, [Icon::ChevronRight, Icon::ChevronDown]);
    assert!(recorder.texts().contains(&"−2 EV · outside the filter"));
}

/// A panorama's stack draws its own icon, a wide frame, on its badge, its originals' notes and its
/// pending row, where an HDR stack draws a bracket.
#[test]
fn a_panorama_stack_draws_its_own_icon() {
    let metrics = GridMetrics::select(200.0);
    let row = PendingRow {
        icon: Icon::Panorama,
        title: "Merging panorama · 7 photos".into(),
        detail: "Stitching".into(),
        progress: None,
        action: Some("Cancel".into()),
        failed: false,
    };
    let layout = GridLayout::new(
        vec![GridBlock::Pending(row), GridBlock::Singles(2)],
        metrics,
        700.0,
    );
    let cell = |grid: GridCell| CellView {
        stack: (grid.item == 0).then_some(CellStack {
            icon: Icon::Panorama,
            label: "Panorama · 7 photos",
            short: "Pano · 7",
            expanded: true,
        }),
        note: (grid.item == 1).then_some("Outside the filter"),
        note_icon: Some(Icon::Panorama),
        ..CellView::default()
    };
    let mut recorder = Recorder::default();
    paint(
        &scene(&layout, 0.0, Size::new(700.0, 400.0)),
        &cell,
        &mut recorder,
    );
    let icons: Vec<Icon> = recorder
        .ops
        .iter()
        .filter_map(|(_, op)| match op {
            Op::Icon { icon, .. } if matches!(icon, Icon::Panorama | Icon::Bracket) => Some(*icon),
            _ => None,
        })
        .collect();
    assert_eq!(
        icons,
        [Icon::Panorama; 3],
        "the row, the badge and the note"
    );
    assert!(recorder.texts().contains(&"Panorama · 7 photos"));
}
