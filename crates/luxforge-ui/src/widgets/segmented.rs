//! A row of mutually exclusive labelled options on one track, as the title bar's view control
//! draws them: the options inset on a [`theme::SEGMENT_TRACK`], the selected one raised on
//! [`theme::SEGMENT_SELECTED`] and never tinted with the accent.
//!
//! [`segmented`] builds the whole control from its options. [`segment`] and [`segment_track`] are
//! its two parts, for a control whose last segment is sometimes something other than a label (the
//! title bar's typed zoom field). [`keyed_segment`] is a segment with the key that selects it after
//! its label, as the workspace switch draws Select `G` and Develop `D`.
//!
//! The Select workspace's filter bar draws a smaller control of the same kind (`.fseg`):
//! [`filter_segment`] on a [`filter_segment_track`], [`theme::FILTER_SEGMENT_HEIGHT`] tall in
//! [`theme::SIZE_FILTER`] text, each segment with an optional count in the accent after its label.

use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, button, container, row, text};
use iced::{Alignment, Element, Length};

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
    button(container(segment_label(label, theme::SIZE_CONTROL)).center_y(Length::Fill))
        .padding([0.0, theme::SEGMENT_PADDING])
        .height(Length::Fixed(theme::SEGMENT_HEIGHT))
        .style(theme::segment(selected))
        .on_press_maybe(on_press)
        .into()
}

/// A [`segment`] with the key that selects it after its label, in 10.5 pt tertiary ink
/// [`theme::SWITCH_HINT_SPACING`] after it, selected or not (`Select G`).
pub fn keyed_segment<'a, M: Clone + 'a>(
    label: String,
    key: String,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let content = row![
        segment_label(label, theme::SIZE_CONTROL),
        text(key)
            .size(theme::SIZE_SECTION_LABEL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(theme::SWITCH_HINT_SPACING)
    .align_y(Alignment::Center);
    button(container(content).center_y(Length::Fill))
        .padding([0.0, theme::SEGMENT_PADDING])
        .height(Length::Fixed(theme::SEGMENT_HEIGHT))
        .style(theme::segment(selected))
        .on_press_maybe(on_press)
        .into()
}

/// One filter segment (`.fseg button`): its label in [`theme::SIZE_FILTER`] text and, when given,
/// a count in the accent [`theme::FILTER_COUNT_SPACING`] after it (`Picked 18`),
/// [`theme::FILTER_SEGMENT_HEIGHT`] tall, selected or not. `None` disables it.
pub fn filter_segment<'a, M: Clone + 'a>(
    label: String,
    count: Option<String>,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let mut content = Row::new()
        .push(segment_label(label, theme::SIZE_FILTER))
        .spacing(theme::FILTER_COUNT_SPACING)
        .align_y(Alignment::Center);
    if let Some(count) = count {
        content = content.push(
            text(count)
                .size(theme::SIZE_FILTER)
                .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
                .wrapping(Wrapping::None)
                .color(theme::ACCENT),
        );
    }
    button(container(content).center_y(Length::Fill))
        .padding([0.0, theme::FILTER_SEGMENT_PADDING])
        .height(Length::Fixed(theme::FILTER_SEGMENT_HEIGHT))
        .style(theme::filter_segment(selected))
        .on_press_maybe(on_press)
        .into()
}

/// The track filter segments sit on, [`theme::SEGMENT_INSET`] around and between them, rounded
/// [`theme::FILTER_TRACK_RADIUS`].
pub fn filter_segment_track<'a, M: 'a>(segments: Vec<Element<'a, M>>) -> Element<'a, M> {
    container(
        Row::with_children(segments)
            .spacing(theme::SEGMENT_INSET)
            .align_y(Alignment::Center),
    )
    .padding(theme::SEGMENT_INSET)
    .style(theme::filter_track)
    .into()
}

/// A segment's label: one line at `size` that takes the button's ink, so the selected segment's
/// label is bright and the others secondary.
fn segment_label<'a>(label: String, size: f32) -> iced::widget::Text<'a> {
    text(label)
        .size(size)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .wrapping(Wrapping::None)
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
        ]);
        // The workspace switch's keyed segments.
        let _: Element<'_, ()> = segment_track(vec![
            keyed_segment("Select".into(), "G".into(), true, Some(())),
            keyed_segment("Develop".into(), "D".into(), false, Some(())),
        ]);
        // The filter bar's segments, one with a count and one disabled.
        let _: Element<'_, ()> = filter_segment_track(vec![
            filter_segment("All".into(), None, true, Some(())),
            filter_segment("Picked".into(), Some("18".into()), false, Some(())),
            filter_segment("Moments without a pick".into(), None, false, None),
        ]);
    }

    /// The filter track is the regular track's 2 pt inset around 22 pt segments: 26 pt, 2 pt
    /// shorter than the title bar's control.
    #[test]
    fn a_filter_track_is_two_points_shorter_than_the_title_bar_control() {
        let regular = theme::SEGMENT_HEIGHT + 2.0 * theme::SEGMENT_INSET;
        let filter = theme::FILTER_SEGMENT_HEIGHT + 2.0 * theme::SEGMENT_INSET;
        assert_eq!((regular, filter), (28.0, 26.0));
    }
}
