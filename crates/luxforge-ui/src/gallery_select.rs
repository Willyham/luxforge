//! Gallery states for the Select workspace's chrome: the sources panel's rows, the filter bar, the
//! title bar's workspace switch and Develop N, long-running work, and the loupe's and the
//! filmstrip's pieces. Widget states only, framed at their real widths (the 240 pt panel, the
//! 330 pt sheet), with every image one of the gallery's stand-in handles made once.

use crate::gallery_thumbnails::{bracket, thumbnail};
use crate::{
    ButtonSize, ButtonTone, ChipEnd, DevelopButtonModel, FilmstripModel, FilterChipModel,
    FilterOption, FilterSegmentsModel, FocusInsetModel, FrameStripModel, Icon, InsetRegion,
    InsetSource, KeyHint, LabelledButtonModel, LoupeInfoModel, MenuEntry, MenuItem, MomentFrame,
    ProgressSheetModel, SearchFieldModel, SourceCount, SourceHeadingModel, SourceRowModel,
    StatusJobModel, Volume, WorkProgress, WorkRowModel, WorkspaceTab, caption, develop_button,
    filmstrip, filmstrip_capacity, filter_action, filter_bar, filter_chip, filter_segments,
    focus_box, focus_inset, frame_strip, key_hints, labelled_button, loupe_info_bar, menu_list,
    popover, progress_sheet, region_box, search_field, source_heading, source_month, source_row,
    status_job, theme, visible_window, work_row, workspace_switch,
};
use crate::{Element, Theme};
use iced::widget::{Column, Space, column, container, image, row, stack};
use iced::{Alignment, ContentFit, Length, Padding, Point, Rectangle, Size};

/// The sources panel's width, and its content's inside the panel's 8 pt padding.
const PANEL_WIDTH: f32 = 240.0;
/// The state panel's width, for the Performance section's rows.
const STATE_PANEL_WIDTH: f32 = 240.0;

/// `content` at `width`, on the panel inside its padding, as the sources and state panels hold it.
fn panel(content: Element<'static, ()>, width: f32) -> Element<'static, ()> {
    container(content)
        .padding([theme::PANEL_PADDING_Y, theme::PANEL_PADDING_X])
        .width(Length::Fixed(width))
        .style(theme::panel_surface)
        .into()
}

/// `content` on the Bar surface, 44 pt tall, as the title bar holds it.
fn title_bar(content: Element<'static, ()>) -> Element<'static, ()> {
    container(content)
        .padding([0.0, theme::TITLE_BAR_INSET])
        .center_y(Length::Fixed(44.0))
        .width(Length::Fill)
        .style(theme::title_bar_surface)
        .into()
}

/// One section of the sources panel: its rows [`theme::SOURCE_LIST_SPACING`] apart.
fn section(rows: Vec<Element<'static, ()>>) -> Element<'static, ()> {
    Column::with_children(rows)
        .spacing(theme::SOURCE_LIST_SPACING)
        .width(Length::Fill)
        .into()
}

fn heading(label: &str, tag: Option<&str>, add: Option<&str>) -> Element<'static, ()> {
    source_heading(
        &SourceHeadingModel {
            label: label.into(),
            tag: tag.map(Into::into),
            add: add.map(Into::into),
        },
        add.map(|_| ()),
    )
}

/// A source row with the given parts; the rest are a plain top-level leaf.
fn source(icon: Icon, name: &str, count: SourceCount) -> SourceRowModel {
    SourceRowModel {
        icon,
        name: name.into(),
        secondary: None,
        indent: 0,
        disclosure: None,
        volume: None,
        count,
        selected: false,
        dimmed: false,
    }
}

fn total(count: &str) -> SourceCount {
    SourceCount::Total(count.into())
}

fn event(name: &str, dates: &str, count: SourceCount) -> SourceRowModel {
    SourceRowModel {
        secondary: Some(dates.into()),
        ..source(Icon::Photos, name, count)
    }
}

fn row_of(model: SourceRowModel) -> Element<'static, ()> {
    let toggle = model.disclosure.map(|_| ());
    source_row(&model, Some(()), toggle)
}

/// The sources panel over files: the search field, a card, the events by month with picks and
/// offline events, and the volumes on disk.
fn sources_over_files() -> Element<'static, ()> {
    let search = container(search_field(
        &SearchFieldModel {
            id: None,
            placeholder: "Search places, dates, cameras".into(),
            value: String::new(),
            key_hint: None,
            compact: true,
        },
        |_| (),
        None,
    ))
    .padding([0.0, 2.0]);
    let cards = section(vec![
        heading("Cards", None, None),
        row_of(source(
            Icon::Drive,
            "NIKON Z 8 \u{b7} SD card",
            total("612"),
        )),
    ]);
    let events = section(vec![
        heading("Events", Some("auto"), None),
        source_month("September 2026"),
        row_of(event("Sa Pa", "14 Sep", total("86"))),
        row_of(SourceRowModel {
            selected: true,
            ..event(
                "Konstanz",
                "12\u{2013}13 Sep",
                SourceCount::Picks {
                    picked: "18".into(),
                    total: "1,042".into(),
                },
            )
        }),
        row_of(event("Brighton", "10 Sep", total("420"))),
        source_month("August 2026"),
        row_of(SourceRowModel {
            volume: Some(Volume::Offline),
            ..event(
                "Reichenau-Niederzell and Mittelzell",
                "22\u{2013}24 Aug",
                total("212"),
            )
        }),
        row_of(SourceRowModel {
            volume: Some(Volume::Offline),
            ..event("Z\u{fc}rich", "3 Aug", total("57"))
        }),
    ]);
    let on_disk = section(vec![
        heading("On disk", None, Some("Add a folder\u{2026}")),
        row_of(SourceRowModel {
            disclosure: Some(false),
            volume: Some(Volume::Mounted),
            ..source(Icon::Drive, "Macintosh HD", SourceCount::None)
        }),
        row_of(SourceRowModel {
            disclosure: Some(false),
            volume: Some(Volume::Offline),
            ..source(Icon::Drive, "Photos SSD", SourceCount::None)
        }),
    ]);
    panel(
        column![search, cards, events, on_disk]
            .spacing(theme::SOURCE_SECTION_SPACING)
            .into(),
        PANEL_WIDTH,
    )
}

/// The sources panel's Catalog: its views, catalog folders by year with one open, collections,
/// Missing originals in the clipping red and Removed dimmed.
fn sources_over_the_catalog() -> Element<'static, ()> {
    let folder = |name: &str, count: &str, selected: bool| {
        row_of(SourceRowModel {
            indent: 1,
            selected,
            ..source(Icon::Folder, name, total(count))
        })
    };
    let catalog = section(vec![
        heading("Catalog", None, Some("New catalog folder")),
        row_of(source(Icon::Photos, "All photographs", total("842"))),
        row_of(source(Icon::Clock, "Recently developed", total("18"))),
        row_of(SourceRowModel {
            disclosure: Some(true),
            ..source(Icon::FolderGroup, "2026", SourceCount::None)
        }),
        folder("Konstanz \u{b7} Sep 2026", "18", true),
        folder("Sa Pa \u{b7} Sep 2026", "6", false),
        folder("Brighton \u{b7} Sep 2026", "31", false),
        row_of(SourceRowModel {
            disclosure: Some(false),
            ..source(Icon::FolderGroup, "2025", SourceCount::None)
        }),
        source_month("Collections"),
        row_of(source(Icon::Collection, "Portfolio", total("58"))),
        row_of(source(
            Icon::SmartCollection,
            "Edited this month",
            total("12"),
        )),
        row_of(source(
            Icon::Warning,
            "Missing originals",
            SourceCount::Unavailable("212".into()),
        )),
        row_of(SourceRowModel {
            dimmed: true,
            ..source(Icon::Trash, "Removed", total("4"))
        }),
    ]);
    panel(catalog, PANEL_WIDTH)
}

/// The search fields empty and typed: the sources panel's, and the catalog filter bar's with its
/// key.
fn search_fields() -> Element<'static, ()> {
    let search = |placeholder: &str, value: &str, compact: bool| {
        search_field(
            &SearchFieldModel {
                id: None,
                placeholder: placeholder.into(),
                value: value.into(),
                key_hint: (!compact).then(|| "\u{2318}F".into()),
                compact,
            },
            |_| (),
            Some(()),
        )
    };
    column![
        row![
            container(search("Search places, dates, cameras", "", true))
                .width(Length::Fixed(PANEL_WIDTH - 2.0 * theme::PANEL_PADDING_X)),
            container(search("Search places, dates, cameras", "Konst", true))
                .width(Length::Fixed(PANEL_WIDTH - 2.0 * theme::PANEL_PADDING_X)),
        ]
        .spacing(theme::SPACING),
        row![
            search("Search photographs", "", false),
            search("Search photographs", "Summilux", false),
        ]
        .spacing(theme::SPACING),
    ]
    .spacing(theme::SPACING)
    .into()
}

fn chip(label: &str, icon: Option<Icon>, set: bool, end: ChipEnd) -> Element<'static, ()> {
    filter_chip(
        &FilterChipModel {
            label: label.into(),
            icon,
            set,
            end,
        },
        Some(()),
        (end == ChipEnd::Clear).then_some(()),
    )
}

fn pick_filter(selected: usize) -> Element<'static, ()> {
    let option = |label: &str, count: Option<&str>| FilterOption {
        label: label.into(),
        count: count.map(Into::into),
    };
    filter_segments(
        &FilterSegmentsModel {
            options: vec![
                option("All", None),
                option("Picked", Some("18")),
                option("Moments without a pick", None),
            ],
            selected,
            enabled: true,
        },
        |_| (),
    )
}

/// The filter bar over files: All chosen with every chip at rest, then Picked chosen with the
/// camera set and cleared by its cross; each bar's count at its right.
fn filter_bar_over_files() -> Element<'static, ()> {
    column![
        filter_bar(
            vec![
                pick_filter(0),
                chip("Camera", Some(Icon::Camera), false, ChipEnd::Menu),
                chip("Kind", None, false, ChipEnd::Menu),
                chip(
                    "Group: Day \u{203a} Camera \u{203a} Moment",
                    None,
                    false,
                    ChipEnd::Menu
                ),
            ],
            Vec::new(),
        ),
        filter_bar(
            vec![
                pick_filter(1),
                chip("Nikon Z 8", Some(Icon::Camera), true, ChipEnd::Clear),
                chip("RAW", None, true, ChipEnd::Menu),
            ],
            vec![caption("18 picked \u{b7} 9 moments")],
        ),
    ]
    .spacing(theme::SPACING)
    .into()
}

/// The filter bar over the catalog: the search field, Kind at rest, Edited set with its cross and
/// a date set with its menu; then the bar's action and count.
fn filter_bar_over_the_catalog() -> Element<'static, ()> {
    column![
        filter_bar(
            vec![
                search_field(
                    &SearchFieldModel {
                        id: None,
                        placeholder: "Search photographs".into(),
                        value: String::new(),
                        key_hint: Some("\u{2318}F".into()),
                        compact: false,
                    },
                    |_| (),
                    None,
                ),
                chip("Kind", None, false, ChipEnd::Menu),
                chip("Edited", None, true, ChipEnd::Clear),
                chip("September 2026", Some(Icon::Camera), true, ChipEnd::Menu),
            ],
            Vec::new(),
        ),
        filter_bar(
            vec![chip("Kind", None, false, ChipEnd::Menu)],
            vec![
                filter_action(
                    "Save as smart collection\u{2026}",
                    Some(Icon::SmartCollection),
                    Some(()),
                ),
                caption("9 of 55"),
            ],
        ),
    ]
    .spacing(theme::SPACING)
    .into()
}

/// The Group chip with its menu dropped under it: the three groupings, the chosen one checked, and
/// the thresholds.
fn group_menu_open() -> Element<'static, ()> {
    let item = |label: &str, chosen: bool| {
        MenuEntry::Item(MenuItem {
            icon: chosen.then_some(Icon::Check),
            label: label.into(),
            trailing: None,
            on_press: Some(()),
            reason: None,
        })
    };
    let menu = menu_list(vec![
        item("Day \u{203a} Camera \u{203a} Moment", true),
        item("Day", false),
        item("None", false),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem {
            icon: None,
            label: "Moment thresholds\u{2026}".into(),
            trailing: None,
            on_press: Some(()),
            reason: None,
        }),
    ]);
    container(popover(
        chip(
            "Group: Day \u{203a} Camera \u{203a} Moment",
            None,
            false,
            ChipEnd::Menu,
        ),
        Some(menu),
        (),
    ))
    .padding(Padding::default().bottom(150.0))
    .into()
}

/// The workspace switch in the title bar both ways, each with Add a folder… and Develop N at the
/// bar's trailing end.
fn switch_both_ways() -> Element<'static, ()> {
    let bar = |current| {
        title_bar(
            row![
                workspace_switch(current, Some(|_| Some(()))),
                Space::new().width(Length::Fill),
                labelled_button(
                    &LabelledButtonModel {
                        label: "Add a folder\u{2026}".into(),
                        icon: Some(Icon::Folder),
                        key_hint: None,
                        tone: ButtonTone::Control,
                        size: ButtonSize::Regular,
                        fill: false,
                        enabled: true,
                    },
                    Some(()),
                ),
                develop_button(&DevelopButtonModel::Ready { picks: 18 }, Some(())),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center)
            .into(),
        )
    };
    column![bar(WorkspaceTab::Select), bar(WorkspaceTab::Develop)]
        .spacing(theme::SPACING)
        .into()
}

/// Develop N ready, busy and with nothing picked, on the title bar's surface.
fn develop_states() -> Element<'static, ()> {
    title_bar(
        row![
            develop_button(&DevelopButtonModel::Ready { picks: 18 }, Some(())),
            develop_button(&DevelopButtonModel::Busy { done: 7, total: 18 }, Some(())),
            develop_button(&DevelopButtonModel::Ready { picks: 0 }, Some(())),
            develop_button(&DevelopButtonModel::Ready { picks: 1042 }, Some(())),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center)
        .into(),
    )
}

/// The status bar's busiest job: with a total and a second job, with no truthful total, and alone.
fn status_jobs() -> Element<'static, ()> {
    let job = |label: &str, fraction, jobs| {
        status_job(
            &StatusJobModel {
                label: label.into(),
                fraction,
                jobs,
            },
            (),
        )
    };
    column![
        job("Indexing ~/Pictures", Some(0.24), 2),
        job("Reading the NIKON Z 8 card", None, 2),
        job("Previews \u{b7} Konstanz", Some(0.614), 1),
    ]
    .spacing(theme::ROW_SPACING)
    .into()
}

/// The Performance section's stoppable jobs in the state panel: a walk with its count and estimate,
/// a preview lane without an estimate yet, and work that can say nothing yet.
fn performance_rows() -> Element<'static, ()> {
    let job = |label: &str, estimate: Option<&str>, count: Option<&str>, fraction| {
        work_row(
            &WorkRowModel {
                label: label.into(),
                estimate: estimate.map(Into::into),
                progress: WorkProgress {
                    count: count.map(Into::into),
                    fraction,
                },
            },
            Some(()),
        )
    };
    panel(
        column![
            job(
                "Indexing ~/Pictures",
                Some("about 1 min 40 s"),
                Some("48,210 of about 200,000 files"),
                Some(0.24),
            ),
            job(
                "Previews",
                Some("about 12 s"),
                Some("640 of 1,042"),
                Some(0.614)
            ),
            job("Finding missing originals", None, None, None),
        ]
        .into(),
        STATE_PANEL_WIDTH,
    )
}

fn sheet(
    count: Option<&str>,
    fraction: Option<f32>,
    estimate: Option<&str>,
) -> Element<'static, ()> {
    progress_sheet(
        &ProgressSheetModel {
            icon: Icon::Drive,
            title: "Reading the NIKON Z 8 card".into(),
            note: "The first look at a card or folder reads every file\u{2019}s header. Frames \
                   appear as soon as it is done; previews follow."
                .into(),
            progress: WorkProgress {
                count: count.map(Into::into),
                fraction,
            },
            estimate: estimate.map(Into::into),
        },
        (),
        (),
    )
}

/// The loupe's info bar.
fn info_bar() -> Element<'static, ()> {
    loupe_info_bar(&LoupeInfoModel {
        moment: "Moment 4 of 37 \u{b7} burst".into(),
        frame: "Frame 3 of 6 \u{b7} +0.52 s".into(),
        exposure: Some("1/2000 s \u{b7} f/5.6 \u{b7} ISO 100".into()),
        source: "Camera preview \u{b7} 8368 \u{d7} 5584".into(),
    })
}

/// A window of `capacity` frames of a `total`-frame burst around `active`, the picked frame
/// checked; every frame the same scene, as a burst's are.
fn frames(total: usize, active: usize, picked: usize, capacity: usize) -> Element<'static, ()> {
    let window = visible_window(total, active, capacity);
    let first = window.start;
    frame_strip(
        &FrameStripModel {
            frames: window
                .map(|index| MomentFrame {
                    image: Some(thumbnail(0)),
                    picked: index == picked,
                })
                .collect(),
            first,
            active: Some(active),
        },
        |_| (),
        Some(()),
        Some(()),
    )
}

/// The 100% inset from a full-size camera preview, and from a Luxforge development, which says so.
fn insets() -> Element<'static, ()> {
    row![
        focus_inset(&FocusInsetModel {
            region: Some(InsetRegion {
                handle: thumbnail(0),
                size: Size::new(theme::FOCUS_INSET_WIDTH, theme::FOCUS_REGION_HEIGHT),
            }),
            source: InsetSource::CameraPreview,
        }),
        focus_inset(&FocusInsetModel {
            region: Some(InsetRegion {
                handle: bracket(1),
                size: Size::new(theme::FOCUS_INSET_WIDTH, theme::FOCUS_REGION_HEIGHT),
            }),
            source: InsetSource::Development,
        }),
    ]
    .spacing(theme::SPACING)
    .into()
}

/// The pointer's region box over a photograph on the loupe's canvas, placed by [`region_box`].
fn region_over_the_loupe() -> Element<'static, ()> {
    let size = Size::new(300.0, 200.0);
    let region = region_box(
        Point::new(186.0, 104.0),
        Rectangle::new(Point::ORIGIN, size),
        Size::new(48.0, 33.0),
    );
    container(stack![
        image(thumbnail(0))
            .width(Length::Fixed(size.width))
            .height(Length::Fixed(size.height))
            .content_fit(ContentFit::Fill),
        container(focus_box(region.size())).padding(Padding {
            top: region.y,
            left: region.x,
            ..Padding::ZERO
        }),
    ])
    .padding(theme::SPACING)
    .style(|_: &Theme| container::Style::default().background(theme::CANVAS))
    .into()
}

fn hints() -> Element<'static, ()> {
    let hint = |key: &str, action: &str| KeyHint {
        key: key.into(),
        action: action.into(),
    };
    key_hints(&[
        hint("\u{2190} \u{2192}", "frames"),
        hint("1\u{2013}6", "jump"),
        hint("P", "pick"),
        hint("Z", "100%"),
        hint("\u{2191} \u{2193}", "moments"),
        hint("C", "side by side"),
        hint("Tab", "panels"),
        hint("Esc", "grid"),
    ])
}

/// The development set's filmstrip at the card's width: as many cells as fit from `first`, the
/// active one outlined, and those without a preview yet empty.
fn strip(first: usize, total: usize, active: usize, loading: &[usize]) -> Element<'static, ()> {
    // The gallery's card is about 684 pt wide; the strip holds what fits there.
    let capacity = filmstrip_capacity(684.0);
    let window = first..(first + capacity).min(total);
    filmstrip(
        &FilmstripModel {
            title: "Development set".into(),
            caption: None,
            cells: window
                .map(|index| (!loading.contains(&index)).then(|| thumbnail(index)))
                .collect(),
            first,
            total,
            active: Some(active),
        },
        |_| (),
        (active > 0).then_some(()),
        (active + 1 < total).then_some(()),
    )
}

/// The Select chrome's states, in gallery order.
pub(crate) fn gallery_select() -> Vec<Element<'static, ()>> {
    vec![
        // -- Select sources and filters.
        row![sources_over_files(), sources_over_the_catalog()]
            .spacing(theme::SPACING)
            .into(),
        filter_bar_over_files(),
        search_fields(),
        filter_bar_over_the_catalog(),
        group_menu_open(),
        // -- Select title bar and long-running work.
        switch_both_ways(),
        develop_states(),
        status_jobs(),
        performance_rows(),
        sheet(
            Some("312 of 612 files"),
            Some(312.0 / 612.0),
            Some("about 4 s left"),
        ),
        sheet(None, None, None),
        // -- Select loupe and filmstrip.
        info_bar(),
        frames(6, 2, 2, 4),
        insets(),
        frames(1000, 499, 500, 4),
        region_over_the_loupe(),
        strip(0, 18, 0, &[]),
        hints(),
        strip(40, 120, 42, &[45, 46]),
    ]
}
