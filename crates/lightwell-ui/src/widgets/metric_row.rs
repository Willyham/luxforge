//! One resource in the state panel's Performance section: its label, its sparkline and its
//! current value, [`theme::METRIC_ROW_HEIGHT`] tall.
//!
//! The row draws what it is given. The caller formats the value and its unit, normalises the
//! series against its own scale and words the tooltip; an unavailable metric is simply the value
//! "–", no unit and an empty series, which draws the baseline alone.

use super::sparkline::{SparklineModel, sparkline};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::text::{Span, Wrapping};
use iced::widget::{container, rich_text, row, span, text, tooltip};
use iced::{Alignment, Element, Length};

/// Plain data for one metric row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetricRowModel {
    /// The resource's short name: Memory, CPU, GPU.
    pub label: String,
    /// The current figure, already formatted (`1.42`, `38`), or "–" while it is unavailable.
    pub value: String,
    /// The figure's unit (`GB`, `%`), drawn smaller and dimmer immediately after it; empty for none.
    pub unit: String,
    pub series: SparklineModel,
    /// What the row means, stated in full on hover; empty for no tooltip.
    pub tooltip: String,
}

/// Renders one metric row: the label in a [`theme::METRIC_LABEL_WIDTH`] box, the sparkline filling
/// the middle, and the value with its unit right-aligned in a [`theme::METRIC_VALUE_WIDTH`] box.
/// The whole row carries the tooltip.
pub fn metric_row<'a, M: Clone + 'a>(model: &MetricRowModel) -> Element<'a, M> {
    let label = container(truncated_text(
        model.label.clone(),
        theme::SIZE_CAPTION,
        theme::FONT,
        theme::TEXT_SECONDARY,
    ))
    .width(Length::Fixed(theme::METRIC_LABEL_WIDTH));

    // One paragraph rather than two text widgets, so the 12 pt figure and the 10.5 pt unit share a
    // baseline, and the unit follows the figure by one space of its own size.
    let mut spans: Vec<Span<'a, ()>> = vec![
        span(model.value.clone())
            .size(theme::SIZE_CONTROL)
            .color(theme::TEXT_PRIMARY),
    ];
    if !model.unit.is_empty() {
        spans.push(
            span(format!(" {}", model.unit))
                .size(theme::SIZE_SMALL_CAPTION)
                .color(theme::TEXT_TERTIARY),
        );
    }
    let value = container(
        rich_text(spans)
            .wrapping(Wrapping::None)
            .align_x(Horizontal::Right)
            .width(Length::Fill),
    )
    .width(Length::Fixed(theme::METRIC_VALUE_WIDTH));

    let content = row![label, sparkline(&model.series), value]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(theme::METRIC_ROW_HEIGHT));

    if model.tooltip.is_empty() {
        return content.into();
    }
    tooltip(
        content,
        container(
            text(model.tooltip.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(theme::TOOLTIP_PADDING)
        .style(theme::bar_surface),
        // The section sits at the bottom of the panel, so the tooltip opens upwards.
        tooltip::Position::Top,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_unavailable_and_untitled_rows_build() {
        let live = MetricRowModel {
            label: "Memory".into(),
            value: "1.42".into(),
            unit: "GB".into(),
            series: SparklineModel {
                values: vec![0.4, 0.5, 0.6],
                capacity: 60,
                version: 3,
            },
            tooltip: "Activity Monitor's Memory".into(),
        };
        let _: Element<'_, ()> = metric_row(&live);
        let _: Element<'_, ()> = metric_row(&MetricRowModel {
            value: "\u{2013}".into(),
            unit: String::new(),
            series: SparklineModel::default(),
            tooltip: "GPU time is not reported on Linux yet".into(),
            ..live.clone()
        });
        let _: Element<'_, ()> = metric_row(&MetricRowModel {
            tooltip: String::new(),
            ..live
        });
    }
}
