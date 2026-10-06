//! The Select and Develop title bar's own pieces, as the catalog boards draw them: the workspace
//! switch at the bar's leading edge (`.seg` with each segment's key) and **Develop N**, the one
//! primary action, in its three states from the components board (`.tb.pri`, the busy button with
//! its bar, `.tb.dis`).
//!
//! Add a folder… is an ordinary [`crate::labelled_button`] with the folder icon; Undo, Redo and the
//! panel toggles are the title bar's existing icon buttons.

use super::icon_button::{Icon, icon};
use super::long_work::work_bar;
use super::segmented::{keyed_segment, segment_track};
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, Space, button, column, container, text};
use iced::{Alignment, Length};

/// The two workspaces the switch chooses between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTab {
    /// Browse files and the catalog, and pick (`G`).
    Select,
    /// Edit the development set (`D`).
    Develop,
}

impl WorkspaceTab {
    /// Both, in the switch's order.
    pub const ALL: [Self; 2] = [Self::Select, Self::Develop];

    /// The segment's label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Develop => "Develop",
        }
    }

    /// The key that switches to it, drawn after the label.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Select => "G",
            Self::Develop => "D",
        }
    }
}

/// Renders the workspace switch with `current` raised. Pressing a segment publishes `on_select`
/// with its workspace; with `on_select` `None` the switch draws as it would but does nothing.
pub fn workspace_switch<'a, M: Clone + 'a>(
    current: WorkspaceTab,
    on_select: Option<impl Fn(WorkspaceTab) -> M + 'a>,
) -> Element<'a, M> {
    segment_track(
        WorkspaceTab::ALL
            .into_iter()
            .map(|tab| {
                keyed_segment(
                    tab.label().to_owned(),
                    tab.key().to_owned(),
                    tab == current,
                    on_select.as_ref().map(|select| select(tab)),
                )
            })
            .collect(),
    )
}

/// What Develop N shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevelopButtonModel {
    /// Ready to develop `picks` picks: `Develop 18` on the accent with a chevron, or, at zero,
    /// `Develop` in [`Token::ZeroTick`], which does nothing.
    Ready { picks: usize },
    /// Developing: `Developing 7 of 18` on the Control surface with a bar filled `done / total`.
    Busy { done: usize, total: usize },
}

/// Renders Develop N. `on_press` is sent while it is ready with picks, and while it is busy (to
/// show the job, say); at zero picks it is never sent. Ready with picks but no `on_press`, it is
/// drawn disabled as at zero, keeping its count.
pub fn develop_button<'a, M: Clone + 'a>(
    model: &DevelopButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let label = |ink: Token| {
        text(develop_label(model))
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .style(theme::ink(ink))
    };
    let control = match *model {
        DevelopButtonModel::Ready { picks } if picks == 0 || on_press.is_none() => {
            button(container(label(Token::ZeroTick)).center_y(Length::Fill))
                .style(theme::develop_disabled)
        }
        DevelopButtonModel::Ready { .. } => button(
            Row::new()
                .push(label(Token::AccentInk))
                .push(icon(
                    Icon::ChevronRight,
                    theme::DEVELOP_CHEVRON_SIZE,
                    Token::AccentInk,
                ))
                .spacing(theme::DEVELOP_SPACING)
                .align_y(Alignment::Center)
                .height(Length::Fill),
        )
        .style(theme::button_accent)
        .on_press_maybe(on_press),
        // The busy button is at least [`theme::DEVELOP_BUSY_MIN_WIDTH`] wide, its content at the
        // left: a zero-height strut under the line sets the floor.
        DevelopButtonModel::Busy { done, total } => button(column![
            Row::new()
                .push(label(Token::Text))
                .push(
                    container(work_bar(
                        develop_fraction(done, total),
                        theme::WORK_BAR_HEIGHT,
                    ))
                    .width(Length::Fixed(theme::DEVELOP_BUSY_BAR_WIDTH)),
                )
                .spacing(theme::DEVELOP_BUSY_SPACING)
                .align_y(Alignment::Center)
                .height(Length::Fixed(theme::DEVELOP_HEIGHT)),
            Space::new()
                .width(Length::Fixed(
                    theme::DEVELOP_BUSY_MIN_WIDTH - 2.0 * theme::DEVELOP_PADDING
                ))
                .height(Length::Fixed(0.0)),
        ])
        .style(theme::develop_busy)
        .on_press_maybe(on_press),
    };
    control
        .padding([0.0, theme::DEVELOP_PADDING])
        .height(Length::Fixed(theme::DEVELOP_HEIGHT))
        .into()
}

/// Develop N's label in each state.
pub(crate) fn develop_label(model: &DevelopButtonModel) -> String {
    match *model {
        DevelopButtonModel::Ready { picks: 0 } => "Develop".to_owned(),
        DevelopButtonModel::Ready { picks } => format!("Develop {}", group_digits(picks)),
        DevelopButtonModel::Busy { done, total } => format!(
            "Developing {} of {}",
            group_digits(done),
            group_digits(total)
        ),
    }
}

/// How far a busy Develop has got: `done / total`, capped at the whole, or `None` (no fill) when
/// the total is zero and there is nothing truthful to draw.
pub(crate) fn develop_fraction(done: usize, total: usize) -> Option<f32> {
    (total > 0).then(|| done.min(total) as f32 / total as f32)
}

/// `n` with its thousands grouped by commas, as every count on the boards is written: `1,042`.
pub(crate) fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_names_each_workspace_and_its_key() {
        assert_eq!(
            WorkspaceTab::ALL.map(|tab| (tab.label(), tab.key())),
            [("Select", "G"), ("Develop", "D")]
        );
        for current in WorkspaceTab::ALL {
            let _: Element<'_, WorkspaceTab> = workspace_switch(current, Some(|tab| tab));
            let _: Element<'_, WorkspaceTab> =
                workspace_switch(current, None::<fn(WorkspaceTab) -> WorkspaceTab>);
        }
    }

    /// components.png: Develop 18, Developing 7 of 18 and a bare Develop at zero picks.
    #[test]
    fn develop_says_what_it_will_do_or_is_doing() {
        assert_eq!(
            develop_label(&DevelopButtonModel::Ready { picks: 18 }),
            "Develop 18"
        );
        assert_eq!(
            develop_label(&DevelopButtonModel::Ready { picks: 0 }),
            "Develop"
        );
        assert_eq!(
            develop_label(&DevelopButtonModel::Ready { picks: 1042 }),
            "Develop 1,042"
        );
        assert_eq!(
            develop_label(&DevelopButtonModel::Busy { done: 7, total: 18 }),
            "Developing 7 of 18"
        );
    }

    /// The busy bar is exactly done over total, never past the whole, and empty with no total.
    #[test]
    fn the_busy_bar_is_done_over_total() {
        assert_eq!(develop_fraction(7, 18), Some(7.0 / 18.0));
        assert_eq!(develop_fraction(0, 18), Some(0.0));
        assert_eq!(develop_fraction(18, 18), Some(1.0));
        assert_eq!(develop_fraction(20, 18), Some(1.0));
        assert_eq!(develop_fraction(0, 0), None);
    }

    #[test]
    fn digits_are_grouped_in_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1042), "1,042");
        assert_eq!(group_digits(200_000), "200,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn every_develop_state_builds() {
        for model in [
            DevelopButtonModel::Ready { picks: 18 },
            DevelopButtonModel::Ready { picks: 0 },
            DevelopButtonModel::Busy { done: 7, total: 18 },
            DevelopButtonModel::Busy { done: 0, total: 0 },
        ] {
            let _: Element<'_, ()> = develop_button(&model, Some(()));
            let _: Element<'_, ()> = develop_button(&model, None);
        }
    }
}
