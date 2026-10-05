//! A row of mutually exclusive labelled options on one track, as the title bar's view control
//! draws them: the options inset on a [`Token::TabTrack`], the selected one raised on
//! [`Token::TabSelected`] and never tinted with the accent.
//!
//! [`segmented`] builds the whole control from its options. [`segment`] and [`segment_track`] are
//! its two parts, for a control whose last segment is sometimes something other than a label (the
//! title bar's typed zoom field). [`chevron_segment`] is a segment that has more behind it (the
//! title bar's percentage, which drops the zoom stops).

use crate::theme;
use crate::widgets::icon_button::{Icon, icon};
use crate::{Element, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, button, container, row, text};
use iced::{Alignment, Length};

/// Plain data for a segmented control.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentedModel {
    pub options: Vec<String>,
    pub selected: usize,
    pub enabled: bool,
}

/// Renders one segmented control: a row of mutually exclusive labelled options.
pub fn segmented<'a, M: Clone + 'a>(
    model: &SegmentedModel,
    on_select: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    segment_track(
        model
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                segment(
                    option.clone(),
                    index == model.selected,
                    model.enabled.then(|| on_select(index)),
                )
            })
            .collect(),
    )
}

/// One segment: its label at control size, [`theme::SEGMENT_HEIGHT`] tall with
/// [`theme::SEGMENT_PADDING`] either side, selected or not. `None` disables it.
pub fn segment<'a, M: Clone + 'a>(
    label: String,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    segment_with(segment_label(label).into(), selected, on_press)
}

/// A [`segment`] with a small chevron after its label, in the label's own ink, for a segment that
/// has more behind it.
pub fn chevron_segment<'a, M: Clone + 'a>(
    label: String,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (on_press.is_some(), selected) {
        (false, _) => Token::TextTertiary,
        (true, true) => Token::TextBright,
        (true, false) => Token::TextSecondary,
    };
    let content = row![
        segment_label(label),
        icon(Icon::ChevronDown, theme::SEGMENT_CHEVRON_SIZE, ink),
    ]
    .spacing(theme::SEGMENT_CHEVRON_SPACING)
    .align_y(Alignment::Center);
    segment_with(content.into(), selected, on_press)
}

fn segment_label<'a>(label: String) -> iced::widget::Text<'a, crate::Theme> {
    text(label)
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .wrapping(Wrapping::None)
}

fn segment_with<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(container(content).center_y(Length::Fill))
        .padding([0.0, theme::SEGMENT_PADDING])
        .height(Length::Fixed(theme::SEGMENT_HEIGHT))
        .style(theme::segment(selected))
        .on_press_maybe(on_press)
        .into()
}

/// The track the segments sit on, [`theme::SEGMENT_INSET`] around and between them.
pub fn segment_track<'a, M: 'a>(segments: Vec<Element<'a, M>>) -> Element<'a, M> {
    container(
        Row::with_children(segments)
            .spacing(theme::SEGMENT_INSET)
            .align_y(Alignment::Center),
    )
    .padding(theme::SEGMENT_INSET)
    .style(theme::segment_track)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segmented_control_builds_selected_disabled_and_mixed() {
        for (selected, enabled) in [(0, true), (1, false), (usize::MAX, true)] {
            let _: Element<'_, ()> = segmented(
                &SegmentedModel {
                    options: vec!["Fit".into(), "100%".into()],
                    selected,
                    enabled,
                },
                |_| (),
            );
        }
        // A track of hand-built segments, as the title bar builds its zoom control.
        let _: Element<'_, ()> = segment_track(vec![
            segment("Fit".into(), true, Some(())),
            segment("18%".into(), false, None),
            chevron_segment("18%".into(), true, Some(())),
            chevron_segment("18%".into(), false, None),
        ]);
    }
}
