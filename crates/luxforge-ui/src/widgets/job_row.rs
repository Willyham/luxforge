//! One piece of long-running work in the state panel's Performance section: a marker, the label and
//! the elapsed time on a [`theme::JOB_LABEL_HEIGHT`] line, an optional detail line under it and an
//! optional progress bar under that.
//!
//! The row draws what it is given: the caller decides which jobs are shown, words the label and
//! the detail, formats the time and passes progress only when the work reports a truthful total.
//! A running job reads bright with a filled marker; a finished one is dimmed with a hollow marker,
//! the same circles as the history's, so neither needs a colour of its own.
//!
//! [`work_row`] is the section's row for long work the person can stop, as the catalog components
//! board draws it (`jobrow`): the accent dot, the label, the estimate once one is truthful and
//! Cancel, over a bar and the work's own count.

use super::list_row::marker_circle;
use super::long_work::{WorkProgress, count_text, work_bar};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, button, column, container, progress_bar, row, text};
use iced::{Alignment, Border, Element, Length, Padding};

/// Plain data for one job row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobRowModel {
    /// What the work is doing, for people: `Developing RAW`.
    pub label: String,
    /// The elapsed time while running, or the duration once finished, right-aligned.
    pub trailing: String,
    /// A second line (a file name and phase, or how the work ended); `None` draws no line.
    pub detail: Option<String>,
    /// How much is done, in `0.0..=1.0`, only for work that knows its total; `None` draws no bar.
    /// Clamped, and a non-finite value draws an empty bar.
    pub progress: Option<f32>,
    /// Running work reads bright with a filled marker; finished work is dimmed with a hollow one.
    pub running: bool,
}

/// The height a job row takes: its label line, plus its detail line and its progress bar when it
/// has them.
#[cfg(test)]
pub(crate) fn job_row_height(model: &JobRowModel) -> f32 {
    let detail = if model.detail.is_some() {
        theme::CAPTION_LINE_HEIGHT
    } else {
        0.0
    };
    let progress = if model.progress.is_some() {
        theme::JOB_PROGRESS_GAP + theme::RAIL_WIDTH
    } else {
        0.0
    };
    theme::JOB_LABEL_HEIGHT + detail + progress
}

/// The fraction of the progress bar that is filled: `progress` clamped into `0.0..=1.0`, and zero
/// for a non-finite value, so a caller's arithmetic can never overfill the rail.
pub(crate) fn progress_fraction(progress: f32) -> f32 {
    if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Renders one job row. The label and the detail each stay on one line and end in an ellipsis
/// when they do not fit; the elapsed time keeps its full width. The detail line and the progress
/// bar start exactly under the label, [`theme::JOB_LABEL_INSET`] in from the marker's edge.
pub fn job_row<'a, M: Clone + 'a>(model: &JobRowModel) -> Element<'a, M> {
    let (marker, label_color, trailing_color) = if model.running {
        (
            marker_circle(Some(theme::TEXT_PRIMARY), None),
            theme::TEXT_PRIMARY,
            theme::TEXT_SECONDARY,
        )
    } else {
        (
            marker_circle(None, Some(theme::TEXT_TERTIARY)),
            theme::TEXT_TERTIARY,
            theme::TEXT_FAINT,
        )
    };
    let line = LineHeight::Absolute(theme::JOB_LABEL_HEIGHT.into());

    // The label takes what the marker and the time leave; it hugs its text inside a filling box,
    // so the time stays at the right edge, one grid unit clear of a truncated label.
    let first = row![
        container(marker).width(Length::Fixed(theme::JOB_LABEL_INSET)),
        container(
            truncated_text(
                model.label.clone(),
                theme::SIZE_CONTROL,
                theme::FONT,
                label_color
            )
            .line_height(line),
        )
        .width(Length::Fill)
        .padding(Padding::default().right(theme::SPACING)),
        text(model.trailing.clone())
            .size(theme::SIZE_SMALL_CAPTION)
            .line_height(line)
            .wrapping(Wrapping::None)
            .color(trailing_color),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fixed(theme::JOB_LABEL_HEIGHT));

    let under_label = Padding::default().left(theme::JOB_LABEL_INSET);
    let mut rows = Column::new().push(first).width(Length::Fill);
    if let Some(detail) = &model.detail {
        rows = rows.push(
            container(
                truncated_text(
                    detail.clone(),
                    theme::SIZE_SMALL_CAPTION,
                    theme::FONT,
                    theme::TEXT_TERTIARY,
                )
                .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into())),
            )
            .padding(under_label)
            .width(Length::Fill)
            .height(Length::Fixed(theme::CAPTION_LINE_HEIGHT)),
        );
    }
    if let Some(progress) = model.progress {
        rows = rows.push(
            container(
                progress_bar(0.0..=1.0, progress_fraction(progress))
                    .length(Length::Fill)
                    .girth(Length::Fixed(theme::RAIL_WIDTH))
                    .style(|_theme| progress_bar::Style {
                        background: theme::RAIL.into(),
                        bar: theme::RAIL_FILL.into(),
                        border: Border::default(),
                    }),
            )
            .padding(under_label.top(theme::JOB_PROGRESS_GAP))
            .width(Length::Fill),
        );
    }
    rows.into()
}

/// Plain data for one piece of long work in the Performance section that can be stopped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkRowModel {
    /// What the work is doing, for people: `Indexing ~/Pictures`.
    pub label: String,
    /// How long is left, once the rate is steady (`about 1 min 40 s`); `None` draws nothing.
    pub estimate: Option<String>,
    pub progress: WorkProgress,
}

/// Renders one stoppable job: the accent dot, the label ending in an ellipsis when it does not
/// fit, the estimate and Cancel, which publishes `on_cancel` (`None` draws no Cancel); under it,
/// [`theme::WORK_ROW_INDENT`] in, a bar filling the width and the work's count, or `working`.
pub fn work_row<'a, M: Clone + 'a>(model: &WorkRowModel, on_cancel: Option<M>) -> Element<'a, M> {
    let mut first = row![
        marker_circle(Some(theme::ACCENT), None),
        truncated_text(
            model.label.clone(),
            theme::SIZE_CONTROL,
            theme::FONT,
            theme::TEXT_LABEL,
        )
        .line_height(LineHeight::Absolute(theme::JOB_LABEL_HEIGHT.into())),
    ]
    .spacing(theme::WORK_ROW_SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    if let Some(estimate) = &model.estimate {
        first = first.push(
            text(estimate.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .color(theme::TEXT_TERTIARY),
        );
    }
    if let Some(message) = on_cancel {
        first = first.push(
            button(
                text("Cancel")
                    .size(theme::SIZE_CAPTION)
                    .wrapping(Wrapping::None),
            )
            .padding(0)
            .style(theme::quiet_action)
            .on_press(message),
        );
    }
    let second = row![
        work_bar(model.progress.fraction, theme::WORK_BAR_HEIGHT),
        text(count_text(&model.progress).to_owned())
            .size(theme::SIZE_SMALL_CAPTION)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(theme::WORK_ROW_SPACING)
    .align_y(Alignment::Center)
    .padding(Padding::default().left(theme::WORK_ROW_INDENT));
    container(column![first, second].spacing(theme::WORK_ROW_LINE_SPACING))
        .padding(theme::WORK_ROW_PADDING)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(detail: Option<&str>, progress: Option<f32>, running: bool) -> JobRowModel {
        JobRowModel {
            label: "Developing RAW".into(),
            trailing: "1.2 s".into(),
            detail: detail.map(Into::into),
            progress,
            running,
        }
    }

    /// The design's heights: a 16 pt label line, a 14 pt detail line and the bar under it.
    #[test]
    fn a_job_row_is_its_label_line_detail_line_and_bar() {
        assert_eq!(job_row_height(&job(None, None, true)), 16.0);
        assert_eq!(job_row_height(&job(Some("DSC_0412.NEF"), None, true)), 30.0);
        assert_eq!(
            job_row_height(&job(Some("DSC_0412.NEF"), Some(0.5), true)),
            34.0
        );
        assert_eq!(job_row_height(&job(None, Some(0.5), false)), 20.0);
    }

    #[test]
    fn progress_is_clamped_and_non_finite_progress_is_empty() {
        assert_eq!(progress_fraction(0.375), 0.375);
        assert_eq!(progress_fraction(-0.2), 0.0);
        assert_eq!(progress_fraction(1.7), 1.0);
        assert_eq!(progress_fraction(f32::NAN), 0.0);
        assert_eq!(progress_fraction(f32::INFINITY), 0.0);
        assert_eq!(progress_fraction(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn every_state_builds() {
        for running in [true, false] {
            for detail in [None, Some("exact phase")] {
                for progress in [None, Some(0.4), Some(f32::NAN)] {
                    let _: Element<'_, ()> = job_row(&job(detail, progress, running));
                }
            }
        }
    }

    /// A stoppable job with a count and an estimate, one still finding its extent, and one that
    /// can say nothing yet, each with and without Cancel.
    #[test]
    fn every_work_row_builds() {
        for (count, fraction, estimate) in [
            (
                Some("48,210 of about 200,000 files"),
                Some(0.24),
                Some("about 1 min 40 s"),
            ),
            (Some("1,204 files so far"), None, None),
            (None, None, None),
        ] {
            let model = WorkRowModel {
                label: "Indexing Pictures".into(),
                estimate: estimate.map(Into::into),
                progress: WorkProgress {
                    count: count.map(Into::into),
                    fraction,
                },
            };
            let _: Element<'_, ()> = work_row(&model, Some(()));
            let _: Element<'_, ()> = work_row(&model, None);
        }
    }
}
