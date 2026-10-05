//! Gallery states for the Performance section's widgets: the disclosure heading, the metric rows
//! and the job rows on their own. Widget states only: no composed section, and no number taken
//! from the app's sampler, scale rules or layout. Each series is given as the fractions a metric
//! row draws.

use crate::Element;
use crate::{
    JobRowModel, MetricRowModel, SparklineModel, disclosure_heading, gallery::narrow, job_row,
    metric_row, theme,
};
use iced::widget::column;

/// The sparklines' capacity in these examples, so a short series starts part-way across.
const CAPACITY: usize = 60;

fn metric(
    label: &str,
    value: &str,
    unit: &str,
    values: Vec<f32>,
    tooltip: &str,
) -> Element<'static, ()> {
    metric_row(&MetricRowModel {
        label: label.into(),
        value: value.into(),
        unit: unit.into(),
        series: SparklineModel {
            values,
            capacity: CAPACITY,
            version: 2,
        },
        tooltip: tooltip.into(),
    })
}

fn job(
    label: &str,
    trailing: &str,
    detail: Option<&str>,
    progress: Option<f32>,
    running: bool,
) -> Element<'static, ()> {
    job_row(&JobRowModel {
        label: label.into(),
        trailing: trailing.into(),
        detail: detail.map(Into::into),
        progress,
        running,
    })
}

/// The Performance widget states, in gallery order.
pub(crate) fn gallery_performance() -> Vec<Element<'static, ()>> {
    vec![
        // The heading expanded with its caption, and collapsed with none: the chevron's ink ends
        // at the row's right edge either way.
        narrow(
            column![
                disclosure_heading("Performance", Some("2 jobs".into()), true, Some(())),
                disclosure_heading("Performance", None, false, Some(())),
            ]
            .spacing(theme::SPACING)
            .into(),
        ),
        // Metric rows as the window fills and when a counter is missing: one memory sample (the
        // dot alone, at the right edge), twelve CPU samples starting part-way across, and GPU
        // unavailable (a dash, no unit, the baseline alone).
        narrow(
            column![
                metric(
                    "Memory",
                    "812",
                    "MB",
                    vec![0.91],
                    "Activity Monitor's Memory \u{b7} peak 812 MB \u{b7} resident 640 MB",
                ),
                metric(
                    "CPU",
                    "3.2",
                    "%",
                    vec![
                        0.04, 0.03, 0.05, 0.61, 0.88, 0.42, 0.12, 0.06, 0.04, 0.03, 0.05, 0.032,
                    ],
                    "Percent of one core \u{b7} 14 cores: 1400% \u{b7} peak 88% this minute",
                ),
                metric(
                    "GPU",
                    "\u{2013}",
                    "",
                    Vec::new(),
                    "GPU time is not reported on Linux yet",
                ),
            ]
            .spacing(theme::ROW_SPACING)
            .into(),
        ),
        // Job rows: running with no detail, running with a file name too long for the row, running
        // with determinate progress, and finished, dimmed with a hollow marker.
        narrow(
            column![
                job("Measuring histogram", "0.6 s", None, None, true),
                job(
                    "Developing RAW",
                    "12 s",
                    Some("DSC_0412_Z6_lossless_14bit_panorama_frame_03.NEF"),
                    None,
                    true
                ),
                job(
                    "Exporting",
                    "1 min 4 s",
                    Some("DSC_0412.jpg \u{b7} 3 of 8"),
                    Some(0.375),
                    true
                ),
                job(
                    "Developing RAW",
                    "1.6 s",
                    Some("Finished 4 s ago"),
                    None,
                    false
                ),
            ]
            .spacing(theme::SPACING)
            .into(),
        ),
    ]
}
