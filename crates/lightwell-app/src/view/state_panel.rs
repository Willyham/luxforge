//! The state panel: what has happened to this photograph, and what the editor is doing now.
//! Versions, history and the recipe scroll; the Performance section is pinned under them. Both
//! render straight from their models with the widget library; nothing here decides what a row
//! means, which figure a counter shows or which jobs are listed.
use crate::{
    app::message::{MenuTarget, Message},
    state::{
        panel::{Marker as PanelMarker, StatePanelModel},
        performance::{PerformanceModel, WINDOW},
    },
};
use iced::{
    Alignment, Element, Length, Padding,
    widget::{
        Column, Space, column, container, row, scrollable, text, text::LineHeight, text::Wrapping,
        text_input,
    },
};
use lightwell_ui::{
    ButtonSize, ButtonTone, ChipModel, Icon, IconButtonModel, JobRowModel, ListRowModel, Marker,
    MetricRowModel, SparklineModel, chip, chip_wrap, disclosure_heading, icon_button, inline_menu,
    job_row, list_heading, list_row, metric_row, section_label, text_button, theme, truncated_text,
};

/// The history and the recipe scroll above a 1 px rule, and the Performance section stays pinned
/// under it, so the section never scrolls out of view and never pushes History off the panel. The
/// rule is inset by the panel's padding, as the rules between the mockup's sections are.
pub(crate) fn state_panel<'a>(
    model: &'a StatePanelModel,
    performance_model: &'a PerformanceModel,
) -> Element<'a, Message> {
    let content = column![versions(model), history(model), recipe(model)]
        .spacing(theme::SPACING * 2.0)
        .padding(theme::SPACING)
        .width(Length::Fill);
    let rule = container(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::band_border_surface),
    )
    .padding([0.0, theme::SPACING]);
    column![
        scrollable(content).height(Length::Fill),
        rule,
        performance(performance_model),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The Performance section in the panel's own padding, its heading at the same left edge as
/// History and Recipe. Collapsed it is the heading alone. Expanded: the three metric rows under
/// the heading, then, one grid unit further down, the job rows and the caption counting any long
/// jobs past the fourth.
fn performance(model: &PerformanceModel) -> Element<'_, Message> {
    let heading = disclosure_heading(
        "Performance",
        model.caption.clone(),
        model.expanded,
        Message::TogglePerformance,
    );
    if !model.expanded {
        return container(heading)
            .padding(theme::SPACING)
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
                    .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
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
    .padding(theme::SPACING)
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

fn versions(model: &StatePanelModel) -> Element<'_, Message> {
    let chips = model.versions.iter().map(|version| {
        chip(
            &ChipModel {
                label: version.name.clone(),
                trailing: Some(version.entry_sequence.to_string()),
                selected: version.selected,
                enabled: !model.busy,
            },
            (!model.busy).then(|| Message::Preview(version.entry_id.clone())),
            Some(Message::OpenMenu(MenuTarget::Version(version.name.clone()))),
        )
    });
    let chip_row = chip_wrap(chips.collect());

    let mut block = column![
        row![
            section_label("Versions"),
            Space::new().width(Length::Fill),
            icon_button(
                &IconButtonModel {
                    icon: Icon::Plus,
                    tooltip: "Save the displayed state as a version".into(),
                    enabled: true,
                    selected: model.version_form_open,
                },
                Some(Message::ToggleVersionForm),
            ),
        ]
        .align_y(Alignment::Center),
        chip_row,
    ]
    .spacing(theme::SPACING / 2.0);

    if model.version_form_open {
        block = block.push(
            row![
                text_input("Name this version", &model.version_name)
                    .on_input(Message::VersionName)
                    .on_submit(Message::SaveVersion)
                    .style(theme::text_input_style(false))
                    .size(theme::SIZE_CONTROL)
                    .width(Length::Fill),
                save_button(model.can_save),
            ]
            .spacing(theme::SPACING / 2.0)
            .align_y(Alignment::Center),
        );
    }

    if let Some(MenuTarget::Version(name)) = &model.menu {
        block = block.push(inline_menu(vec![
            ("Delete".to_string(), Message::DeleteVersion(name.clone())),
            ("Cancel".to_string(), Message::CloseMenu),
        ]));
    }

    block.into()
}

fn save_button(can_save: bool) -> Element<'static, Message> {
    text_button(
        "Save",
        ButtonTone::Control,
        ButtonSize::Compact,
        can_save.then_some(Message::SaveVersion),
    )
}

fn history(model: &StatePanelModel) -> Element<'_, Message> {
    let mut block = column![list_heading("History")].spacing(theme::LIST_ROW_SPACING);
    let editable = !model.busy;
    for entry in &model.history {
        block = block.push(list_row(
            &ListRowModel {
                marker: ui_marker(entry.marker),
                leading: entry.sequence.to_string(),
                label: entry.label.clone(),
                trailing: Some(entry.actor.clone()),
                dimmed: entry.branch,
                tag: entry.branch.then(|| "branch".to_string()),
                enabled: editable,
            },
            Some(Message::Preview(entry.entry_id.clone())),
            None,
        ));
    }
    if model.can_load_older {
        block = block.push(text_button(
            "Load older",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            editable.then_some(Message::LoadOlder),
        ));
    }
    if let Some(preview) = model.preview {
        block = block.push(
            row![
                text_button(
                    "Return to current",
                    ButtonTone::Control,
                    ButtonSize::Compact,
                    preview.can_return.then_some(Message::ReturnCurrent),
                ),
                text_button(
                    "Restore",
                    ButtonTone::Primary,
                    ButtonSize::Compact,
                    preview.can_restore.then_some(Message::Restore),
                ),
            ]
            .spacing(theme::BUTTON_ROW_SPACING),
        );
    }
    block.into()
}

fn recipe(model: &StatePanelModel) -> Element<'_, Message> {
    let mut block = column![list_heading("Recipe")].spacing(theme::LIST_ROW_SPACING);
    if let Some(message) = &model.recipe_caption {
        block = block.push(lightwell_ui::caption(message.clone()));
        return block.into();
    }
    for (index, layer) in model.recipe.iter().enumerate() {
        // A mask's own heading sits above the first of its layers, in the durable processing order
        // rather than in place of it: the list's whole job is to show the order edits are applied
        // in, so a masked layer is labelled where it is rather than moved under its mask.
        //
        // The heading is the mask's name and nothing else. A default-named mask is called `Mask 1`
        // because it is the first mask, so spelling its position beside its name read `Mask 1 ·
        // mask 1`: the same fact twice, and a second ordering in a list whose whole subject is the
        // processing order. The position a mask composes in belongs to the Masks panel, whose list
        // *is* that order.
        if let Some(mask) = &layer.mask
            && mask.heading
        {
            block = block.push(section_label(mask.heading_label()));
        }
        block = block.push(recipe_row(
            index,
            layer.title.clone(),
            layer.summary.clone(),
            layer.available,
            layer.mask.as_ref().map(|mask| mask.name.clone()),
        ));
    }
    block.into()
}

/// One recipe row: the sequence and the module title on their own line, the layer's payload
/// summary as a caption underneath. Two short lines read better here than a trailing caption,
/// which a long summary ("unavailable: disabled by --disable-module") would otherwise squeeze
/// into a sliver next to a title that keeps the rest of the row. Both lines stay on one line each:
/// the title clips (`Wrapping::None`) and the unbounded summary ends in an ellipsis.
fn recipe_row(
    index: usize,
    title: String,
    summary: String,
    available: bool,
    mask: Option<String>,
) -> Element<'static, Message> {
    let title_color = if available {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_TERTIARY
    };
    let mut heading = row![
        container(lightwell_ui::caption((index + 1).to_string())).width(Length::Fixed(24.0)),
        container(
            text(title)
                .size(theme::SIZE_CONTROL)
                .color(title_color)
                .wrapping(Wrapping::None),
        )
        .clip(true)
        .width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    // The mask this layer applies through, on the row itself, so a masked layer is never mistaken
    // for a global one wherever the processing order puts it.
    if let Some(mask) = mask {
        heading = heading.push(lightwell_ui::caption(mask));
    }
    let detail = container(truncated_text(
        summary,
        theme::SIZE_CAPTION,
        theme::FONT,
        theme::TEXT_TERTIARY,
    ))
    .width(Length::Fill)
    .padding(Padding {
        left: 32.0,
        ..Padding::default()
    });
    column![heading, detail].spacing(2.0).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::performance::{JobRow, MetricRow};

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

    /// A summary long enough to have caused the old trailing-caption layout to wrap into a tall
    /// sliver still builds as one row, its own two lines, with no panic.
    #[test]
    fn a_long_recipe_summary_stays_a_two_line_row() {
        let _: Element<'static, Message> = recipe_row(
            0,
            "Basic".into(),
            "unavailable: disabled by --disable-module".into(),
            false,
            None,
        );
        // A masked row carries its mask's name beside the title and still builds as one row.
        let _: Element<'static, Message> = recipe_row(
            1,
            "Basic".into(),
            "exposure +0.45".into(),
            true,
            Some("Mask 1".into()),
        );
    }
}
