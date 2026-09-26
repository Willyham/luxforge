//! The state panel: what has happened to this photograph, and what the editor is doing now.
//! Versions and history scroll; the Performance section is pinned under them. Both render straight
//! from their models with the widget library; nothing here decides what a row means, which figure a
//! counter shows or which jobs are listed.
//!
//! The panel pads its sections [`theme::PANEL_PADDING_X`] from its sides and each row adds its own
//! grid unit inside that, so every label, chip and caption starts 16 pt from the panel's edge, as
//! the default board draws them.
use crate::{
    app::message::{HistoryMessage, MenuTarget, Message, PerformanceMessage, ViewMessage},
    state::{
        panel::{Marker as PanelMarker, StatePanelModel},
        performance::{PerformanceModel, WINDOW},
    },
};
use iced::{
    Alignment, Element, Length, Padding,
    widget::{Column, Row, Space, button, column, container, row, scrollable, text, text_input},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, ChipModel, Icon, IconButtonModel, JobRowModel, ListRowModel, Marker,
    MetricRowModel, SparklineModel, compact_chip, disclosure_heading, header_icon_button,
    inline_menu, job_row, list_row, metric_row, panel_heading, text_button, theme,
};

/// Where a row's text starts inside the panel's padding: the grid unit a list row pads itself by.
const ROW_INSET: f32 = theme::SPACING;

/// Where "Load older…" starts, under the history rows' labels: a row's inset, its sequence box, the
/// marker and the gaps between them.
const LABEL_INSET: f32 =
    ROW_INSET + theme::LIST_LEADING_WIDTH + theme::SPACING + theme::MARKER_SIZE + theme::SPACING;

/// Versions and History scroll above a 1 px rule, and the Performance section stays pinned under
/// it, so the section never scrolls out of view and never pushes History off the panel. The rule is
/// inset by the panel's padding.
pub(crate) fn state_panel<'a>(
    model: &'a StatePanelModel,
    performance_model: &'a PerformanceModel,
) -> Element<'a, Message> {
    let content = column![versions(model), history(model)]
        .spacing(theme::PANEL_SECTION_SPACING)
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
        scrollable(content).height(Length::Fill),
        rule,
        performance(performance_model),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The pinned section's padding: the panel's own at its sides and foot, plus a row's inset so its
/// heading starts where History's does, and a grid unit under the rule.
fn pinned_padding() -> Padding {
    Padding {
        top: theme::SPACING,
        right: theme::PANEL_PADDING_X + ROW_INSET,
        bottom: theme::PANEL_PADDING_Y,
        left: theme::PANEL_PADDING_X + ROW_INSET,
    }
}

/// The Performance section in the panel's own padding, its heading at the same left edge as
/// Versions and History. Collapsed it is the heading alone. Expanded: the three metric rows under
/// the heading, then, one grid unit further down, the job rows and the caption counting any long
/// jobs past the fourth.
fn performance(model: &PerformanceModel) -> Element<'_, Message> {
    let heading = disclosure_heading(
        "Performance",
        model.caption.clone(),
        model.expanded,
        Message::Performance(PerformanceMessage::Toggle),
    );
    if !model.expanded {
        return container(heading)
            .padding(pinned_padding())
            .width(Length::Fill)
            .into();
    }
    let metrics = Column::with_children(model.metrics.iter().map(|row| {
        metric_row(&MetricRowModel {
            label: row.label.to_owned(),
            value: row.value.clone(),
            unit: row.unit.to_owned(),
            series: SparklineModel {
                values: row.series.clone(),
                capacity: WINDOW,
                version: model.version,
            },
            tooltip: row.tooltip.clone(),
        })
    }))
    .spacing(theme::ROW_SPACING);
    let mut jobs = Column::with_children(model.jobs.iter().map(|job| {
        job_row(&JobRowModel {
            label: job.label.clone(),
            trailing: job.trailing.clone(),
            detail: job.detail.clone(),
            progress: job.progress,
            running: job.running,
        })
    }))
    .spacing(theme::SPACING)
    .width(Length::Fill);
    if model.reserve_detail {
        // The column's own spacing would add a grid unit above it; this is the detail line alone.
        jobs = jobs
            .push(Space::new().height(Length::Fixed(theme::CAPTION_LINE_HEIGHT - theme::SPACING)));
    }
    if let Some(more) = &model.more {
        jobs = jobs.push(
            container(
                text(more.clone())
                    .size(theme::SIZE_SMALL_CAPTION)
                    .line_height(text::LineHeight::Absolute(
                        theme::CAPTION_LINE_HEIGHT.into(),
                    ))
                    .color(theme::TEXT_TERTIARY),
            )
            .padding(Padding::default().left(theme::JOB_LABEL_INSET)),
        );
    }
    container(
        column![
            heading,
            metrics,
            // The rows above end with their own row spacing; this makes the gap one grid unit.
            container(jobs).padding(Padding::default().top(theme::SPACING - theme::ROW_SPACING)),
        ]
        .spacing(theme::ROW_SPACING),
    )
    .padding(pinned_padding())
    .width(Length::Fill)
    .into()
}

fn ui_marker(marker: PanelMarker) -> Marker {
    match marker {
        PanelMarker::Current => Marker::Current,
        PanelMarker::Previewed => Marker::Previewed,
        PanelMarker::Plain => Marker::Plain,
    }
}

/// A heading, a chip row or a caption inset by a row's own grid unit, so it lines up with the
/// history rows' text.
fn inset<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding([0.0, ROW_INSET])
        .width(Length::Fill)
        .into()
}

fn versions(model: &StatePanelModel) -> Element<'_, Message> {
    let add = header_icon_button(
        &IconButtonModel {
            icon: Icon::Plus,
            tooltip: "Save the displayed state as a version".into(),
            enabled: true,
            selected: model.version_form_open,
        },
        Some(Message::History(HistoryMessage::ToggleVersionForm)),
    );
    // The heading's `+` ends where the rows' captions do: the row's inset less the button's own
    // clearance around its icon.
    let mut block = column![
        container(panel_heading("Versions", Some(add)))
            .padding(Padding {
                left: ROW_INSET,
                right: ROW_INSET - (theme::HEADER_BUTTON_SIZE - theme::HEADER_ICON_SIZE) / 2.0,
                ..Padding::default()
            })
            .width(Length::Fill),
    ]
    .spacing(theme::VERSION_CHIP_SPACING);

    if !model.versions.is_empty() {
        let chips = model.versions.iter().map(|version| {
            compact_chip(
                &ChipModel {
                    label: version.name.clone(),
                    trailing: Some(version.entry_sequence.to_string()),
                    selected: version.selected,
                    enabled: !model.busy,
                },
                (!model.busy)
                    .then(|| Message::History(HistoryMessage::Select(version.entry_id.clone()))),
                Some(Message::View(ViewMessage::OpenMenu(MenuTarget::Version(
                    version.name.clone(),
                )))),
            )
        });
        block = block.push(inset(
            Row::with_children(chips.collect::<Vec<_>>())
                .spacing(theme::VERSION_CHIP_SPACING)
                .wrap()
                .vertical_spacing(theme::VERSION_CHIP_SPACING),
        ));
    }

    if model.version_form_open {
        block = block.push(inset(
            row![
                text_input("Name this version", &model.version_name)
                    .on_input(|value| Message::History(HistoryMessage::VersionName(value)))
                    .on_submit(Message::History(HistoryMessage::SaveVersion))
                    .style(theme::text_input_style(false))
                    .size(theme::SIZE_CONTROL)
                    .width(Length::Fill),
                save_button(model.can_save),
            ]
            .spacing(theme::SPACING / 2.0)
            .align_y(Alignment::Center),
        ));
    }

    if let Some(MenuTarget::Version(name)) = &model.menu {
        block = block.push(inset(inline_menu(vec![
            (
                "Delete".to_string(),
                Message::History(HistoryMessage::DeleteVersion(name.clone())),
            ),
            ("Cancel".to_string(), Message::View(ViewMessage::CloseMenu)),
        ])));
    }

    block.into()
}

fn save_button(can_save: bool) -> Element<'static, Message> {
    text_button(
        "Save",
        ButtonTone::Control,
        ButtonSize::Compact,
        can_save.then_some(Message::History(HistoryMessage::SaveVersion)),
    )
}

fn history(model: &StatePanelModel) -> Element<'_, Message> {
    let revision = model.revision.clone().map(|revision| {
        text(revision)
            .size(theme::SIZE_SMALL_CAPTION)
            .color(theme::TEXT_TERTIARY)
            .wrapping(text::Wrapping::None)
            .into()
    });
    // The heading clears the first row by the rows' own spacing twice, as the board spaces them.
    let mut block = column![
        container(inset(panel_heading("History", revision)))
            .padding(Padding::default().bottom(theme::LIST_ROW_SPACING))
    ]
    .spacing(theme::LIST_ROW_SPACING);
    let editable = !model.busy;
    for entry in &model.history {
        block = block.push(list_row(
            &ListRowModel {
                marker: ui_marker(entry.marker),
                leading: entry.sequence.to_string(),
                label: entry.label.clone(),
                trailing: entry.actor.clone(),
                dimmed: entry.branch,
                tag: entry.branch.then(|| "branch".to_string()),
                enabled: editable,
            },
            Some(Message::History(HistoryMessage::Select(
                entry.entry_id.clone(),
            ))),
            None,
        ));
    }
    if model.can_load_older {
        block = block.push(
            button(
                container(
                    text("Load older\u{2026}")
                        .size(theme::SIZE_CAPTION)
                        .wrapping(text::Wrapping::None),
                )
                .center_y(Length::Fill),
            )
            .padding(Padding::default().left(LABEL_INSET))
            .height(Length::Fixed(theme::LIST_ROW_HEIGHT))
            .style(theme::button_disclosure)
            .on_press_maybe(editable.then_some(Message::History(HistoryMessage::LoadOlder))),
        );
    }
    if let Some(preview) = model.preview {
        block = block.push(inset(
            row![
                text_button(
                    "Return to current",
                    ButtonTone::Control,
                    ButtonSize::Compact,
                    preview
                        .can_return
                        .then_some(Message::History(HistoryMessage::ReturnCurrent)),
                ),
                text_button(
                    "Restore",
                    ButtonTone::Primary,
                    ButtonSize::Compact,
                    preview
                        .can_restore
                        .then_some(Message::History(HistoryMessage::Restore)),
                ),
            ]
            .spacing(theme::BUTTON_ROW_SPACING),
        ));
    }
    block.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        panel::{HistoryRow, PreviewControls, VersionChip},
        performance::{JobRow, MetricRow},
    };

    /// The section builds collapsed, expanded with every row kind, and with more jobs than rows.
    #[test]
    fn the_performance_section_builds_in_every_state() {
        let panel = StatePanelModel::default();
        let _: Element<'_, Message> = state_panel(&panel, &PerformanceModel::default());
        let job = JobRow {
            label: "Developing RAW".into(),
            trailing: "1.2 s".into(),
            detail: Some("DSC_0412.NEF".into()),
            progress: Some(0.5),
            running: true,
        };
        let expanded = PerformanceModel {
            expanded: true,
            caption: Some("6 jobs".into()),
            metrics: vec![
                MetricRow {
                    label: "Memory",
                    value: "1.42".into(),
                    unit: "GB",
                    available: true,
                    series: vec![0.5, 0.6],
                    tooltip: "Memory footprint".into(),
                },
                MetricRow {
                    label: "GPU",
                    value: "\u{2013}".into(),
                    unit: "",
                    available: false,
                    series: Vec::new(),
                    tooltip: "GPU time is not reported on Linux yet".into(),
                },
            ],
            jobs: vec![job.clone(); 4],
            reserve_detail: false,
            more: Some("+2 more".into()),
            version: 7,
        };
        let _: Element<'_, Message> = state_panel(&panel, &expanded);
        let quiet = PerformanceModel {
            jobs: vec![JobRow {
                label: "No background work".into(),
                ..JobRow::default()
            }],
            reserve_detail: true,
            more: None,
            ..expanded
        };
        let _: Element<'_, Message> = state_panel(&panel, &quiet);
    }

    /// Versions, the naming form, a version's menu, rows by this client, another and the core, the
    /// older-page button and the preview controls all build together.
    #[test]
    fn the_history_and_versions_build_with_every_part_shown() {
        // The view never names the core; a fresh identity is its type's default.
        let row = |sequence: u64, actor: Option<&str>, marker: PanelMarker| HistoryRow {
            entry_id: Default::default(),
            sequence,
            label: "Clarity +18".into(),
            actor: actor.map(str::to_owned),
            marker,
            branch: false,
        };
        let panel = StatePanelModel {
            versions: vec![VersionChip {
                name: "Warm".into(),
                entry_sequence: 5,
                entry_id: Default::default(),
                selected: true,
            }],
            version_name: "Print".into(),
            version_form_open: true,
            can_save: true,
            revision: Some("rev 41".into()),
            history: vec![
                row(7, Some("you"), PanelMarker::Current),
                row(4, Some("agent \u{b7} lw-assist"), PanelMarker::Previewed),
                row(0, None, PanelMarker::Plain),
            ],
            can_load_older: true,
            preview: Some(PreviewControls {
                can_return: true,
                can_restore: false,
            }),
            menu: Some(MenuTarget::Version("Warm".into())),
            busy: false,
        };
        let _: Element<'_, Message> = state_panel(&panel, &PerformanceModel::default());
    }
}
