//! The Select workspace's screen ([event board](../../../../docs/design/catalog/event.png)): its
//! title bar with the workspace switch, the sources panel, the filter bar over the grouped
//! virtualized grid with its floating strip, the Info panel and the status bar, drawn from
//! `state/select.rs`'s model with the Develop workspace's tokens.
//!
//! Like every view it reads only its model, and what the app lends it for the frame: the grid's
//! layout, scroll offset and viewport, the rows read so far and the footer labels. The grid asks for
//! the visible cells alone, and draws a cell whose row is not read yet as the placeholder at the
//! shape its header gives, or 3:2.
use crate::{
    app::{
        loupe::LoupeImages,
        message::{Message, loupe::LoupeMessage, select::SelectMessage, view::ViewMessage},
        select_previews::GridImages,
    },
    layout::{
        DIVIDER_WIDTH, STATE_PANEL_WIDTH, STATUS_BAR_HEIGHT, TITLE_BAR_HEIGHT, TOOLS_PANEL_WIDTH,
    },
    state::{
        Workspace,
        long_work::LongWorkModel,
        performance::PerformanceModel,
        select::{
            Availability, CELL_WIDTH_MAX, CELL_WIDTH_MIN, CELL_WIDTH_STEP, CardNotice, ChipModel,
            Count, Dot, FilterBarModel, ForgetSheet, GridContent, InfoModel, ItemInfo, MenuChoice,
            PickBand, PickFilter, QueryChange, RowCache, RowChoice, SelectMenu, SelectModel,
            SelectPanel, SelectStatus, SelectTitle, SelectionModel, Shown, SourceIcon, SourcePress,
            SourceRow, SourcesModel, StripModel,
        },
    },
    view::state_panel,
    window_frame,
};
use iced::{
    Alignment, Length, Padding, Size,
    alignment::Vertical,
    widget::{Column, Space, column, container, mouse_area, row, scrollable, stack, text, tooltip},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, CatalogSheetModel, CellAvailability, CellView, ChipEnd,
    FilterChipModel, FilterOption, FilterSegmentsModel, GridCell, GridLayout, Icon,
    IconButtonModel, LabelledButtonModel, MenuEntry, MenuItem, NoticeCardModel, SearchFieldModel,
    SelectStripModel, SourceCount, SourceHeadingModel, SourceRowModel, Tone as NoticeTone, Volume,
    WorkspaceTab, caption, catalog_sheet, filter_bar, filter_chip, filter_segments,
    header_icon_button, labelled_button, menu_list, notice_card, popover, search_field,
    select_strip, source_heading, source_month, source_row, theme, thumbnail_grid,
    title_bar_icon_button, truncated_text, with_tooltip, workspace_switch,
};
use luxforge_ui::{Derived, Element, Theme, Token};

/// The sources panel's search field, as a focus target.
pub(crate) const SEARCH_FIELD: &str = "luxforge.select.search";
/// The sources panel's scrolling list, as a scroll target.
pub(crate) const SOURCES_SCROLL: &str = "luxforge.select.sources";

/// Add a folder…'s tooltip, with its shortcut.
const ADD_FOLDER_TOOLTIP: &str =
    "Add a folder, with its subfolders, to the indexed folders (\u{2318}O)";
/// Develop N's tooltip, with its shortcut.
const DEVELOP_TOOLTIP: &str = "Develop the picks in view (\u{2318}\u{21a9})";
/// Why the strip's Loupe cannot be entered without a view.
const NO_LOUPE: &str = "choose a source first";

/// A catalog cell's footer when its photograph is drawn from an approximate rendered preview.
const APPROXIMATE: &str = "approximate";

/// The width of an Info panel row's label.
const INFO_LABEL_WIDTH: f32 = 96.0;
/// The tallest the Info panel's preview placeholder is drawn.
const INFO_PREVIEW_MAX_HEIGHT: f32 = 220.0;

/// What the grid is drawn from this frame, lent by the app: its layout, scroll offset and viewport,
/// the rows read so far and the cells' footer labels.
#[derive(Clone, Copy)]
pub(crate) struct Grid<'a> {
    pub(crate) layout: &'a GridLayout,
    pub(crate) scroll: f32,
    pub(crate) viewport: Size,
    pub(crate) rows: &'a RowCache,
    pub(crate) content: &'a GridContent,
    /// Each cell's decoded preview, borrowed so its handle keeps its id and uploads once.
    pub(crate) images: GridImages<'a>,
    /// The loupe's decoded frames and region, borrowed likewise.
    pub(crate) loupe: LoupeImages<'a>,
}

/// The workspace switch at a title bar's leading edge, `current` raised. Either segment sends the
/// switch; the app answers it, refusing Select while a Develop draft is open.
pub(crate) fn switch<'a>(current: Shown, can_develop: bool) -> Element<'a, Message> {
    let tab = match current {
        Shown::Select => WorkspaceTab::Select,
        Shown::Develop => WorkspaceTab::Develop,
    };
    workspace_switch(
        tab,
        Some(move |tab| {
            if tab == WorkspaceTab::Develop && !can_develop {
                return None;
            }
            Some(Message::Select(SelectMessage::Switch(match tab {
                WorkspaceTab::Select => Shown::Select,
                WorkspaceTab::Develop => Shown::Develop,
            })))
        }),
    )
}

/// The whole Select screen: title bar, the middle row and the status bar.
pub(crate) fn screen<'a>(model: &'a Workspace, grid: Grid<'a>) -> Element<'a, Message> {
    let select = &model.select;
    let title = container(title_bar(&select.title, &model.develop))
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .style(theme::title_bar_surface);
    let mut middle = row![].height(Length::Fill);
    let mut centre = centre(select, grid, &model.long_work);
    // Over the centre: a connected card's notice at its top, and Remove from indexed folders…'s
    // confirmation.
    if let Some(notice) = &select.card_notice {
        centre = stack![
            centre,
            container(card_notice(notice))
                .center_x(Length::Fill)
                .padding(Padding::default().top(theme::SPACING * 2.0)),
        ]
        .into();
    }
    if let Some(sheet) = &select.forget {
        centre = stack![centre, forget_sheet(sheet)].into();
    }
    if select.title.sources_open {
        middle = middle.push(
            container(sources(
                &select.sources,
                &select.catalog.sources,
                &model.performance,
                model.panel.can_interact,
            ))
            .width(Length::Fixed(STATE_PANEL_WIDTH))
            .height(Length::Fill)
            .style(theme::panel_surface),
        );
        middle = middle.push(vertical_divider());
    }
    middle = middle.push(centre);
    if select.title.info_open {
        middle = middle.push(vertical_divider());
        middle = middle.push(
            container(if select.missing.shown {
                crate::view::select_missing::info(&select.missing)
            } else if let Some(photos) = &select.catalog.info {
                crate::view::select_catalog::info(photos, grid.images)
            } else {
                info(&select.info)
            })
            .width(Length::Fixed(TOOLS_PANEL_WIDTH))
            .height(Length::Fill)
            .style(theme::panel_surface),
        );
    }
    // Develop N's confirmation, under the button, over whatever lies below it.
    let middle: Element<'a, Message> = match &model.develop.confirm {
        Some(confirm) => stack![
            middle,
            container(crate::view::develop::confirmation(confirm))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_right(Length::Fill)
                .padding(Padding {
                    top: crate::view::develop::CONFIRM_TOP,
                    right: crate::view::develop::CONFIRM_RIGHT,
                    bottom: 0.0,
                    left: 0.0,
                }),
        ]
        .into(),
        None => middle.into(),
    };
    let status = container(status_bar(
        &select.status,
        &model.long_work,
        select.catalog.status_report,
    ))
    .height(Length::Fixed(STATUS_BAR_HEIGHT))
    .padding([0.0, theme::TITLE_BAR_INSET])
    .align_y(Vertical::Center)
    .style(theme::panel_surface);
    column![
        title,
        horizontal_divider(),
        middle,
        horizontal_divider(),
        status
    ]
    .into()
}

// -- Title bar -------------------------------------------------------------------------------------

/// The switch, the view's name and summary; Add a folder…, Undo and Redo of library changes,
/// Develop N and the two panel toggles.
fn title_bar<'a>(
    model: &'a SelectTitle,
    develop: &'a crate::state::develop::DevelopModel,
) -> Element<'a, Message> {
    let identity = row![
        switch(Shown::Select, develop.can_enter),
        text(model.name.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright))
            .wrapping(text::Wrapping::None),
        truncated_text(
            model.summary.clone(),
            theme::SIZE_IDENTITY,
            theme::FONT,
            Token::TextIdentity,
        ),
    ]
    .spacing(theme::TITLE_GROUP_SPACING)
    .align_y(Alignment::Center);
    let add_folder = with_tooltip(
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
            Some(Message::Select(SelectMessage::AddFolder)),
        ),
        ADD_FOLDER_TOOLTIP.into(),
        tooltip::Position::Bottom,
    );
    let library = |icon, tooltip: &str, message: SelectMessage| {
        title_bar_icon_button(
            &IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: true,
                selected: false,
            },
            Some(Message::Select(message)),
        )
    };
    let toggle = |icon, tooltip: &str, open: bool, panel: SelectPanel| {
        title_bar_icon_button(
            &IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: true,
                selected: open,
            },
            Some(Message::Select(SelectMessage::TogglePanel(panel))),
        )
    };
    let develop = with_tooltip(
        crate::view::develop::develop_n(model.picks, develop),
        DEVELOP_TOOLTIP.into(),
        tooltip::Position::Bottom,
    );
    let actions = row![
        add_folder,
        library(
            Icon::Undo,
            "Undo this desktop's last library change (\u{2318}Z)",
            SelectMessage::Undo,
        ),
        library(
            Icon::Redo,
            "Redo it (\u{21e7}\u{2318}Z)",
            SelectMessage::Redo,
        ),
        develop,
        toggle(
            Icon::StatePanel,
            "Toggle the sources panel (Tab)",
            model.sources_open,
            SelectPanel::Sources,
        ),
        toggle(
            Icon::ToolsPanel,
            "Toggle the Info panel (Tab)",
            model.info_open,
            SelectPanel::Info,
        ),
    ]
    .spacing(theme::TITLE_ACTION_SPACING)
    .align_y(Alignment::Center);
    let bar = row![container(identity).width(Length::Fill), actions]
        .spacing(theme::TITLE_GROUP_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .padding(Padding {
            top: 0.0,
            right: theme::TITLE_BAR_INSET,
            bottom: 0.0,
            left: window_frame::title_bar_leading(model.fullscreen),
        });
    if window_frame::INTEGRATED_TITLE_BAR {
        mouse_area(bar)
            .on_press(Message::View(ViewMessage::DragWindow))
            .into()
    } else {
        bar.into()
    }
}

// -- Sources panel ---------------------------------------------------------------------------------

/// The search field, Cards when one is mounted, Events by month, On disk and Catalog scrolling
/// above a rule, and the Performance section pinned under it, as in Develop.
fn sources<'a>(
    model: &'a SourcesModel,
    catalog: &'a crate::state::select_catalog::CatalogSources,
    performance: &'a PerformanceModel,
    can_interact: bool,
) -> Element<'a, Message> {
    let search = container(search_field(
        &SearchFieldModel {
            id: Some(SEARCH_FIELD.into()),
            placeholder: "Search places, dates, cameras".into(),
            value: model.search.clone(),
            key_hint: None,
            compact: true,
        },
        |text| Message::Select(SelectMessage::Search(text)),
        None,
    ))
    .padding([0.0, 2.0]);
    let mut events = Column::new()
        .spacing(theme::SOURCE_LIST_SPACING)
        .width(Length::Fill)
        .push(source_heading(
            &SourceHeadingModel {
                label: "Events".into(),
                tag: Some("auto".into()),
                add: None,
            },
            None::<Message>,
        ));
    for month in &model.months {
        events = events.push(source_month(&month.label));
        for row in &month.rows {
            events = events.push(source(row));
        }
    }
    if let Some(note) = &model.events_note {
        events =
            events.push(container(caption(note.clone())).padding([2.0, theme::SOURCE_ROW_PADDING]));
    }
    let mut content = column![search];
    if !model.cards.is_empty() {
        content = content.push(section("Cards", &model.cards));
    }
    let content = content
        .push(events)
        .push(on_disk(&model.on_disk, &model.indexed))
        .push(crate::view::select_catalog::catalog_section(
            &model.catalog,
            catalog,
        ))
        .spacing(theme::SOURCE_SECTION_SPACING)
        .padding([theme::PANEL_PADDING_Y, theme::PANEL_PADDING_X])
        .width(Length::Fill);
    let rule = container(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::band_border_surface),
    )
    .padding([0.0, theme::PANEL_PADDING_X]);
    column![
        scrollable(content)
            .id(iced::widget::Id::from(SOURCES_SCROLL))
            .direction(theme::panel_scrollbar())
            .height(Length::Fill),
        rule,
        state_panel::performance(performance, can_interact),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// On disk: its heading with the `+` that adds a folder, the volumes and their open folders, the
/// folder Browse a folder… chose, the indexed folders under their own label, then Browse a folder….
fn on_disk<'a>(rows: &'a [SourceRow], indexed: &'a [SourceRow]) -> Element<'a, Message> {
    let heading = source_heading(
        &SourceHeadingModel {
            label: "On disk".into(),
            tag: None,
            add: Some("Add a folder\u{2026} (\u{2318}O)".into()),
        },
        Some(Message::Select(SelectMessage::AddFolder)),
    );
    // Browse a folder… stays last.
    let (body, browse) = match rows.split_last() {
        Some((last, body)) if last.press == Some(SourcePress::BrowseFolder) => (body, Some(last)),
        _ => (rows, None),
    };
    let mut column = Column::with_children(std::iter::once(heading).chain(body.iter().map(source)))
        .spacing(theme::SOURCE_LIST_SPACING)
        .width(Length::Fill);
    if !indexed.is_empty() {
        column = column.push(source_month("Indexed folders"));
        for row in indexed {
            column = column.push(source(row));
        }
    }
    if let Some(browse) = browse {
        column = column.push(source(browse));
    }
    column.into()
}

fn section<'a>(label: &str, rows: &'a [SourceRow]) -> Element<'a, Message> {
    let heading = source_heading(
        &SourceHeadingModel {
            label: label.into(),
            tag: None,
            add: None,
        },
        None::<Message>,
    );
    Column::with_children(std::iter::once(heading).chain(rows.iter().map(source)))
        .spacing(theme::SOURCE_LIST_SPACING)
        .width(Length::Fill)
        .into()
}

pub(crate) fn source(model: &SourceRow) -> Element<'_, Message> {
    let row = SourceRowModel {
        icon: match model.icon {
            SourceIcon::Event | SourceIcon::AllPhotographs => Icon::Photos,
            SourceIcon::Folder => Icon::Folder,
            SourceIcon::Drive => Icon::Drive,
            SourceIcon::Recent => Icon::Clock,
            SourceIcon::Missing => Icon::Warning,
            SourceIcon::Removed => Icon::Trash,
        },
        name: model.name.clone(),
        secondary: model.secondary.clone(),
        indent: model.indent,
        disclosure: model.disclosure.as_ref().map(|(open, _)| *open),
        volume: model.dot.map(|dot| match dot {
            Dot::Mounted => Volume::Mounted,
            Dot::Offline => Volume::Offline,
        }),
        count: match &model.count {
            Count::None => SourceCount::None,
            Count::Total(total) => SourceCount::Total(total.clone()),
            Count::Picks { picked, total } => SourceCount::Picks {
                picked: picked.clone(),
                total: total.clone(),
            },
            Count::Unavailable(count) => SourceCount::Unavailable(count.clone()),
        },
        selected: model.selected,
        dimmed: model.dimmed,
    };
    let press = model.press.as_ref().map(|press| {
        Message::Select(match press {
            SourcePress::View(source) => SelectMessage::Source(source.clone()),
            SourcePress::Read(source) => SelectMessage::Read(source.clone()),
            SourcePress::Toggle(path) => SelectMessage::Toggle(path.clone()),
            SourcePress::BrowseFolder => SelectMessage::BrowseFolder,
            SourcePress::Menu(path) => SelectMessage::IndexedMenu(path.clone()),
            SourcePress::Forget(path) => SelectMessage::AskForget(Some(path.clone())),
        })
    });
    let toggle = model
        .disclosure
        .as_ref()
        .map(|(_, path)| Message::Select(SelectMessage::Toggle(path.clone())));
    let drawn = source_row(&row, press, toggle);
    let Some(SourcePress::Menu(menu)) = &model.context else {
        return drawn;
    };
    let drawn =
        mouse_area(drawn).on_right_press(Message::Select(SelectMessage::IndexedMenu(menu.clone())));
    popover(
        drawn,
        model.menu.as_deref().map(row_menu),
        Message::Select(SelectMessage::IndexedMenu(None)),
    )
}

/// A source row's menu: each choice, or why it is refused.
fn row_menu(choices: &[RowChoice]) -> Element<'_, Message> {
    menu_list(
        choices
            .iter()
            .map(|choice| {
                MenuEntry::Item(MenuItem {
                    icon: None,
                    label: choice.label.clone(),
                    trailing: None,
                    on_press: choice.press.clone().map(|press| {
                        Message::Select(match press {
                            SourcePress::Forget(path) => SelectMessage::AskForget(Some(path)),
                            SourcePress::Menu(path) => SelectMessage::IndexedMenu(path),
                            SourcePress::View(source) => SelectMessage::Source(source),
                            SourcePress::Read(source) => SelectMessage::Read(source),
                            SourcePress::Toggle(path) => SelectMessage::Toggle(path),
                            SourcePress::BrowseFolder => SelectMessage::BrowseFolder,
                        })
                    }),
                    reason: choice.reason.clone(),
                })
            })
            .collect(),
    )
}

/// Remove from indexed folders…'s confirmation over the centre: Cancel, Escape or a press beside it
/// puts it away; Remove sends `index.remove-folder`.
fn forget_sheet(model: &ForgetSheet) -> Element<'_, Message> {
    let drawn = CatalogSheetModel {
        icon: Icon::Folder,
        title: model.title.clone(),
        note: model.note.clone(),
        sections: Vec::new(),
        dismiss: "Cancel".into(),
        confirm: Some(model.confirm.clone()),
    };
    let close = Message::Select(SelectMessage::AskForget(None));
    let sheet = catalog_sheet(
        &drawn,
        close.clone(),
        Some(Message::Select(SelectMessage::Forget)),
    );
    stack![
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .on_press(close),
        container(sheet).center(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A connected card's notice: what its listing found, with Not now and Browse.
fn card_notice(model: &CardNotice) -> Element<'_, Message> {
    notice_card(
        &NoticeCardModel {
            icon: Icon::Drive,
            title: model.title.clone(),
            body: model.body.clone(),
            tone: NoticeTone::Warning,
        },
        vec![
            (
                "Not now".to_owned(),
                Message::Select(SelectMessage::DismissCard),
            ),
            (
                "Browse".to_owned(),
                Message::Select(SelectMessage::Read(model.browse.clone())),
            ),
        ],
    )
}

// -- Centre ----------------------------------------------------------------------------------------

/// The filter bar over the grid, the grid on the canvas surface, a note over it when there is
/// nothing to draw, and the floating strip at its foot.
fn centre<'a>(
    model: &'a SelectModel,
    grid: Grid<'a>,
    work: &'a LongWorkModel,
) -> Element<'a, Message> {
    if model.loupe.open {
        return crate::view::loupe::loupe(&model.loupe, grid.loupe);
    }
    if model.missing.shown {
        return crate::view::select_missing::centre(&model.missing);
    }
    let Grid {
        layout,
        scroll,
        viewport,
        rows,
        content,
        images,
        ..
    } = grid;
    // The progress sheet of a view with nothing to show yet, in this view only: over the empty
    // canvas of the view that waits, never over the view it is replacing.
    let sheet = crate::view::long_work::sheet(work);
    let canvas: Element<'a, Message> = if sheet.is_some() {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    } else {
        let selection = &model.selection;
        thumbnail_grid(layout, scroll, move |cell: GridCell| {
            cell_view(cell, rows, content, selection, images)
        })
        .on_press(|press| Message::Select(SelectMessage::Press(press)))
        .on_context(|context| Message::Select(SelectMessage::Context(context)))
        .on_moment_action(|moment| Message::Select(SelectMessage::PickAll(moment)))
        .on_scroll(|offset| Message::Select(SelectMessage::Scrolled(offset)))
        .viewport(viewport)
        .on_viewport(|size| Message::Select(SelectMessage::Viewport(size)))
        .into()
    };
    let mut layers = stack![
        container(canvas)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::canvas_surface(theme::CANVAS))
    ];
    match (sheet, &model.note) {
        (Some(sheet), _) => layers = layers.push(container(sheet).center(Length::Fill)),
        (None, Some(note)) => {
            layers = layers.push(container(caption(note.clone())).center(Length::Fill));
        }
        (None, None) => {}
    }
    layers = layers.push(
        container(strip(&model.strip))
            .center_x(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(Padding::default().bottom(theme::SPACING * 1.75)),
    );
    // The selected photographs' menu, where a right-click on the grid opened it.
    if let Some(menu) = &model.catalog.context {
        layers = layers.push(crate::view::select_catalog::photo_menu(menu, viewport));
    }
    // A catalog confirmation or a batch's report, over the grid.
    if let Some(sheet) = &model.catalog.sheet {
        layers = layers.push(crate::view::select_catalog::sheet(sheet));
    }
    // Over the catalog: its own filter bar, and the Metadata browser under it while open.
    let mut regions = match &model.catalog.filter {
        Some(catalog) => column![crate::view::select_catalog::filter_bar(
            catalog,
            &model.filter.kind,
            model.filter.enabled,
        )],
        None => column![filters(&model.filter)],
    };
    if let Some(columns) = &model.catalog.metadata {
        regions = regions.push(crate::view::select_catalog::metadata(columns));
    }
    regions
        .push(layers.width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One visible cell, from its row when it has been read and the session's selection. A collapsed
/// burst shows its pick, or its first frame, with its frame count.
fn cell_view<'a>(
    cell: GridCell,
    rows: &'a RowCache,
    content: &'a GridContent,
    selection: &SelectionModel,
    images: GridImages<'a>,
) -> CellView<'a> {
    let shown = rows.shown(cell.item, cell.span);
    let facts = rows.cell(shown).unwrap_or_default();
    let unreadable = images.unreadable(shown);
    CellView {
        // The decoded preview while the cache holds it; the placeholder at the photograph's shape
        // while it loads.
        image: images.image(shown),
        aspect: facts.aspect,
        picked: facts.picked,
        selected: selection.selected(cell.item, cell.span),
        active: selection.active_in(cell.item, cell.span),
        in_catalog: facts.in_catalog,
        availability: match facts.availability {
            Availability::Available if unreadable => CellAvailability::Unreadable,
            Availability::Available => CellAvailability::Available,
            Availability::Offline => CellAvailability::Offline,
            Availability::Unreadable => CellAvailability::Unreadable,
        },
        count: (cell.span > 1).then_some(cell.span),
        // A bracket frame's exposure step; a photograph drawn from an approximate rendered preview
        // says so, quietly, as the loupe does.
        label: content
            .label(cell.item)
            .or_else(|| images.approximate(shown).then_some(APPROXIMATE)),
        edited: facts.edited,
    }
}

/// All / Picked / Moments without a pick over files, the Camera and Kind chips and, over files, the
/// Group chip; the view's size at the right.
fn filters(model: &FilterBarModel) -> Element<'_, Message> {
    let mut leading = Vec::new();
    if let Some(pick) = &model.pick {
        leading.push(filter_segments(
            &FilterSegmentsModel {
                options: PickFilter::ALL
                    .map(|filter| FilterOption {
                        label: filter.label().into(),
                        count: (filter == PickFilter::Picked)
                            .then(|| pick.picked.clone())
                            .flatten(),
                    })
                    .to_vec(),
                selected: PickFilter::ALL
                    .iter()
                    .position(|filter| *filter == pick.selected)
                    .unwrap_or(0),
                enabled: model.enabled,
            },
            |index| {
                Message::Select(SelectMessage::Change(QueryChange::Pick(
                    PickFilter::ALL[index.min(PickFilter::ALL.len() - 1)],
                )))
            },
        ));
    }
    leading.push(chip(
        &model.camera,
        Some(Icon::Camera),
        SelectMenu::Camera,
        model.enabled,
    ));
    leading.push(chip(&model.kind, None, SelectMenu::Kind, model.enabled));
    if let Some(group) = &model.group {
        leading.push(chip(group, None, SelectMenu::Group, model.enabled));
    }
    filter_bar(leading, vec![caption(model.count.clone())])
}

/// A chip that opens its menu under it; pressing it again, or anywhere else, closes the menu.
pub(crate) fn chip<'a>(
    model: &'a ChipModel,
    icon: Option<Icon>,
    menu: SelectMenu,
    enabled: bool,
) -> Element<'a, Message> {
    let open = model.menu.is_some();
    let anchor = filter_chip(
        &FilterChipModel {
            label: model.label.clone(),
            icon,
            set: model.set,
            end: ChipEnd::Menu,
        },
        enabled.then(|| Message::Select(SelectMessage::Menu((!open).then_some(menu)))),
        None,
    );
    popover(
        anchor,
        model.menu.as_deref().map(menu_view),
        Message::Select(SelectMessage::Menu(None)),
    )
}

/// A chip's or the sort's menu: each choice checked when it is the query's, with its count.
fn menu_view(choices: &[MenuChoice]) -> Element<'_, Message> {
    menu_list(
        choices
            .iter()
            .map(|choice| {
                MenuEntry::Item(MenuItem {
                    icon: choice.checked.then_some(Icon::Check),
                    label: choice.label.clone(),
                    trailing: choice.trailing.clone(),
                    on_press: choice
                        .change
                        .clone()
                        .map(|change| Message::Select(SelectMessage::Change(change))),
                    reason: choice.change.is_none().then(|| choice.label.clone()),
                })
            })
            .collect(),
    )
}

/// The floating strip: Grid, the Loupe, the sort and the size slider.
fn strip(model: &StripModel) -> Element<'_, Message> {
    let open = model.sort_menu.is_some();
    select_strip(
        &SelectStripModel {
            loupe_unavailable: (!model.enabled).then(|| NO_LOUPE.into()),
            sort: model.sort.clone(),
            sort_enabled: model.enabled,
            cell_width: model.cell_width,
            cell_range: (CELL_WIDTH_MIN, CELL_WIDTH_MAX),
            cell_step: CELL_WIDTH_STEP,
        },
        model
            .enabled
            .then_some(Message::Select(SelectMessage::Loupe(LoupeMessage::Open))),
        Some(Message::Select(SelectMessage::Menu(
            (!open).then_some(SelectMenu::Sort),
        ))),
        model.sort_menu.as_deref().map(menu_view),
        Message::Select(SelectMessage::Menu(None)),
        |width| Message::Select(SelectMessage::CellWidth(width)),
    )
}

// -- Info panel ------------------------------------------------------------------------------------

/// The active item's preview placeholder, Moment and Metadata bands; several selected, their count.
fn info(model: &InfoModel) -> Element<'_, Message> {
    let content: Element<'_, Message> = match model {
        InfoModel::Nothing => caption("Nothing selected"),
        InfoModel::Reading => caption("Reading\u{2026}"),
        InfoModel::Several { count, active } => {
            let mut content = column![
                text(count.clone())
                    .size(theme::SIZE_TITLE)
                    .font(theme::FONT_SEMIBOLD)
                    .style(theme::ink(Token::TextBright)),
            ]
            .spacing(theme::ROW_SPACING);
            if let Some(active) = active {
                content = content.push(caption(format!("{active} is active")));
            }
            content.into()
        }
        InfoModel::One(item) => item_view(item),
    };
    scrollable(
        container(content)
            .padding([theme::PANEL_PADDING_Y, theme::PANEL_PADDING_X])
            .width(Length::Fill),
    )
    .direction(theme::panel_scrollbar())
    .height(Length::Fill)
    .into()
}

fn item_view(item: &ItemInfo) -> Element<'_, Message> {
    let mut content =
        column![preview_placeholder(item.aspect)].spacing(theme::PANEL_SECTION_SPACING);
    if let Some(pick) = &item.pick {
        content = content.push(pick_band(pick));
    }
    if !item.moment.is_empty() {
        content = content.push(band("Moment", &item.moment));
    }
    content.push(band("Metadata", &item.metadata)).into()
}

/// The Pick band: "Picked for Develop" in the accent tint once picked, otherwise "Pick", each with
/// `P`, which does the same; and once picked, what that means.
fn pick_band(band: &PickBand) -> Element<'_, Message> {
    let button = labelled_button(
        &LabelledButtonModel {
            label: if band.picked {
                "Picked for Develop"
            } else {
                "Pick"
            }
            .into(),
            icon: band.picked.then_some(Icon::Check),
            key_hint: Some("P".into()),
            tone: if band.picked {
                ButtonTone::Selected
            } else {
                ButtonTone::Control
            },
            size: ButtonSize::Regular,
            fill: true,
            enabled: true,
        },
        Some(Message::Select(SelectMessage::Pick)),
    );
    let mut band_column = column![button].spacing(theme::SPACING);
    if let Some(note) = &band.note {
        band_column = band_column.push(caption(note.clone()));
    }
    band_column.width(Length::Fill).into()
}

/// The preview's place at the photograph's shape: previews come with the preview lane.
pub(crate) fn preview_placeholder<'a>(aspect: Option<f32>) -> Element<'a, Message> {
    let width = TOOLS_PANEL_WIDTH - 2.0 * theme::PANEL_PADDING_X;
    let aspect = aspect
        .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
        .unwrap_or(1.5);
    let height = (width / aspect).min(INFO_PREVIEW_MAX_HEIGHT);
    container(Space::new())
        .width(Length::Fixed(height * aspect))
        .height(Length::Fixed(height))
        .style(|theme: &Theme| {
            container::Style::default()
                .background(Derived::CellPlaceholder.resolve(theme.palette()))
                .border(iced::Border {
                    radius: theme::THUMBNAIL_RADIUS.into(),
                    ..iced::Border::default()
                })
        })
        .into()
}

/// A band: its title, then one label and value per row.
pub(crate) fn band<'a>(title: &str, rows: &'a [(String, String)]) -> Element<'a, Message> {
    let heading = text(title.to_owned())
        .size(theme::SIZE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .style(theme::ink(Token::Text));
    Column::with_children(std::iter::once(heading.into()).chain(rows.iter().map(
        |(label, value)| {
            row![
                text(label.clone())
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::TextTertiary))
                    .wrapping(text::Wrapping::None)
                    .width(Length::Fixed(INFO_LABEL_WIDTH)),
                truncated_text(
                    value.clone(),
                    theme::SIZE_CONTROL,
                    theme::FONT,
                    Token::TextLabel
                ),
            ]
            .align_y(Alignment::Center)
            .into()
        },
    )))
    .spacing(theme::SPACING)
    .width(Length::Fill)
    .into()
}

// -- Status bar ------------------------------------------------------------------------------------

/// What last happened with Copy — and the batch's Report when it is the batch's sentence — then
/// the connected agents and the Select line.
fn status_bar<'a>(
    model: &'a SelectStatus,
    work: &'a LongWorkModel,
    report: bool,
) -> Element<'a, Message> {
    let mut message = row![truncated_text(
        model.message.clone(),
        theme::SIZE_CAPTION,
        theme::FONT,
        Token::TextSecondary,
    )]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center);
    if report {
        message = message.push(crate::view::select_catalog::status_report());
    }
    let message = message.push(header_icon_button(
        &IconButtonModel {
            icon: Icon::Copy,
            tooltip: "Copy the status".into(),
            enabled: true,
            selected: false,
        },
        Some(Message::View(ViewMessage::CopyStatus)),
    ));
    let connected = model.agents_connected;
    let dot = container(Space::new())
        .width(Length::Fixed(theme::STATUS_DOT_SIZE))
        .height(Length::Fixed(theme::STATUS_DOT_SIZE))
        .style(move |theme: &Theme| {
            container::Style::default()
                .background(if connected {
                    theme::AGENT_CONNECTED
                } else {
                    theme.palette().text_tertiary
                })
                .border(iced::Border {
                    radius: (theme::STATUS_DOT_SIZE / 2.0).into(),
                    ..iced::Border::default()
                })
        });
    let fact = |value: &str| {
        text(value.to_owned())
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(Token::TextTertiary))
            .wrapping(text::Wrapping::None)
    };
    let mut facts = row![]
        .spacing(theme::STATUS_FACT_SPACING)
        .align_y(Alignment::Center);
    if let Some(job) = crate::view::long_work::busiest(work) {
        facts = facts.push(job);
    }
    let facts = facts
        .push(
            row![dot, fact(&model.clients)]
                .spacing(theme::STATUS_DOT_SIZE)
                .align_y(Alignment::Center),
        )
        .push(fact(&model.line));
    row![
        container(container(message).width(Length::Shrink)).width(Length::Fill),
        facts,
    ]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center)
    .into()
}

// -- Rules -----------------------------------------------------------------------------------------

fn vertical_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(DIVIDER_WIDTH))
        .height(Length::Fill)
        .style(theme::divider_surface)
        .into()
}

fn horizontal_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(DIVIDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::select::{SelectState, model};

    /// The screen builds with nothing chosen, and the switch both ways.
    #[test]
    fn the_select_screen_builds_before_any_view() {
        let state = SelectState {
            shown: Shown::Select,
            ..SelectState::default()
        };
        let workspace = Workspace {
            select: model(&state, &Default::default(), "", Some(0), false),
            ..Workspace::default()
        };
        let layout = GridLayout::new(Vec::new(), luxforge_ui::GridMetrics::default(), 0.0);
        let previews = crate::app::select_previews::SelectPreviews::default();
        let loupe = crate::app::loupe::Loupe::default();
        let _ = screen(
            &workspace,
            Grid {
                layout: &layout,
                scroll: 0.0,
                viewport: Size::ZERO,
                rows: &state.rows,
                content: &state.content,
                images: previews.grid(&state.rows),
                loupe: loupe.images(&previews),
            },
        );
        let _ = switch(Shown::Develop, true);
        let _ = switch(Shown::Select, false);
    }
}
