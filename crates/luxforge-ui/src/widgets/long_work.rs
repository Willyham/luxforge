//! Long-running work, as the catalog components board draws it: the bar every piece shares, the
//! status bar's busiest job, and the in-view progress sheet. The Performance section's row for work
//! that can be stopped is [`crate::work_row`], beside the section's other job rows.
//!
//! These widgets are honest by construction. A bar is filled exactly as far as the fraction it is
//! given, clamped to the whole, and not at all when the caller gives none because the work does
//! not know its total; a count the work cannot state reads "working". Nothing here guesses, eases
//! or animates, so nothing here sets a timer.

use super::button_row::{ButtonSize, ButtonTone, text_button};
use super::icon_button::{Icon, icon};
use super::list_row::marker_circle;
use crate::theme;
use crate::{Derived, Element, Theme, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Space, button, column, container, progress_bar, row, text};
use iced::{Alignment, Border, Length, Padding};

/// How far a piece of long-running work has got, as the work itself reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkProgress {
    /// What is done out of what, in words: `312 of 612 files`, or `48,210 of about 200,000 files`
    /// while a walk is still finding its extent. `None` while the work cannot say, drawn as
    /// `working`.
    pub count: Option<String>,
    /// The fraction done, only when the work knows its total truthfully. `None` draws the bar with
    /// no fill, never a guess.
    pub fraction: Option<f32>,
}

/// What a count reads: the work's own words, or `working` when it has none.
pub(crate) fn count_text(progress: &WorkProgress) -> &str {
    progress.count.as_deref().unwrap_or("working")
}

/// How much of a bar is filled: the fraction clamped into `0.0..=1.0`, or `None` — no fill — for no
/// fraction or a non-finite one.
pub(crate) fn bar_fill(fraction: Option<f32>) -> Option<f32> {
    fraction
        .filter(|fraction| fraction.is_finite())
        .map(|fraction| fraction.clamp(0.0, 1.0))
}

/// A whole percentage that never rounds up to done: 99.6% reads 99%, and only the whole reads
/// 100%.
pub(crate) fn percent(fraction: f32) -> u32 {
    // Truncation is the point: a job is never shown further on than it is.
    (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32
}

/// The bar long-running work shares: `height` tall on the rail, rounded to a pill, filled with the
/// accent exactly as far as [`bar_fill`] says, filling the width it is given.
pub(crate) fn work_bar<'a, M: 'a>(fraction: Option<f32>, height: f32) -> Element<'a, M> {
    progress_bar(0.0..=1.0, bar_fill(fraction).unwrap_or(0.0))
        .length(Length::Fill)
        .girth(Length::Fixed(height))
        .style(move |theme: &Theme| progress_bar::Style {
            background: theme.palette().rail.into(),
            bar: theme.palette().accent.into(),
            border: Border {
                radius: (height / 2.0).into(),
                width: 0.0,
                color: iced::Color::TRANSPARENT,
            },
        })
        .into()
}

/// Plain data for the status bar's busiest job.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusJobModel {
    /// The busiest job, for people: `Indexing ~/Pictures`.
    pub label: String,
    /// Its fraction done, only when it knows its total; `None` shows `working` and no fill.
    pub fraction: Option<f32>,
    /// How many jobs are running, this one included.
    pub jobs: usize,
}

/// The status bar job's caption: the percentage done or `working`, then how many jobs run when
/// there is more than one (`24% · 2 jobs`).
pub(crate) fn status_caption(fraction: Option<f32>, jobs: usize) -> String {
    let done = match bar_fill(fraction) {
        Some(fraction) => format!("{}%", percent(fraction)),
        None => "working".to_owned(),
    };
    if jobs > 1 {
        format!("{done} \u{b7} {jobs} jobs")
    } else {
        done
    }
}

/// Renders the status bar's busiest job: the accent dot, its label, a
/// [`theme::STATUS_JOB_BAR_WIDTH`] bar and its caption, all one button that publishes `on_press`
/// (the caller opens the Performance section).
pub fn status_job<'a, M: Clone + 'a>(model: &StatusJobModel, on_press: M) -> Element<'a, M> {
    let content = row![
        marker_circle(Some(Token::Accent), None),
        text(model.label.clone())
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextSecondary)),
        container(work_bar(model.fraction, theme::WORK_BAR_HEIGHT))
            .width(Length::Fixed(theme::STATUS_JOB_BAR_WIDTH)),
        text(status_caption(model.fraction, model.jobs))
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextTertiary)),
    ]
    .spacing(theme::STATUS_JOB_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    button(content)
        .padding([0.0, theme::SPACING])
        .height(Length::Fixed(theme::STATUS_JOB_HEIGHT))
        .style(theme::status_job)
        .on_press(on_press)
        .into()
}

/// Plain data for the in-view progress sheet: what the view is waiting for.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressSheetModel {
    /// The glyph before the title: the drive for a card, the folder for a folder.
    pub icon: Icon,
    /// `Reading the NIKON Z 8 card`.
    pub title: String,
    /// One or two sentences on what is happening and what comes next.
    pub note: String,
    pub progress: WorkProgress,
    /// `about 4 s left`, once the rate is steady; `None` draws nothing in its place.
    pub estimate: Option<String>,
}

/// Renders the progress sheet a view shows while it has nothing to draw yet,
/// [`theme::SHEET_WIDTH`] wide: the title, the note, the bar, the count and the estimate, and a
/// footer with Continue in background and Cancel, which publish `on_background` and `on_cancel`.
/// The caller places it over the waiting view only.
pub fn progress_sheet<'a, M: Clone + 'a>(
    model: &ProgressSheetModel,
    on_background: M,
    on_cancel: M,
) -> Element<'a, M> {
    let caption = |content: String| {
        text(content)
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextIdentity))
    };
    let mut figures = row![
        caption(count_text(&model.progress).to_owned()),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center);
    if let Some(estimate) = &model.estimate {
        figures = figures.push(caption(estimate.clone()));
    }
    let body = column![
        row![
            icon(model.icon, theme::SHEET_ICON_SIZE, Token::TextBright),
            text(model.title.clone())
                .size(theme::SIZE_TITLE)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::TextBright)),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center),
        text(model.note.clone())
            .size(theme::SIZE_CAPTION)
            .line_height(LineHeight::Relative(1.4))
            .style(theme::ink(Token::TextSecondary)),
        work_bar(model.progress.fraction, theme::SHEET_BAR_HEIGHT),
        figures,
    ]
    .spacing(theme::SHEET_SPACING)
    .width(Length::Fill);
    let footer = container(
        row![
            Space::new().width(Length::Fill),
            text_button(
                "Continue in background",
                ButtonTone::Control,
                ButtonSize::Regular,
                Some(on_background),
            ),
            text_button(
                "Cancel",
                ButtonTone::Control,
                ButtonSize::Regular,
                Some(on_cancel)
            ),
        ]
        .spacing(theme::BUTTON_ROW_SPACING)
        .align_y(Alignment::Center),
    )
    .padding(theme::SHEET_FOOTER_PADDING)
    .width(Length::Fill)
    .style(|theme: &Theme| {
        // The sheet's lower corners, less its outline, so the footer's fill stays inside them.
        let corner = theme::SHEET_RADIUS - theme::BORDER_WIDTH;
        container::Style::default()
            .background(theme.palette().background)
            .border(Border {
                radius: iced::border::bottom(corner),
                width: 0.0,
                color: iced::Color::TRANSPARENT,
            })
    });
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(|theme: &Theme| {
            container::Style::default()
                .background(Derived::SheetFooterRule.resolve(theme.palette()))
        });
    // Inset by the outline's width, so the footer's fill never covers the outline.
    container(column![
        container(body).padding(theme::SHEET_PADDING),
        rule,
        footer
    ])
    .padding(Padding::new(theme::BORDER_WIDTH))
    .width(Length::Fixed(theme::SHEET_WIDTH))
    .style(theme::sheet_surface)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar draws the fraction it is given, clamped to the whole, and nothing for none: a job
    /// with no truthful total is never drawn nearly done.
    #[test]
    fn a_bar_is_filled_exactly_as_far_as_the_work_reports() {
        assert_eq!(bar_fill(Some(0.24)), Some(0.24));
        assert_eq!(bar_fill(Some(0.0)), Some(0.0));
        assert_eq!(bar_fill(Some(1.5)), Some(1.0));
        assert_eq!(bar_fill(Some(-0.5)), Some(0.0));
        assert_eq!(bar_fill(None), None);
        assert_eq!(bar_fill(Some(f32::NAN)), None);
        assert_eq!(bar_fill(Some(f32::INFINITY)), None);
    }

    #[test]
    fn a_percentage_never_rounds_up_to_done() {
        assert_eq!(percent(0.24), 24);
        assert_eq!(percent(0.996), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(2.0), 100);
    }

    /// components.png's `24% · 2 jobs`; `working` in place of a percentage the work cannot
    /// state; the job count only when there is more than one.
    #[test]
    fn the_status_caption_says_how_far_and_how_many() {
        assert_eq!(status_caption(Some(0.24), 2), "24% \u{b7} 2 jobs");
        assert_eq!(status_caption(Some(0.24), 1), "24%");
        assert_eq!(status_caption(None, 2), "working \u{b7} 2 jobs");
        assert_eq!(status_caption(None, 1), "working");
        assert_eq!(status_caption(Some(f32::NAN), 3), "working \u{b7} 3 jobs");
        assert_eq!(status_caption(Some(0.5), 0), "50%");
    }

    #[test]
    fn a_count_the_work_cannot_state_reads_working() {
        let known = WorkProgress {
            count: Some("312 of 612 files".into()),
            fraction: Some(0.51),
        };
        assert_eq!(count_text(&known), "312 of 612 files");
        assert_eq!(count_text(&WorkProgress::default()), "working");
    }

    #[test]
    fn every_state_builds() {
        for fraction in [Some(0.24), None] {
            for jobs in [1, 2] {
                let _: Element<'_, ()> = status_job(
                    &StatusJobModel {
                        label: "Indexing ~/Pictures".into(),
                        fraction,
                        jobs,
                    },
                    (),
                );
            }
            for estimate in [Some("about 4 s left".to_owned()), None] {
                let _: Element<'_, ()> = progress_sheet(
                    &ProgressSheetModel {
                        icon: Icon::Drive,
                        title: "Reading the NIKON Z 8 card".into(),
                        note: "Frames appear as soon as it is done.".into(),
                        progress: WorkProgress {
                            count: fraction.map(|_| "312 of 612 files".into()),
                            fraction,
                        },
                        estimate,
                    },
                    (),
                    (),
                );
            }
        }
    }
}
