//! Missing originals' region ([resolve board](../../../../docs/design/catalog/resolve-missing.png)):
//! the filter segments over the groups, each group's header with Find in a folder… and its rows
//! with what a search found, the floating bar with Stop search and Relink N, and the Info panel for
//! the selected row. Drawn from `state/select_missing.rs`'s model with the `luxforge-ui` resolve
//! pieces. **Lane D (views and desktop)** owns it.
//!
//! Like every view it reads only its model: each press sends the message its gesture is, and the
//! app sends the request.
use crate::{
    app::message::{Message, select::SelectMessage, select_missing::MissingMessage},
    layout::TOOLS_PANEL_WIDTH,
    state::select_missing::{
        ActionModel, BarModel, GroupModel, MissingFilter, MissingInfo, MissingModel, PhotoInfo,
        RELINK_NOTE, ResultGlyph, RowAction, RowModel,
    },
};
use iced::{
    Alignment, Element, Length, Padding, Theme,
    widget::{Column, Space, column, container, row, scrollable, stack, text, tooltip},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, FilterOption, FilterSegmentsModel, Icon, LabelledButtonModel,
    MenuEntry, MenuItem, ResolveBarModel, ResolveGroupModel, ResolveResultModel, ResolveRowModel,
    ResolveTone, caption, filter_bar, filter_segments, labelled_button, menu_list, popover,
    resolve_action, resolve_bar, resolve_group, resolve_row, theme, truncated_text, with_tooltip,
};

/// What the filter bar says at its right.
const NOTHING_CHANGES: &str = "Nothing changes until you relink";
/// The width of an Info panel row's label.
const INFO_LABEL_WIDTH: f32 = 96.0;
/// Room under the list for the floating bar, so its last row can be scrolled clear of it.
const BAR_CLEARANCE: f32 = 64.0;

fn missing(message: MissingMessage) -> Message {
    Message::Select(SelectMessage::Missing(message))
}

/// The centre: the filter bar, the heading and the groups on the canvas surface, a note in their
/// place when there is nothing to list, and the floating bar at the foot.
pub(crate) fn centre(model: &MissingModel) -> Element<'_, Message> {
    let segments = filter_segments(
        &FilterSegmentsModel {
            options: model
                .filters
                .iter()
                .map(|(label, count)| FilterOption {
                    label: label.clone(),
                    count: count.clone(),
                })
                .collect(),
            selected: model.filter,
            enabled: true,
        },
        |index| {
            missing(MissingMessage::Filter(
                MissingFilter::ALL[index.min(MissingFilter::ALL.len() - 1)],
            ))
        },
    );
    let bar = filter_bar(vec![segments], vec![caption(NOTHING_CHANGES)]);

    let mut list = Column::new().width(Length::Fill);
    if let Some((heading, note)) = &model.heading {
        list = list.push(
            container(
                row![
                    text(heading.clone())
                        .size(theme::SIZE_TITLE)
                        .font(theme::FONT_SEMIBOLD)
                        .color(theme::TEXT_BRIGHT),
                    text(note.clone())
                        .size(theme::SIZE_CAPTION)
                        .color(theme::TEXT_TERTIARY),
                ]
                .spacing(theme::RESOLVE_HEADER_SPACING)
                .align_y(Alignment::Center),
            )
            .padding(theme::RESOLVE_HEADING_PADDING),
        );
    }
    let groups = Column::with_children(model.groups.iter().map(group))
        .spacing(theme::RESOLVE_GROUP_SPACING)
        .width(Length::Fill);
    list = list
        .push(groups)
        .push(Space::new().height(Length::Fixed(BAR_CLEARANCE)));
    let mut layers = stack![
        container(
            scrollable(
                container(list)
                    .padding([0.0, theme::RESOLVE_LIST_PADDING])
                    .width(Length::Fill)
            )
            .direction(theme::panel_scrollbar())
            .height(Length::Fill)
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::canvas_surface)
    ];
    if let Some(note) = &model.note {
        layers = layers.push(container(caption(note.clone())).center(Length::Fill));
    }
    if let Some(model) = &model.bar {
        layers = layers.push(
            container(floating_bar(model))
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(Padding::default().bottom(theme::RESOLVE_BAR_BOTTOM)),
        );
    }
    column![bar, layers.width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn group(model: &GroupModel) -> Element<'_, Message> {
    let action = model.find.as_ref().map(|find| {
        let press = missing(MissingMessage::Find(model.folder.clone()));
        refusable(find, |enabled| {
            resolve_action(&find.label, enabled, enabled.then_some(press))
        })
    });
    let mut rows: Vec<Element<'_, Message>> = model.rows.iter().map(photo_row).collect();
    if let Some(more) = &model.more {
        rows.push(
            container(caption(more.clone()))
                .padding(theme::RESOLVE_ROW_PADDING)
                .height(Length::Fixed(theme::RESOLVE_ROW_HEIGHT))
                .align_y(iced::alignment::Vertical::Center)
                .into(),
        );
    }
    resolve_group(
        &ResolveGroupModel {
            path: model.path.clone(),
            detail: model.detail.clone(),
            status: model.status.clone(),
        },
        action,
        rows,
    )
}

/// An action that says on hover why it is refused.
fn refusable<'a>(
    model: &ActionModel,
    button: impl FnOnce(bool) -> Element<'a, Message>,
) -> Element<'a, Message> {
    match &model.reason {
        Some(reason) => with_tooltip(button(false), reason.clone(), tooltip::Position::Bottom),
        None => button(true),
    }
}

fn photo_row(model: &RowModel) -> Element<'_, Message> {
    let (icon, tone) = match model.glyph {
        ResultGlyph::Found => (Icon::Check, ResolveTone::Found),
        ResultGlyph::Refused => (Icon::Warning, ResolveTone::Refused),
        ResultGlyph::Several => (Icon::Stack, ResolveTone::Neutral),
        ResultGlyph::NotFound => (Icon::Search, ResolveTone::Neutral),
        ResultGlyph::Checking => (Icon::Clock, ResolveTone::Neutral),
    };
    let asset = &model.asset_id;
    let action = model.action.as_ref().map(|action| match action {
        RowAction::Locate { enabled } => resolve_action(
            "Locate\u{2026}",
            *enabled,
            enabled.then(|| missing(MissingMessage::Locate(asset.clone()))),
        ),
        RowAction::Choose { open, choices } => {
            let anchor = resolve_action(
                "Choose\u{2026}",
                true,
                Some(missing(MissingMessage::Menu(
                    (!open).then(|| asset.clone()),
                ))),
            );
            let menu = open.then(|| {
                menu_list(
                    choices
                        .iter()
                        .map(|(label, path, chosen)| {
                            MenuEntry::Item(MenuItem {
                                icon: chosen.then_some(Icon::Check),
                                label: label.clone(),
                                trailing: None,
                                on_press: Some(missing(MissingMessage::Choose {
                                    asset: asset.clone(),
                                    path: path.clone(),
                                })),
                                reason: None,
                            })
                        })
                        .collect(),
                )
            });
            popover(anchor, menu, missing(MissingMessage::Menu(None)))
        }
    });
    resolve_row(
        &ResolveRowModel {
            name: model.name.clone(),
            folder: model.folder.clone(),
            result: ResolveResultModel {
                icon,
                tone,
                text: model.text.clone(),
                detail: model.detail.clone(),
            },
            selected: model.selected,
        },
        Some(missing(MissingMessage::Row(asset.clone()))),
        action,
    )
}

fn floating_bar(model: &BarModel) -> Element<'_, Message> {
    let bar = resolve_bar(
        &ResolveBarModel {
            verified: model.verified.clone(),
            detail: model.detail.clone(),
            stop: model.stop,
            relink: model.relink.clone(),
            relink_enabled: model.pairs > 0,
        },
        model.stop.then_some(missing(MissingMessage::Stop)),
        (model.pairs > 0).then_some(missing(MissingMessage::Relink)),
    );
    match &model.reason {
        Some(reason) => with_tooltip(bar, reason.clone(), tooltip::Position::Top),
        None => bar,
    }
}

// -- Info panel ------------------------------------------------------------------------------------

/// The selected row's preview place, its Original band and Locate a different file…, or a caption.
pub(crate) fn info(model: &MissingModel) -> Element<'_, Message> {
    let content: Element<'_, Message> = match &model.info {
        MissingInfo::Nothing => {
            caption("Select a photograph to see where its original was and what was found")
        }
        MissingInfo::One(info) => photo(info),
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

fn photo(info: &PhotoInfo) -> Element<'_, Message> {
    let width = TOOLS_PANEL_WIDTH - 2.0 * theme::PANEL_PADDING_X;
    let preview = container(
        container(Space::new())
            .width(Length::Fixed(width))
            .height(Length::Fixed(width / 1.5))
            .style(|_: &Theme| {
                container::Style::default()
                    .background(theme::CELL_PLACEHOLDER)
                    .border(iced::Border {
                        radius: theme::THUMBNAIL_RADIUS.into(),
                        ..iced::Border::default()
                    })
            }),
    )
    .center_x(Length::Fill);
    let heading = text("Original")
        .size(theme::SIZE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .color(theme::TEXT_PRIMARY);
    let rows = info.rows.iter().enumerate().map(|(index, (label, value))| {
        let value = truncated_text(
            value.clone(),
            theme::SIZE_CONTROL,
            theme::FONT,
            theme::TEXT_LABEL,
        );
        // Check's dot is the verified green when the file's bytes are the original's.
        let value: Element<'_, Message> = if index == 2 {
            let ink = if info.verified {
                theme::RESOLVE_FOUND
            } else {
                theme::TEXT_TERTIARY
            };
            row![
                container(Space::new())
                    .width(Length::Fixed(theme::STATUS_DOT_SIZE))
                    .height(Length::Fixed(theme::STATUS_DOT_SIZE))
                    .style(move |_: &Theme| {
                        container::Style::default()
                            .background(ink)
                            .border(iced::Border {
                                radius: (theme::STATUS_DOT_SIZE / 2.0).into(),
                                ..iced::Border::default()
                            })
                    }),
                value,
            ]
            .spacing(theme::RESOLVE_DETAIL_SPACING)
            .align_y(Alignment::Center)
            .into()
        } else {
            value.into()
        };
        row![
            text(label.clone())
                .size(theme::SIZE_CONTROL)
                .color(theme::TEXT_TERTIARY)
                .wrapping(text::Wrapping::None)
                .width(Length::Fixed(INFO_LABEL_WIDTH)),
            value,
        ]
        .align_y(Alignment::Center)
        .into()
    });
    let band = Column::with_children(std::iter::once(heading.into()).chain(rows))
        .spacing(theme::SPACING)
        .width(Length::Fill);
    let press = missing(MissingMessage::Locate(info.asset_id.clone()));
    let locate = refusable(&info.locate, |enabled| {
        labelled_button(
            &LabelledButtonModel {
                label: info.locate.label.clone(),
                icon: None,
                key_hint: None,
                tone: ButtonTone::Control,
                size: ButtonSize::Regular,
                fill: true,
                enabled,
            },
            enabled.then_some(press),
        )
    });
    column![preview, band, locate, caption(RELINK_NOTE)]
        .spacing(theme::PANEL_SECTION_SPACING)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::select_missing::{derive, tests::showing};

    /// The region and the Info panel build before the list is read.
    #[test]
    fn the_missing_originals_region_builds_before_the_list_is_read() {
        let model = derive(&showing());
        assert!(model.shown);
        let _ = centre(&model);
        let _ = info(&model);
    }
}
