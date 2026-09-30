//! The Select workspace's screen ([event board](../../../../docs/design/catalog/event.png)): its
//! title bar with the workspace switch, the sources panel, the filter bar over the grouped
//! virtualized grid with its floating strip, the Info panel and the status bar, drawn from
//! `state/select.rs`'s model with the Develop workspace's tokens. **Lane D (views and desktop)**
//! owns it.
//!
//! Like every view it reads only its model, and what the app lends it for the frame: the grid's
//! layout, scroll offset and viewport, the rows read so far and the footer labels. The grid asks for
//! the visible cells alone, and draws a cell whose row is not read yet as the placeholder at the
//! shape its header gives, or 3:2.
use crate::{
    app::message::{Message, select::SelectMessage, view::ViewMessage},
    layout::{
        DIVIDER_WIDTH, STATE_PANEL_WIDTH, STATUS_BAR_HEIGHT, TITLE_BAR_HEIGHT, TOOLS_PANEL_WIDTH,
    },
    state::{
        Workspace,
        performance::PerformanceModel,
        select::{
            Availability, CELL_WIDTH_MAX, CELL_WIDTH_MIN, CELL_WIDTH_STEP, ChipModel, Count,
            FilterBarModel, GridContent, InfoModel, ItemInfo, MenuChoice, PickFilter, QueryChange,
            RowCache, SelectMenu, SelectModel, SelectPanel, SelectStatus, SelectTitle,
            SelectionModel, Shown, SourceIcon, SourcePress, SourceRow, SourcesModel, StripModel,
        },
    },
    view::state_panel,
    window_frame,
};
use iced::{
    Alignment, Element, Length, Padding, Size, Theme,
    alignment::Vertical,
    widget::{Column, Space, column, container, mouse_area, row, scrollable, stack, text, tooltip},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, CellAvailability, CellView, ChipEnd, DevelopButtonModel,
    FilterChipModel, FilterOption, FilterSegmentsModel, GridCell, GridLayout, Icon,
    IconButtonModel, LabelledButtonModel, MenuEntry, MenuItem, SearchFieldModel, SelectStripModel,
    SourceCount, SourceHeadingModel, SourceRowModel, Volume, WorkspaceTab, caption, develop_button,
    filter_bar, filter_chip, filter_segments, header_icon_button, labelled_button, menu_list,
    popover, search_field, select_strip, source_heading, source_month, source_row, theme,
    thumbnail_grid, title_bar_icon_button, truncated_text, with_tooltip, workspace_switch,
};

/// The sources panel's search field, as a focus target.
pub(crate) const SEARCH_FIELD: &str = "luxforge.select.search";

/// What is not built yet, as the controls waiting for it say on hover.
const NOT_YET_FOLDERS: &str = "Add a folder\u{2026} comes with indexed folders (not yet available)";
const NOT_YET_UNDO: &str = "Library undo and redo come with picks (not yet available)";
const NOT_YET_DEVELOP: &str = "Developing picks is not yet available";
const NOT_YET_LOUPE: &str = "not yet available";

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
}

/// The workspace switch at a title bar's leading edge, `current` raised. Either segment sends the
/// switch; the app answers it, refusing Select while a Develop draft is open.
pub(crate) fn switch<'a>(current: Shown) -> Element<'a, Message> {
    let tab = match current {
        Shown::Select => WorkspaceTab::Select,
        Shown::Develop => WorkspaceTab::Develop,
    };
    workspace_switch(
        tab,
        Some(|tab| {
            Message::Select(SelectMessage::Switch(match tab {
                WorkspaceTab::Select => Shown::Select,
                WorkspaceTab::Develop => Shown::Develop,
            }))
        }),
    )
}

/// The whole Select screen: title bar, the middle row and the status bar.
pub(crate) fn screen<'a>(model: &'a Workspace, grid: Grid<'a>) -> Element<'a, Message> {
    let select = &model.select;
    let title = container(title_bar(&select.title))
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .style(theme::title_bar_surface);
    let mut middle = row![].height(Length::Fill);
    if select.title.sources_open {
        middle = middle.push(
            container(sources(
                &select.sources,
                &model.performance,
                model.panel.can_interact,
            ))
            .width(Length::Fixed(STATE_PANEL_WIDTH))
            .height(Length::Fill)
            .style(theme::panel_surface),
        );
        middle = middle.push(vertical_divider());
    }
    middle = middle.push(centre(select, grid));
    if select.title.info_open {
        middle = middle.push(vertical_divider());
        middle = middle.push(
            container(info(&select.info))
                .width(Length::Fixed(TOOLS_PANEL_WIDTH))
                .height(Length::Fill)
                .style(theme::panel_surface),
        );
    }
    let status = container(status_bar(&select.status))
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

/// The switch, the view's name and summary; Add a folder…, Undo and Redo (waiting for their lanes),
/// Develop N and the two panel toggles.
fn title_bar(model: &SelectTitle) -> Element<'_, Message> {
    let identity = row![
        switch(Shown::Select),
        text(model.name.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .color(theme::TEXT_BRIGHT)
            .wrapping(text::Wrapping::None),
        truncated_text(
            model.summary.clone(),
            theme::SIZE_IDENTITY,
            theme::FONT,
            theme::TEXT_IDENTITY,
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
                enabled: false,
            },
            None,
        ),
        NOT_YET_FOLDERS.into(),
        tooltip::Position::Bottom,
    );
    let disabled = |icon, tooltip: &str| {
        title_bar_icon_button(
            &IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: false,
                selected: false,
            },
            None,
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
        develop_button(
            &DevelopButtonModel::Ready {
                picks: model.picks as usize,
            },
            None,
        ),
        NOT_YET_DEVELOP.into(),
        tooltip::Position::Bottom,
    );
    let actions = row![
        add_folder,
        disabled(Icon::Undo, NOT_YET_UNDO),
        disabled(Icon::Redo, NOT_YET_UNDO),
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

/// The search field, Events by month, On disk and Catalog scrolling above a rule, and the
/// Performance section pinned under it, as in Develop.
fn sources<'a>(
    model: &'a SourcesModel,
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
    let content = column![
        search,
        events,
        section("On disk", &model.on_disk),
        section("Catalog", &model.catalog),
    ]
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
            .direction(theme::panel_scrollbar())
            .height(Length::Fill),
        rule,
        state_panel::performance(performance, can_interact),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
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

fn source(model: &SourceRow) -> Element<'_, Message> {
    let row = SourceRowModel {
        icon: match model.icon {
            SourceIcon::Event | SourceIcon::AllPhotographs => Icon::Photos,
            SourceIcon::Folder => Icon::Folder,
            SourceIcon::Recent => Icon::Clock,
            SourceIcon::Missing => Icon::Warning,
            SourceIcon::Removed => Icon::Trash,
        },
        name: model.name.clone(),
        secondary: model.secondary.clone(),
        indent: 0,
        disclosure: None,
        volume: model.offline.then_some(Volume::Offline),
        count: match &model.count {
            Count::None => SourceCount::None,
            Count::Total(total) => SourceCount::Total(total.clone()),
            Count::Picks { picked, total } => SourceCount::Picks {
                picked: picked.clone(),
                total: total.clone(),
            },
        },
        selected: model.selected,
        dimmed: model.dimmed,
    };
    let press = match &model.press {
        SourcePress::View(source) => SelectMessage::Source(source.clone()),
        SourcePress::BrowseFolder => SelectMessage::BrowseFolder,
    };
    source_row(&row, Some(Message::Select(press)), None)
}

// -- Centre ----------------------------------------------------------------------------------------

/// The filter bar over the grid, the grid on the canvas surface, a note over it when there is
/// nothing to draw, and the floating strip at its foot.
fn centre<'a>(model: &'a SelectModel, grid: Grid<'a>) -> Element<'a, Message> {
    let Grid {
        layout,
        scroll,
        viewport,
        rows,
        content,
    } = grid;
    let selection = &model.selection;
    let widget = thumbnail_grid(layout, scroll, move |cell: GridCell| {
        cell_view(cell, rows, content, selection)
    })
    .on_press(|press| Message::Select(SelectMessage::Press(press)))
    .on_scroll(|offset| Message::Select(SelectMessage::Scrolled(offset)))
    .viewport(viewport)
    .on_viewport(|size| Message::Select(SelectMessage::Viewport(size)));
    let mut layers = stack![
        container(widget)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::canvas_surface)
    ];
    if let Some(note) = &model.note {
        layers = layers.push(container(caption(note.clone())).center(Length::Fill));
    }
    layers = layers.push(
        container(strip(&model.strip))
            .center_x(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(Padding::default().bottom(theme::SPACING * 1.75)),
    );
    column![
        filters(&model.filter),
        layers.width(Length::Fill).height(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// One visible cell, from its row when it has been read and the session's selection.
fn cell_view<'a>(
    cell: GridCell,
    rows: &'a RowCache,
    content: &'a GridContent,
    selection: &SelectionModel,
) -> CellView<'a> {
    let facts = rows.cell(cell.item).unwrap_or_default();
    CellView {
        // Previews come with the preview lane; until then every cell is the placeholder at its
        // photograph's shape.
        image: None,
        aspect: facts.aspect,
        picked: facts.picked,
        selected: selection.selected(cell.item, cell.span),
        active: selection.active_in(cell.item, cell.span),
        in_catalog: facts.in_catalog,
        availability: match facts.availability {
            Availability::Available => CellAvailability::Available,
            Availability::Offline => CellAvailability::Offline,
            Availability::Unreadable => CellAvailability::Unreadable,
        },
        count: (cell.span > 1).then_some(cell.span),
        label: content.label(cell.item),
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
fn chip<'a>(
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

/// The floating strip: Grid, the Loupe (not yet), the sort and the size slider.
fn strip(model: &StripModel) -> Element<'_, Message> {
    let open = model.sort_menu.is_some();
    select_strip(
        &SelectStripModel {
            loupe_unavailable: Some(NOT_YET_LOUPE.into()),
            sort: model.sort.clone(),
            sort_enabled: model.enabled,
            cell_width: model.cell_width,
            cell_range: (CELL_WIDTH_MIN, CELL_WIDTH_MAX),
            cell_step: CELL_WIDTH_STEP,
        },
        None,
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
                    .color(theme::TEXT_BRIGHT),
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
    if !item.moment.is_empty() {
        content = content.push(band("Moment", &item.moment));
    }
    content.push(band("Metadata", &item.metadata)).into()
}

/// The preview's place at the photograph's shape: previews come with the preview lane.
fn preview_placeholder<'a>(aspect: Option<f32>) -> Element<'a, Message> {
    let width = TOOLS_PANEL_WIDTH - 2.0 * theme::PANEL_PADDING_X;
    let aspect = aspect
        .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
        .unwrap_or(1.5);
    let height = (width / aspect).min(INFO_PREVIEW_MAX_HEIGHT);
    container(Space::new())
        .width(Length::Fixed(height * aspect))
        .height(Length::Fixed(height))
        .style(|_: &Theme| {
            container::Style::default()
                .background(theme::CELL_PLACEHOLDER)
                .border(iced::Border {
                    radius: theme::THUMBNAIL_RADIUS.into(),
                    ..iced::Border::default()
                })
        })
        .into()
}

/// A band: its title, then one label and value per row.
fn band<'a>(title: &str, rows: &'a [(String, String)]) -> Element<'a, Message> {
    let heading = text(title.to_owned())
        .size(theme::SIZE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .color(theme::TEXT_PRIMARY);
    Column::with_children(std::iter::once(heading.into()).chain(rows.iter().map(
        |(label, value)| {
            row![
                text(label.clone())
                    .size(theme::SIZE_CONTROL)
                    .color(theme::TEXT_TERTIARY)
                    .wrapping(text::Wrapping::None)
                    .width(Length::Fixed(INFO_LABEL_WIDTH)),
                truncated_text(
                    value.clone(),
                    theme::SIZE_CONTROL,
                    theme::FONT,
                    theme::TEXT_LABEL
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

/// What last happened with Copy, then the connected agents and the Select line.
fn status_bar(model: &SelectStatus) -> Element<'_, Message> {
    let message = row![
        truncated_text(
            model.message.clone(),
            theme::SIZE_CAPTION,
            theme::FONT,
            theme::TEXT_SECONDARY,
        ),
        header_icon_button(
            &IconButtonModel {
                icon: Icon::Copy,
                tooltip: "Copy the status".into(),
                enabled: true,
                selected: false,
            },
            Some(Message::View(ViewMessage::CopyStatus)),
        ),
    ]
    .spacing(theme::STATUS_SPACING)
    .align_y(Alignment::Center);
    let connected = model.agents_connected;
    let dot = container(Space::new())
        .width(Length::Fixed(theme::STATUS_DOT_SIZE))
        .height(Length::Fixed(theme::STATUS_DOT_SIZE))
        .style(move |_: &Theme| {
            container::Style::default()
                .background(if connected {
                    theme::AGENT_CONNECTED
                } else {
                    theme::TEXT_TERTIARY
                })
                .border(iced::Border {
                    radius: (theme::STATUS_DOT_SIZE / 2.0).into(),
                    ..iced::Border::default()
                })
        });
    let fact = |value: &str| {
        text(value.to_owned())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_TERTIARY)
            .wrapping(text::Wrapping::None)
    };
    let facts = row![
        row![dot, fact(&model.clients)]
            .spacing(theme::STATUS_DOT_SIZE)
            .align_y(Alignment::Center),
        fact(&model.line),
    ]
    .spacing(theme::STATUS_FACT_SPACING)
    .align_y(Alignment::Center);
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
        let _ = screen(
            &workspace,
            Grid {
                layout: &layout,
                scroll: 0.0,
                viewport: Size::ZERO,
                rows: &state.rows,
                content: &state.content,
            },
        );
        let _ = switch(Shown::Develop);
        let _ = switch(Shown::Select);
    }
}
