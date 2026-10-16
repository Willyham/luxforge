//! Gallery states for the Select workspace's thumbnail grid, each drawn by the grid widget itself
//! from a layout made once: every cell state, moments with their headers, a moment wider than the
//! view, a 10,000-file view scrolled to the day after its middle, and the catalog's larger cells.
//!
//! The gallery rebuilds its states in every `view()`, so each layout and each photograph's handle
//! lives in a `LazyLock` and is borrowed, as the app holds its own: a handle made in `view()` would
//! upload again every frame. The small states end 10 pt under their last line rather than the
//! floating strip's 80, so each shows whole without a scrollbar.

use crate::Element;
use crate::gallery_thumbnails::{SCENES, bracket, thumbnail};
use crate::{
    CellAvailability, CellStack, CellView, GridBlock, GridCell, GridHeading, GridLayout,
    GridLineKind, GridMetrics, HeaderAction, MenuEntry, MenuItem, MomentHeader, MomentKind,
    PendingRow, caption, menu_list, theme, thumbnail_grid,
};
use iced::Length;
use iced::widget::image::Handle;
use iced::widget::{column, container};
use std::sync::LazyLock;

/// The grid's width in a gallery card, which is about 684 pt wide at the 1440 pt capture.
const WIDTH: f32 = 680.0;
/// A narrow grid: the sources and Info panels open beside a small window, at the smallest cells.
const NARROW: f32 = 330.0;
/// The size slider's smallest cell.
const SMALLEST_CELL: f32 = 96.0;
/// The 10,000-file view's height.
const TALL: f32 = 740.0;

static PHOTOS: LazyLock<Vec<Handle>> = LazyLock::new(|| (0..SCENES).map(thumbnail).collect());
static EXPOSURES: LazyLock<[Handle; 3]> = LazyLock::new(|| [0, 1, 2].map(bracket));

fn photo(index: u32) -> &'static Handle {
    &PHOTOS[index as usize % SCENES]
}

/// The Select metrics, ending 10 pt under the last line.
fn compact(metrics: GridMetrics) -> GridMetrics {
    GridMetrics {
        bottom_inset: metrics.top_inset,
        ..metrics
    }
}

fn heading(title: &str, detail: &str) -> GridHeading {
    GridHeading {
        title: title.into(),
        detail: detail.into(),
    }
}

fn moment(
    kind: MomentKind,
    title: &str,
    detail: &str,
    frames: u32,
    evidence: Option<&str>,
    picked: Option<&str>,
    action: Option<&str>,
) -> GridBlock {
    GridBlock::Moment {
        header: MomentHeader {
            kind,
            title: title.into(),
            detail: detail.into(),
            evidence: evidence.map(Into::into),
            picked: picked.map(Into::into),
            actions: action.map(HeaderAction::new).into_iter().collect(),
        },
        frames,
        collapsed: false,
    }
}

fn collapsed(frames: u32) -> GridBlock {
    GridBlock::Moment {
        header: MomentHeader {
            kind: MomentKind::Burst,
            title: "Burst".into(),
            detail: format!("{frames} frames"),
            evidence: None,
            picked: None,
            actions: Vec::new(),
        },
        frames,
        collapsed: true,
    }
}

/// A grid of `layout` at the gallery's width, `height` tall, on the canvas as the centre draws it.
fn grid(
    layout: &'static GridLayout,
    scroll: f32,
    height: f32,
    cell: impl Fn(GridCell) -> CellView<'static> + 'static,
) -> Element<'static, ()> {
    container(
        thumbnail_grid(layout, scroll, cell)
            .width(Length::Fixed(WIDTH))
            .height(Length::Fixed(height)),
    )
    .style(theme::canvas_surface(theme::CANVAS))
    .into()
}

/// A small layout shown whole.
fn whole(
    layout: &'static GridLayout,
    cell: impl Fn(GridCell) -> CellView<'static> + 'static,
) -> Element<'static, ()> {
    grid(layout, 0.0, layout.height(), cell)
}

// -- Cells and moments ----------------------------------------------------------------------------

/// Ten cells, one per state: the fifth block is a collapsed burst of four.
static CELLS: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![GridBlock::Singles(5), collapsed(4), GridBlock::Singles(4)],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn cells() -> Element<'static, ()> {
    whole(&CELLS, |grid| {
        let mut view = CellView {
            image: Some(photo(1)),
            ..CellView::default()
        };
        match grid.cell {
            1 => view.selected = true,
            2 => view.active = true,
            3 => view.picked = true,
            4 => view.in_catalog = true,
            5 => {
                view.image = Some(photo(3));
                view.picked = true;
                view.count = Some(grid.span);
            }
            6 => {
                view.image = Some(photo(6));
                view.availability = CellAvailability::Offline;
            }
            7 => view.availability = CellAvailability::Unreadable,
            8 => {
                view.image = None;
                view.aspect = Some(2.0 / 3.0);
            }
            9 => view.copied = true,
            _ => {}
        }
        view
    })
}

/// A six-frame burst in a view four cells wide: two rows in one frame, then a single on its own
/// line.
static WRAPPED: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            moment(
                MomentKind::Burst,
                "Burst",
                "6 frames in 1.4 s",
                6,
                None,
                Some("1 picked"),
                None,
            ),
            GridBlock::Singles(1),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn wrapped() -> Element<'static, ()> {
    whole(&WRAPPED, |grid| CellView {
        image: Some(photo(if grid.item < 6 { 0 } else { 9 })),
        picked: grid.item == 2,
        active: grid.item == 2,
        ..CellView::default()
    })
}

/// A day and a camera heading over a burst with its pick and a bracket from the metadata with its
/// tag, frame labels and Pick all 3.
static DAY: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            GridBlock::Day(heading(
                "Saturday 12 September 2026",
                "618 photographs \u{b7} 9 picked",
            )),
            GridBlock::Camera(heading("Leica Q2", "318")),
            moment(
                MomentKind::Burst,
                "Burst",
                "3 frames in 0.6 s",
                3,
                None,
                Some("1 picked"),
                None,
            ),
            moment(
                MomentKind::Bracket,
                "Bracket",
                "3 exposures \u{b7} \u{2212}2 \u{b7} 0 \u{b7} +2 EV",
                3,
                Some("from metadata"),
                None,
                Some("Pick all 3"),
            ),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

const EXPOSURE_LABELS: [&str; 3] = ["\u{2212}2 EV", "0 EV", "+2 EV"];

fn day() -> Element<'static, ()> {
    whole(&DAY, |grid| {
        if grid.item < 3 {
            CellView {
                image: Some(photo(0)),
                picked: grid.item == 1,
                ..CellView::default()
            }
        } else {
            let step = (grid.item - 3) as usize;
            CellView {
                image: Some(&EXPOSURES[step]),
                label: Some(EXPOSURE_LABELS[step]),
                ..CellView::default()
            }
        }
    })
}

/// A bracket measured from the previews, a collapsed burst beside it (its top on the bracket's
/// cells), and two singles on the next line.
static PREVIEWS: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            moment(
                MomentKind::Bracket,
                "Bracket",
                "about \u{2212}1.7 \u{b7} 0 \u{b7} +1.9 EV",
                3,
                Some("from previews"),
                None,
                None,
            ),
            collapsed(5),
            GridBlock::Singles(2),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn previews() -> Element<'static, ()> {
    whole(&PREVIEWS, |grid| match grid.item {
        0..3 => CellView {
            image: Some(&EXPOSURES[grid.item as usize]),
            ..CellView::default()
        },
        3 => CellView {
            image: Some(photo(3)),
            count: Some(grid.span),
            ..CellView::default()
        },
        item => CellView {
            image: Some(photo(item)),
            selected: item == 9,
            ..CellView::default()
        },
    })
}

// -- 10,000 files ---------------------------------------------------------------------------------

/// A deterministic view of 10,000 files from 1 September 2026: days of a few hundred to about 1,600
/// frames from one to three cameras, moments of 2 to 30 frames (bursts, some collapsed, and
/// brackets of 2 to 9) and runs of singles.
pub(crate) fn ten_thousand_blocks() -> Vec<GridBlock> {
    const ITEMS: u32 = 10_000;
    const WEEKDAYS: [&str; 7] = [
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
    ];
    const CAMERAS: [&str; 3] = ["Leica Q2", "Nikon Z 8", "DJI FC3411"];
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut below = |bound: u32| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) % bound as u64) as u32
    };
    let mut blocks = Vec::new();
    let mut items = 0;
    let mut date = 0;
    while items < ITEMS {
        let cameras = 1 + below(3) as usize;
        let mut day = Vec::new();
        let mut day_items = 0;
        for name in &CAMERAS[..cameras] {
            let mut run = Vec::new();
            let mut camera_items = 0;
            let target = 150 + below(450);
            while camera_items < target && items + day_items + camera_items < ITEMS {
                let left = ITEMS - items - day_items - camera_items;
                let block = match below(8) {
                    0 | 1 => {
                        let frames = (2 + below(29)).min(left);
                        if below(4) == 0 {
                            collapsed(frames)
                        } else {
                            let picked = (below(3) == 0).then_some("1 picked");
                            moment(
                                MomentKind::Burst,
                                "Burst",
                                &format!("{frames} frames in {}.{} s", frames / 5, frames % 5 * 2),
                                frames,
                                None,
                                picked,
                                None,
                            )
                        }
                    }
                    2 => {
                        let frames = (2 + below(8)).min(left);
                        moment(
                            MomentKind::Bracket,
                            "Bracket",
                            &format!("{frames} exposures"),
                            frames,
                            Some("from metadata"),
                            None,
                            Some(&format!("Pick all {frames}")),
                        )
                    }
                    _ => GridBlock::Singles((1 + below(16)).min(left)),
                };
                camera_items += match &block {
                    GridBlock::Moment { frames, .. } => *frames,
                    GridBlock::Singles(count) => *count,
                    _ => 0,
                };
                run.push(block);
            }
            if cameras > 1 {
                day.push(GridBlock::Camera(heading(name, &camera_items.to_string())));
            }
            day.extend(run);
            day_items += camera_items;
        }
        blocks.push(GridBlock::Day(heading(
            &format!(
                "{} {} {} 2026",
                WEEKDAYS[date % 7],
                if date < 30 { date + 1 } else { date - 29 },
                if date < 30 { "September" } else { "October" }
            ),
            &format!("{day_items} photographs"),
        )));
        blocks.extend(day);
        items += day_items;
        date += 1;
    }
    blocks
}

static TEN_THOUSAND: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        ten_thousand_blocks(),
        GridMetrics::select(theme::CELL_SIZE.width),
        WIDTH,
    )
});

/// Where the 10,000-file view is scrolled and which file is active: the first day that starts after
/// the middle file, its heading at the top, so the window shows a day's heading, its cameras and
/// moments in the middle of the view; the day's first file is active, with a few selected after it.
static MIDDLE: LazyLock<(f32, u32)> = LazyLock::new(|| {
    let layout = &*TEN_THOUSAND;
    let middle = layout.item_rect(5_000).map_or(0.0, |rect| rect.y);
    let lines = layout.lines();
    let day = lines
        .iter()
        .position(|line| {
            line.top >= middle
                && matches!(line.kind, GridLineKind::Heading(block)
                    if matches!(layout.block(block), GridBlock::Day(_)))
        })
        .unwrap_or(0);
    let first = lines[day..]
        .iter()
        .find(|line| !line.cells.is_empty())
        .map_or(0, |line| layout.cell(line.cells.start).item);
    (layout.clamp_scroll(lines[day].top.round(), TALL), first)
});

/// The 10,000-file view's scroll offset.
pub(crate) fn middle(layout: &GridLayout) -> f32 {
    debug_assert!(std::ptr::eq(layout, &*TEN_THOUSAND));
    MIDDLE.0
}

/// `n` with thousands separated by commas.
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What each of the 10,000 files shows: a pick every so often, a selection with the active file in
/// the middle of it, and a collapsed burst's count.
pub(crate) fn ten_thousand_cell(grid: GridCell) -> CellView<'static> {
    let item = grid.item;
    CellView {
        image: Some(photo(item.wrapping_mul(7) / 3)),
        picked: item % 17 == 3,
        selected: (MIDDLE.1..MIDDLE.1 + 5).contains(&item),
        active: item == MIDDLE.1,
        count: (grid.span > 1).then_some(grid.span),
        ..CellView::default()
    }
}

fn ten_thousand() -> Element<'static, ()> {
    let layout = &*TEN_THOUSAND;
    let scroll = middle(layout);
    let visible = layout.visible_cells(scroll, TALL, 0.0);
    let first = layout.cell(visible.start).item;
    let last = layout.cell(visible.end - 1);
    let last = last.item + last.span - 1;
    column![
        caption(format!(
            "{} files in {} cells \u{b7} scrolled to {} of {} pt \u{b7} items {}\u{2013}{} on \
             screen \u{b7} the closure is asked for {} cells",
            thousands(layout.item_count()),
            thousands(layout.cell_count()),
            thousands(scroll as u32),
            thousands(layout.height() as u32),
            thousands(first),
            thousands(last),
            visible.len(),
        )),
        grid(layout, scroll, TALL, ten_thousand_cell),
    ]
    .spacing(theme::SPACING / 2.0)
    .into()
}

// -- The catalog's cells --------------------------------------------------------------------------

static CATALOG: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![GridBlock::Singles(6)],
        compact(GridMetrics::catalog(theme::CATALOG_CELL_SIZE.width)),
        WIDTH,
    )
});

fn catalog() -> Element<'static, ()> {
    whole(&CATALOG, |grid| CellView {
        image: Some(photo([10, 0, 9, 1, 3, 6][grid.cell as usize % 6])),
        selected: matches!(grid.cell, 1 | 2 | 4),
        active: grid.cell == 1,
        edited: true,
        availability: if grid.cell == 5 {
            CellAvailability::Offline
        } else {
            CellAvailability::Available
        },
        ..CellView::default()
    })
}

// -- HDR merges ------------------------------------------------------------------------------------

/// A bracket's header with its merge: Pick all while not all are picked, and the primary Merge
/// HDR.
fn bracket_with_merges(picked: Option<&str>) -> GridBlock {
    GridBlock::Moment {
        header: MomentHeader {
            kind: MomentKind::Bracket,
            title: "Bracket".into(),
            detail: "3 exposures \u{b7} \u{2212}2 \u{b7} 0 \u{b7} +2 EV".into(),
            evidence: Some("from metadata".into()),
            picked: picked.map(Into::into),
            actions: vec![
                HeaderAction::new("Pick all 3"),
                HeaderAction::primary("Merge HDR"),
            ],
        },
        frames: 3,
        collapsed: false,
    }
}

fn pending(
    title: &str,
    detail: &str,
    progress: Option<f32>,
    action: &str,
    failed: bool,
) -> GridBlock {
    GridBlock::Pending(PendingRow {
        icon: crate::Icon::Bracket,
        title: title.into(),
        detail: detail.into(),
        progress,
        action: Some(action.into()),
        failed,
    })
}

/// A bracket's frame: its exposure and its footer label.
fn exposure(step: usize) -> CellView<'static> {
    CellView {
        image: Some(&EXPOSURES[step]),
        label: Some(EXPOSURE_LABELS[step]),
        ..CellView::default()
    }
}

/// A result's badge: "HDR · 3 exposures", or "HDR · 3" in a narrow cell.
fn badge(expanded: bool) -> Option<CellStack<'static>> {
    Some(CellStack {
        icon: crate::Icon::Bracket,
        label: "HDR \u{b7} 3 exposures",
        short: "HDR \u{b7} 3",
        expanded,
    })
}

/// The header's merges beside Pick all, over a bracket of files.
static MERGE_HEADER: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![bracket_with_merges(None), GridBlock::Singles(1)],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn merge_header() -> Element<'static, ()> {
    whole(&MERGE_HEADER, |grid| match grid.item {
        0..3 => CellView {
            selected: grid.item == 1,
            ..exposure(grid.item as usize)
        },
        _ => CellView {
            image: Some(photo(9)),
            ..CellView::default()
        },
    })
}

/// The merge running: its row before the bracket, whose frames stay until the result is
/// published.
static MERGE_PENDING: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            pending(
                "Merging HDR \u{b7} 3 exposures",
                "Reading 2 of 3",
                Some(0.42),
                "Cancel",
                false,
            ),
            bracket_with_merges(None),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn merge_pending() -> Element<'static, ()> {
    whole(&MERGE_PENDING, |grid| exposure(grid.item as usize))
}

/// The result published: active, leading its collapsed stack with its badge, never shown picked,
/// between the frames before and after the bracket.
static MERGE_RESULT: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            GridBlock::Day(heading("Saturday 12 September 2026", "4 photographs")),
            GridBlock::Singles(4),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn merge_result() -> Element<'static, ()> {
    whole(&MERGE_RESULT, |grid| match grid.item {
        1 => CellView {
            image: Some(&EXPOSURES[1]),
            selected: true,
            active: true,
            stack: badge(false),
            ..CellView::default()
        },
        item => CellView {
            image: Some(photo([0, 0, 6, 9][item as usize % 4])),
            ..CellView::default()
        },
    })
}

/// A merge that failed: its row names the input, with Dismiss; the frames are as they were.
static MERGE_FAILED: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            pending(
                "HDR merge failed \u{b7} 3 exposures",
                "DSC_0002.NEF: the file changed since it was planned",
                None,
                "Dismiss",
                true,
            ),
            bracket_with_merges(None),
        ],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn merge_failed() -> Element<'static, ()> {
    whole(&MERGE_FAILED, |grid| exposure(grid.item as usize))
}

/// Show all 3 originals under the Picked filter: the result open, its originals after it with
/// their exposures, the one picked with its check and the two the filter left out labelled so.
static MERGE_EXPANDED: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![GridBlock::Singles(5)],
        compact(GridMetrics::select(theme::CELL_SIZE.width)),
        WIDTH,
    )
});

fn merge_expanded() -> Element<'static, ()> {
    whole(&MERGE_EXPANDED, |grid| match grid.item {
        0 => CellView {
            image: Some(&EXPOSURES[1]),
            stack: badge(true),
            ..CellView::default()
        },
        1..4 => {
            let step = (grid.item - 1) as usize;
            CellView {
                picked: step == 0,
                active: step == 0,
                selected: step == 0,
                note: (step != 0).then_some("Outside the filter"),
                ..exposure(step)
            }
        }
        _ => CellView {
            image: Some(photo(3)),
            picked: true,
            ..CellView::default()
        },
    })
}

/// The smallest cells in a narrow grid: the header's actions give way from Pick all, the primary
/// Merge HDR last; the result's badge is short.
static MERGE_NARROW: LazyLock<GridLayout> = LazyLock::new(|| {
    GridLayout::new(
        vec![
            pending(
                "Merging HDR \u{b7} 3 exposures",
                "Aligning",
                Some(0.7),
                "Cancel",
                false,
            ),
            bracket_with_merges(Some("1 picked")),
            GridBlock::Singles(2),
        ],
        compact(GridMetrics::select(SMALLEST_CELL)),
        NARROW,
    )
});

fn merge_narrow() -> Element<'static, ()> {
    container(
        thumbnail_grid(&MERGE_NARROW, 0.0, |grid| match grid.item {
            0..3 => CellView {
                picked: grid.item == 0,
                ..exposure(grid.item as usize)
            },
            3 => CellView {
                image: Some(&EXPOSURES[1]),
                stack: badge(false),
                ..CellView::default()
            },
            _ => CellView {
                image: Some(photo(6)),
                ..CellView::default()
            },
        })
        .width(Length::Fixed(NARROW))
        .height(Length::Fixed(MERGE_NARROW.height())),
    )
    .style(theme::canvas_surface(theme::CANVAS))
    .into()
}

/// The selection's merge menu: the HDR group's Merge HDR… with its hint; beside it the same with ⌥
/// held, without the ellipsis; and the same refused, the core's reason as its tooltip.
fn merge_menu() -> Element<'static, ()> {
    let item = |label: String, enabled: bool| {
        MenuEntry::Item(MenuItem {
            icon: None,
            label,
            trailing: None,
            on_press: enabled.then_some(()),
            reason: (!enabled).then(|| "These frames' exposures do not differ".to_owned()),
        })
    };
    let entries = |enabled: bool, option: bool| {
        let ellipsis = if option { "" } else { "\u{2026}" };
        vec![
            MenuEntry::Heading("HDR".into()),
            item(format!("Merge HDR{ellipsis}"), enabled),
            MenuEntry::Hint("Merge the frames into one HDR photograph in the catalog".into()),
        ]
    };
    iced::widget::row![
        menu_list(entries(true, false)),
        menu_list(entries(true, true)),
        menu_list(entries(false, false))
    ]
    .spacing(theme::SPACING * 2.0)
    .into()
}

/// The grid's states, in gallery order: four pages, each two columns.
pub(crate) fn gallery_select_grid() -> Vec<Element<'static, ()>> {
    vec![
        cells(),
        wrapped(),
        day(),
        previews(),
        ten_thousand(),
        catalog(),
        merge_header(),
        merge_pending(),
        merge_result(),
        merge_failed(),
        merge_expanded(),
        merge_narrow(),
        merge_menu(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::paint_for_tests;

    /// The caption's count is what the grid asks its closure for, drawn at the caption's offset.
    #[test]
    fn the_ten_thousand_state_asks_only_for_the_cells_it_names() {
        let layout = &*TEN_THOUSAND;
        assert_eq!(layout.item_count(), 10_000);
        let scroll = middle(layout);
        let asked = paint_for_tests(
            layout,
            scroll,
            iced::Size::new(WIDTH, TALL),
            ten_thousand_cell,
        );
        let visible: Vec<u32> = layout.visible_cells(scroll, TALL, 0.0).collect();
        assert_eq!(asked, visible);
        assert!(asked.len() < 60, "{} cells", asked.len());
    }

    #[test]
    fn the_small_states_show_whole() {
        for layout in [
            &*CELLS,
            &*WRAPPED,
            &*DAY,
            &*PREVIEWS,
            &*CATALOG,
            &*MERGE_HEADER,
            &*MERGE_PENDING,
            &*MERGE_RESULT,
            &*MERGE_FAILED,
            &*MERGE_EXPANDED,
            &*MERGE_NARROW,
        ] {
            assert_eq!(layout.clamp_scroll(1.0e6, layout.height()), 0.0);
        }
        assert_eq!(CELLS.cell_count(), 10);
        assert_eq!(WRAPPED.frames().len(), 1);
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }
}
