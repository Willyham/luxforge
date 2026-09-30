//! The catalog's regions in the Select workspace ([catalog board](../../../../docs/design/catalog/catalog.png)):
//! the Catalog sources' folders by year and collections with their menus and names typed in place,
//! the filter bar over the catalog with Save as smart collection… and the view's count, the
//! Metadata browser, and the Info panel over photographs (one photograph's Organize band, or the
//! batch form), drawn from `state/select_catalog.rs`'s model. **Lane D (views and desktop)** owns
//! it.
//!
//! Like every view it reads only its model and what the app lends it for the frame: the grid's
//! decoded previews, which the Info panel borrows for the photographs it describes.
use crate::{
    app::{
        message::{Message, select::SelectMessage, select_catalog::CatalogMessage},
        select_previews::GridImages,
    },
    layout::TOOLS_PANEL_WIDTH,
    state::{
        select::{ChipModel, SelectMenu, SourceRow},
        select_catalog::{
            ActionChoice, CatalogAction, CatalogFilterBar, CatalogIcon, CatalogMenu, CatalogRow,
            CatalogSources, ConditionGlyph, FacetColumnModel, NOT_YET_EXPORT, NOT_YET_PRESETS,
            NamingTarget, OrganizeChip, PhotoInfo,
        },
    },
    view::select::{band, chip, preview_placeholder, source},
};
use iced::{
    Alignment, ContentFit, Element, Length, Padding,
    alignment::Horizontal,
    widget::{
        Column, Row, Space, column, container, image, mouse_area, row, scrollable, text,
        text::{LineHeight, Wrapping},
        text_input, tooltip,
    },
};
use luxforge_ui::{
    ButtonSize, ButtonTone, ChipEnd, FacetColumnModel as ColumnWidget, FacetRowModel,
    FilterChipModel, Icon, LabelledButtonModel, MenuEntry, MenuItem, OrganizeChipModel,
    OrganizeTone, SearchFieldModel, SourceCount, SourceHeadingModel, SourceRowModel, caption,
    facet_column, filter_action, filter_bar as bar, filter_chip, labelled_button, menu_list,
    metadata_browser, organize_chip, organize_chips, popover, search_field, source_heading,
    source_month, source_row, theme, with_tooltip,
};

/// The catalog's search field, as a focus target for `Cmd+F`.
pub(crate) const SEARCH_FIELD: &str = "luxforge.select.catalog.search";
/// The field a name is typed in: a new folder's or collection's, a rename, a smart collection's.
pub(crate) const NAMING_FIELD: &str = "luxforge.select.catalog.name";

/// The tallest a catalog menu grows before it scrolls: a long list of folders.
const MENU_MAX_HEIGHT: f32 = 360.0;
/// The Info panel's preview box, as the event board draws it: 276 × 176 pt.
const PREVIEW_BOX: (f32, f32) = (276.0, 176.0);
/// The width of an Organize band's labels: the Metadata band's, so the bands' values line up.
const ORGANIZE_LABEL_WIDTH: f32 = 96.0;

fn act(action: CatalogAction) -> Message {
    Message::Select(SelectMessage::Catalog(CatalogMessage::Act(action)))
}

fn close() -> Message {
    act(CatalogAction::Menu(None))
}

/// A catalog menu: each choice with its check, count or reason, scrolling past
/// [`MENU_MAX_HEIGHT`].
fn menu(choices: &[ActionChoice]) -> Element<'_, Message> {
    let mut entries = Vec::with_capacity(choices.len());
    for choice in choices {
        if choice.separated {
            entries.push(MenuEntry::Separator);
        }
        entries.push(MenuEntry::Item(MenuItem {
            icon: choice.checked.then_some(Icon::Check),
            label: choice.label.clone(),
            trailing: choice.trailing.clone(),
            on_press: choice.action.clone().map(act),
            reason: choice.reason.clone(),
        }));
    }
    container(scrollable(menu_list(entries)).direction(theme::panel_scrollbar()))
        .max_height(MENU_MAX_HEIGHT)
        .into()
}

/// The field a name is typed in, Return saving it.
fn naming_input<'a>(placeholder: &str, value: &str) -> Element<'a, Message> {
    text_input(placeholder, value)
        .id(iced::widget::Id::from(NAMING_FIELD))
        .on_input(|text| act(CatalogAction::NameText(text)))
        .on_submit(act(CatalogAction::Submit))
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(
            (theme::RENAME_INPUT_HEIGHT - 2.0 * theme::FIELD_PADDING_Y).into(),
        ))
        .padding(Padding {
            top: theme::FIELD_PADDING_Y,
            right: theme::FIELD_INSET,
            bottom: theme::FIELD_PADDING_Y,
            left: theme::FIELD_INSET,
        })
        .align_x(Horizontal::Left)
        .width(Length::Fill)
        .style(theme::field_input_style(false))
        .into()
}

// -- Sources ---------------------------------------------------------------------------------------

/// The Catalog section: its heading with the `+` for a new folder or collection, All photographs
/// and Recently developed, the folders by year, the collections, then Missing originals and
/// Removed.
pub(crate) fn catalog_section<'a>(
    shell: &'a [SourceRow],
    model: &'a CatalogSources,
) -> Element<'a, Message> {
    let open = model.add_menu.is_some();
    let heading = popover(
        source_heading(
            &SourceHeadingModel {
                label: "Catalog".into(),
                tag: None,
                add: Some("New folder or collection".into()),
            },
            Some(act(CatalogAction::Menu(
                (!open).then_some(CatalogMenu::Add),
            ))),
        ),
        model.add_menu.as_deref().map(menu),
        close(),
    );
    let split = shell.len().min(2);
    let mut rows = Column::new()
        .spacing(theme::SOURCE_LIST_SPACING)
        .width(Length::Fill)
        .push(heading);
    for row in &shell[..split] {
        rows = rows.push(source(row));
    }
    for row in &model.folders {
        rows = rows.push(catalog_row(row));
    }
    if let Some(note) = &model.note {
        rows =
            rows.push(container(caption(note.clone())).padding([2.0, theme::SOURCE_ROW_PADDING]));
    }
    if !model.collections.is_empty() {
        rows = rows.push(source_month("Collections"));
        for row in &model.collections {
            rows = rows.push(catalog_row(row));
        }
    }
    for row in &shell[split..] {
        rows = rows.push(source(row));
    }
    rows.into()
}

/// One folder, year or collection row: pressed to view it (or open it), right-clicked for its
/// menu, or a name being typed in its place.
fn catalog_row(model: &CatalogRow) -> Element<'_, Message> {
    let glyph = match model.icon {
        CatalogIcon::Year => Icon::FolderGroup,
        CatalogIcon::Folder => Icon::Folder,
        CatalogIcon::Collection => Icon::Collection,
        CatalogIcon::Smart => Icon::SmartCollection,
        CatalogIcon::Group => Icon::FolderGroup,
    };
    if let Some(typed) = &model.naming {
        let inset =
            theme::SOURCE_ROW_PADDING + theme::SOURCE_INDENT * f32::from(model.indent) + 12.0;
        return container(naming_input(naming_placeholder(glyph), typed))
            .padding(Padding {
                top: 1.0,
                right: theme::SOURCE_ROW_PADDING,
                bottom: 1.0,
                left: inset,
            })
            .width(Length::Fill)
            .into();
    }
    let drawn = SourceRowModel {
        icon: glyph,
        name: model.name.clone(),
        secondary: None,
        indent: model.indent,
        disclosure: model.open,
        volume: None,
        count: model
            .count
            .clone()
            .map_or(SourceCount::None, SourceCount::Total),
        selected: model.selected,
        dimmed: false,
    };
    let row = source_row(
        &drawn,
        model.press.clone().map(act),
        model.toggle.clone().map(act),
    );
    let row: Element<'_, Message> = match &model.context {
        Some(context) => mouse_area(row).on_right_press(act(context.clone())).into(),
        None => row,
    };
    popover(row, model.menu.as_deref().map(menu), close())
}

// -- Filter bar and Metadata browser ---------------------------------------------------------------

/// The filter bar over the catalog: the search (`Cmd+F`), Metadata, the shell's Kind chip, Edited,
/// each metadata condition set in the accent with its clear ✕; Save as smart collection… and the
/// view's count at the right.
pub(crate) fn filter_bar<'a>(
    model: &'a CatalogFilterBar,
    kind: &'a ChipModel,
    enabled: bool,
) -> Element<'a, Message> {
    let search = search_field(
        &SearchFieldModel {
            id: Some(SEARCH_FIELD.into()),
            placeholder: "Search photographs".into(),
            value: model.search.clone(),
            key_hint: Some("\u{2318}F".into()),
            compact: false,
        },
        |text| act(CatalogAction::Search(text)),
        None,
    );
    let metadata = filter_chip(
        &FilterChipModel {
            label: "Metadata".into(),
            icon: Some(Icon::Sliders),
            set: false,
            end: ChipEnd::Menu,
        },
        enabled.then(|| act(CatalogAction::Metadata)),
        None,
    );
    let edited_open = model.edited.menu.is_some();
    let edited = popover(
        filter_chip(
            &FilterChipModel {
                label: model.edited.label.clone(),
                icon: None,
                set: model.edited.set,
                end: if model.edited.set {
                    ChipEnd::Clear
                } else {
                    ChipEnd::Menu
                },
            },
            enabled.then(|| {
                act(CatalogAction::Menu(
                    (!edited_open).then_some(CatalogMenu::Edited),
                ))
            }),
            model.edited.set.then(|| {
                act(CatalogAction::Change(
                    crate::state::select_catalog::CatalogChange::Edited(None),
                ))
            }),
        ),
        model.edited.menu.as_deref().map(menu),
        close(),
    );
    let mut leading = vec![
        search,
        metadata,
        chip(kind, None, SelectMenu::Kind, enabled),
        edited,
    ];
    for condition in &model.conditions {
        leading.push(filter_chip(
            &FilterChipModel {
                label: condition.label.clone(),
                icon: match condition.glyph {
                    ConditionGlyph::Date => Some(Icon::Clock),
                    ConditionGlyph::Camera => Some(Icon::Camera),
                    ConditionGlyph::Place | ConditionGlyph::Lens => None,
                },
                set: true,
                end: ChipEnd::Clear,
            },
            (!model.metadata_open).then(|| act(CatalogAction::Metadata)),
            Some(act(condition.clear.clone())),
        ));
    }
    let save_press = model
        .save_refused
        .is_none()
        .then(|| act(CatalogAction::Name(NamingTarget::SmartCollection)));
    let save = filter_action(
        "Save as smart collection\u{2026}",
        Some(Icon::SmartCollection),
        save_press,
    );
    let save = match &model.save_refused {
        Some(reason) => with_tooltip(save, reason.clone(), tooltip::Position::Bottom),
        None => save,
    };
    let save = popover(save, model.naming.as_deref().map(smart_naming), close());
    let count = text(model.count.clone())
        .size(theme::SIZE_CAPTION)
        .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
        .color(theme::TEXT_TERTIARY)
        .wrapping(Wrapping::None);
    bar(leading, vec![save, count.into()])
}

/// The smart collection's name, typed under Save as smart collection…: Return or Save saves the
/// view's query under it.
fn smart_naming(typed: &str) -> Element<'_, Message> {
    let button = |label: &str, tone, message| {
        labelled_button(
            &LabelledButtonModel {
                label: label.into(),
                icon: None,
                key_hint: None,
                tone,
                size: ButtonSize::Regular,
                fill: false,
                enabled: true,
            },
            Some(message),
        )
    };
    container(
        column![
            caption("Save this view as a smart collection".to_owned()),
            naming_input("Name", typed),
            row![
                Space::new().width(Length::Fill),
                button("Cancel", ButtonTone::Control, close()),
                button("Save", ButtonTone::Primary, act(CatalogAction::Submit)),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        ]
        .spacing(theme::SPACING * 1.5),
    )
    .padding(theme::MENU_PADDING * 2.0)
    .width(Length::Fixed(theme::MENU_WIDTH * 1.4))
    .style(theme::menu_surface)
    .into()
}

/// The Metadata browser: Date, Place, Camera and Lens with their counts, each value pressed to
/// narrow the view to it.
pub(crate) fn metadata(columns: &[FacetColumnModel]) -> Element<'_, Message> {
    metadata_browser(
        columns
            .iter()
            .map(|column| {
                let drawn = ColumnWidget {
                    title: column.title.to_owned(),
                    rows: column
                        .rows
                        .iter()
                        .map(|row| FacetRowModel {
                            label: row.label.clone(),
                            count: row.count.clone(),
                            indent: row.indent,
                            selected: row.selected,
                            enabled: row.change.is_some(),
                        })
                        .collect(),
                    note: column.note.clone(),
                };
                facet_column(&drawn, move |index| {
                    column
                        .rows
                        .get(index)
                        .and_then(|row| row.change.clone())
                        .map_or_else(close, |change| act(CatalogAction::Change(change)))
                })
            })
            .collect(),
    )
}

// -- Info panel ------------------------------------------------------------------------------------

/// The Info panel over photographs: the preview (or the selection's previews), the title, the
/// Organize band with Move to… and Add to…, the Metadata band and the Develop band.
pub(crate) fn info<'a>(model: &'a PhotoInfo, images: GridImages<'a>) -> Element<'a, Message> {
    let mut content = Column::new().spacing(theme::PANEL_SECTION_SPACING);
    if model.count <= 1 {
        let preview: Element<'a, Message> = match model
            .previews
            .first()
            .and_then(|position| images.image(*position))
        {
            Some(handle) => container(
                image(handle.clone())
                    .content_fit(ContentFit::Contain)
                    .width(Length::Fixed(PREVIEW_BOX.0))
                    .height(Length::Fixed(PREVIEW_BOX.1)),
            )
            .center_x(Length::Fill)
            .into(),
            None => preview_placeholder(model.aspect),
        };
        content = content.push(preview).push(
            text(model.title.clone())
                .size(theme::SIZE_TITLE)
                .font(theme::FONT_SEMIBOLD)
                .color(theme::TEXT_BRIGHT)
                .wrapping(Wrapping::None),
        );
    } else {
        let thumbs: Vec<Element<'a, Message>> = model
            .previews
            .iter()
            .filter_map(|position| images.image(*position))
            .map(|handle| {
                image(handle.clone())
                    .content_fit(ContentFit::Contain)
                    .width(Length::FillPortion(1))
                    .height(Length::Fixed(theme::BATCH_PREVIEW_HEIGHT))
                    .into()
            })
            .collect();
        if !thumbs.is_empty() {
            content = content.push(
                Row::with_children(thumbs)
                    .spacing(theme::BATCH_PREVIEW_SPACING)
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
            );
        }
        let mut title = row![
            text(model.title.clone())
                .size(theme::SIZE_TITLE)
                .font(theme::FONT_SEMIBOLD)
                .color(theme::TEXT_BRIGHT)
                .wrapping(Wrapping::None)
        ]
        .spacing(theme::SPACING * 2.0)
        .align_y(Alignment::Center);
        if let Some(active) = &model.active {
            title = title.push(caption(format!("{active} is active")));
        }
        content = content.push(title);
    }
    content = content.push(organize(model));
    if !model.metadata.is_empty() {
        content = content.push(band("Metadata", &model.metadata));
    }
    content = content.push(develop(model));
    scrollable(
        container(content)
            .padding([theme::PANEL_PADDING_Y, theme::PANEL_PADDING_X])
            .width(Length::Fixed(TOOLS_PANEL_WIDTH)),
    )
    .direction(theme::panel_scrollbar())
    .height(Length::Fill)
    .into()
}

/// A folder's or collection's chip, with its menu under it while open.
fn organize_chip_view(chip: &OrganizeChip) -> Element<'_, Message> {
    let drawn = OrganizeChipModel {
        icon: Some(if chip.collection {
            Icon::Collection
        } else {
            Icon::Folder
        }),
        label: chip.label.clone(),
        count: chip.partial.clone(),
        tone: if chip.partial.is_some() {
            OrganizeTone::Partial
        } else {
            OrganizeTone::Member
        },
    };
    popover(
        organize_chip(&drawn, chip.press.clone().map(act)),
        chip.menu.as_deref().map(menu),
        close(),
    )
}

/// An action in a chip's shape (`Move to…`, `+ Add to…`), with its menu under it while open.
fn organize_action<'a>(
    label: &str,
    open: Option<&'a [ActionChoice]>,
    menu_of: CatalogMenu,
) -> Element<'a, Message> {
    popover(
        organize_chip(
            &OrganizeChipModel {
                icon: None,
                label: label.into(),
                count: None,
                tone: OrganizeTone::Action,
            },
            Some(act(CatalogAction::Menu(open.is_none().then_some(menu_of)))),
        ),
        open.map(menu),
        close(),
    )
}

/// One of the Organize band's lines: its label, then what it holds.
fn labelled<'a>(label: &str, value: Element<'a, Message>) -> Element<'a, Message> {
    row![
        container(
            text(label.to_owned())
                .size(theme::SIZE_CONTROL)
                .color(theme::TEXT_TERTIARY)
                .wrapping(Wrapping::None)
        )
        .width(Length::Fixed(ORGANIZE_LABEL_WIDTH))
        .padding(Padding::default().top(3.0)),
        container(value).width(Length::Fill),
    ]
    .align_y(Alignment::Start)
    .into()
}

/// What a name's field says while empty: what it names.
fn naming_placeholder(glyph: Icon) -> &'static str {
    match glyph {
        Icon::Folder => "Folder name",
        Icon::FolderGroup => "Group name",
        _ => "Collection name",
    }
}

/// The Organize band: the photographs' catalog folders with Move to…, and their collections with
/// Add to….
fn organize(model: &PhotoInfo) -> Element<'_, Message> {
    let mut folders: Vec<Element<'_, Message>> =
        model.folders.iter().map(organize_chip_view).collect();
    folders.push(organize_action(
        "Move to\u{2026}",
        model.move_menu.as_deref(),
        CatalogMenu::MovePhotos,
    ));
    let mut collections: Vec<Element<'_, Message>> =
        model.collections.iter().map(organize_chip_view).collect();
    collections.push(organize_action(
        "+ Add to\u{2026}",
        model.add_menu.as_deref(),
        CatalogMenu::AddTo,
    ));
    let mut band = column![
        text("Organize")
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .color(theme::TEXT_PRIMARY),
        labelled("Folder", organize_chips(folders)),
        labelled("Collections", organize_chips(collections)),
    ]
    .spacing(theme::SPACING);
    if let Some(note) = &model.organize_note {
        band = band.push(caption(note.clone()));
    }
    band.width(Length::Fill).into()
}

/// The Develop band: whether the photographs are edited, and Apply preset… and Export…, which wait
/// for batch jobs.
fn develop(model: &PhotoInfo) -> Element<'_, Message> {
    let waiting = |label: String, reason: &str, glyph: Option<Icon>| {
        with_tooltip(
            labelled_button(
                &LabelledButtonModel {
                    label,
                    icon: glyph,
                    key_hint: None,
                    tone: ButtonTone::Control,
                    size: ButtonSize::Regular,
                    fill: true,
                    enabled: false,
                },
                None,
            ),
            reason.to_owned(),
            tooltip::Position::Top,
        )
    };
    let mut content = Column::new().spacing(theme::SPACING);
    content = content.push(band("Develop", &model.develop));
    content
        .push(
            row![
                container(waiting(
                    "Apply preset\u{2026}".into(),
                    NOT_YET_PRESETS,
                    None
                ))
                .width(Length::Fill),
                container(waiting(
                    model.export.clone(),
                    NOT_YET_EXPORT,
                    Some(Icon::Export)
                ))
                .width(Length::Fill),
            ]
            .spacing(theme::SPACING),
        )
        .width(Length::Fill)
        .into()
}
