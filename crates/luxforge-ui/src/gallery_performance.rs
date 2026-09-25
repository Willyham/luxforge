//! Gallery states for the state panel's Performance section at the panel's width: the section as
//! it reads while long work runs, then the heading, the metric rows and the job rows on their own.
//!
//! The series are shaped like a real minute rather than noise, and normalised by the design's
//! scale rules (memory against 110% of the window's peak, CPU against the larger of 100% and its
//! peak, GPU against 100%), so the board shows the lines the panel will draw.

use crate::{
    JobRowModel, ListRowModel, Marker, MetricRowModel, SparklineModel, disclosure_heading, job_row,
    list_heading, list_row, metric_row, theme,
};
use iced::widget::{Column, Space, column, container};
use iced::{Element, Length};

/// The state panel's width in the reference screens and the performance mockup.
const STATE_PANEL_WIDTH: f32 = 240.0;

/// The sampler's window: one sample a second for a minute.
const WINDOW: usize = 60;

/// The panel around a state: [`STATE_PANEL_WIDTH`] with the panel's own padding, so the rows get
/// exactly the width the state panel gives them.
fn state_panel(content: Element<'static, ()>) -> Element<'static, ()> {
    container(content)
        .width(Length::Fixed(STATE_PANEL_WIDTH))
        .padding(theme::SPACING)
        .style(theme::panel_surface)
        .into()
}

/// A smooth bump of `height` centred on sample `centre`, `width` samples to either side at half
/// its height or so.
fn bump(index: usize, centre: f32, width: f32, height: f32) -> f32 {
    let offset = (index as f32 - centre) / width;
    height * (-offset * offset).exp()
}

/// A minute of memory footprint in GB: a plateau, a RAW development's buffers as a bump that is
/// mostly released again, and 1.42 GB now.
fn memory_minute() -> Vec<f32> {
    (0..WINDOW)
        .map(|index| {
            let kept = 0.12 / (1.0 + (-(index as f32 - 46.0) / 1.5).exp());
            1.30 + bump(index, 44.0, 3.0, 0.26) + kept
        })
        .collect()
}

/// A minute of CPU in percent of one core: idle, a preview render past one core, a RAW
/// development across several, and 38% now.
fn cpu_minute() -> Vec<f32> {
    (0..WINDOW)
        .map(|index| {
            let idle = 3.0 + 2.0 * ((index as f32) * 1.7).sin().abs();
            idle + bump(index, 22.0, 2.0, 150.0)
                + bump(index, 45.0, 2.8, 410.0)
                + bump(index, 59.0, 1.2, 34.6)
        })
        .collect()
}

/// A minute of GPU time in percent: uploads and presentation following the same two renders.
fn gpu_minute() -> Vec<f32> {
    (0..WINDOW)
        .map(|index| {
            1.0 + bump(index, 22.5, 2.0, 21.0)
                + bump(index, 45.5, 2.6, 34.0)
                + bump(index, 59.0, 1.0, 11.0)
        })
        .collect()
}

/// `samples` divided by `scale`, the one number the caller's scale rule settles on.
fn normalised(samples: &[f32], scale: f32) -> Vec<f32> {
    samples.iter().map(|sample| sample / scale).collect()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().copied().fold(0.0, f32::max)
}

fn series(values: Vec<f32>, version: u64) -> SparklineModel {
    SparklineModel {
        values,
        capacity: WINDOW,
        version,
    }
}

fn metric(
    label: &str,
    value: &str,
    unit: &str,
    series: SparklineModel,
    tooltip: &str,
) -> Element<'static, ()> {
    metric_row(&MetricRowModel {
        label: label.into(),
        value: value.into(),
        unit: unit.into(),
        series,
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

/// The three metric rows as the running mockup draws them, each figure and peak read from its
/// own series so the board agrees with itself.
fn live_metrics() -> Element<'static, ()> {
    let memory = memory_minute();
    let cpu = cpu_minute();
    let gpu = gpu_minute();
    let now = |samples: &[f32]| samples.last().copied().unwrap_or_default();
    column![
        metric(
            "Memory",
            &format!("{:.2}", now(&memory)),
            "GB",
            series(normalised(&memory, (1.1 * peak(&memory)).max(0.0625)), 1),
            &format!(
                "Activity Monitor's Memory \u{b7} peak {:.2} GB \u{b7} resident 980 MB \u{b7} \
                 includes 312 MB of GPU allocations",
                peak(&memory)
            ),
        ),
        metric(
            "CPU",
            &format!("{:.0}", now(&cpu)),
            "%",
            series(normalised(&cpu, peak(&cpu).max(100.0)), 1),
            &format!(
                "Percent of one core \u{b7} 14 cores: 1400% \u{b7} peak {:.0}% this minute",
                peak(&cpu)
            ),
        ),
        metric(
            "GPU",
            &format!("{:.0}", now(&gpu)),
            "%",
            series(normalised(&gpu, 100.0), 1),
            &format!(
                "Percent of GPU time \u{b7} peak {:.0}% this minute \u{b7} 312 MB allocated",
                peak(&gpu)
            ),
        ),
    ]
    .spacing(theme::ROW_SPACING)
    .into()
}

/// The Performance states, in gallery order.
pub(crate) fn gallery_performance() -> Vec<Element<'static, ()>> {
    let mut states = Vec::new();

    // -- The section while two long jobs run, under the end of the history as the running column
    // -- of the mockup draws it: the heading left-aligned with the list heading above the rule, the
    // -- three metric rows, and the two jobs.
    let running: Element<'static, ()> = column![
        column![
            list_heading("History"),
            list_row(
                &ListRowModel {
                    marker: Marker::Current,
                    leading: "7".into(),
                    label: "Clarity +18".into(),
                    trailing: Some("you".into()),
                    dimmed: false,
                    tag: None,
                    enabled: true,
                },
                Some(()),
                None,
            ),
        ]
        .spacing(theme::LIST_ROW_SPACING),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::band_border_surface),
        column![
            disclosure_heading("Performance", Some("2 jobs".into()), true, ()),
            live_metrics(),
            Column::new()
                .push(job(
                    "Developing RAW",
                    "1.2 s",
                    Some("DSC_0412.NEF"),
                    None,
                    true
                ))
                .push(job(
                    "Rendering preview",
                    "0.7 s",
                    Some("exact phase"),
                    None,
                    true
                ))
                .spacing(theme::SPACING)
                .padding(iced::Padding::default().top(theme::SPACING - theme::ROW_SPACING)),
        ]
        .spacing(theme::ROW_SPACING),
    ]
    .spacing(theme::SPACING)
    .into();
    states.push(state_panel(running));

    // -- The heading expanded with its caption, and collapsed with none: the chevron's ink ends at
    // -- the row's right edge either way.
    states.push(state_panel(
        column![
            disclosure_heading("Performance", Some("2 jobs".into()), true, ()),
            disclosure_heading("Performance", None, false, ()),
        ]
        .spacing(theme::SPACING)
        .into(),
    ));

    // -- Metric rows as the window fills and when a counter is missing: one memory sample (the dot
    // -- alone, at the right edge), twelve CPU samples starting part-way across, and GPU
    // -- unavailable (a dash, no unit, the baseline alone).
    let cpu_start = [
        4.0, 3.0, 5.0, 61.0, 88.0, 42.0, 12.0, 6.0, 4.0, 3.0, 5.0, 3.2,
    ];
    states.push(state_panel(
        column![
            metric(
                "Memory",
                "812",
                "MB",
                series(vec![1.0 / 1.1], 2),
                "Activity Monitor's Memory \u{b7} peak 812 MB \u{b7} resident 640 MB",
            ),
            metric(
                "CPU",
                "3.2",
                "%",
                series(normalised(&cpu_start, 100.0), 2),
                "Percent of one core \u{b7} 14 cores: 1400% \u{b7} peak 88% this minute",
            ),
            metric(
                "GPU",
                "\u{2013}",
                "",
                series(Vec::new(), 2),
                "GPU time is not reported on Linux yet",
            ),
        ]
        .spacing(theme::ROW_SPACING)
        .into(),
    ));

    // -- Job rows: running with no detail, running with a file name too long for the panel,
    // -- running with determinate progress, and finished, dimmed with a hollow marker.
    states.push(state_panel(
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
    ));

    states
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The series follow the scale rules they claim: every normalised value is in range and each
    /// line reaches its scale's top only where the rule says it should.
    #[test]
    fn the_gallery_series_are_normalised_by_the_design_rules() {
        let memory = memory_minute();
        let memory_line = normalised(&memory, 1.1 * peak(&memory));
        assert!((peak(&memory_line) - 1.0 / 1.1).abs() < 1e-5);
        let cpu = cpu_minute();
        assert!(peak(&cpu) > 100.0, "the CPU minute spikes past one core");
        assert!((peak(&normalised(&cpu, peak(&cpu).max(100.0))) - 1.0).abs() < 1e-5);
        let gpu = gpu_minute();
        assert!(peak(&gpu) < 100.0);
        for line in [memory_line, normalised(&gpu, 100.0)] {
            assert_eq!(line.len(), WINDOW);
            assert!(line.iter().all(|value| (0.0..=1.0).contains(value)));
        }
        assert_eq!(gallery_performance().len(), 4);
    }
}
