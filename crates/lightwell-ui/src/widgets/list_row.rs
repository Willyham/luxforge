//! One row in a history, version or recipe list: [`theme::LIST_ROW_HEIGHT`] tall, the sequence
//! right-aligned in a [`theme::LIST_LEADING_WIDTH`] box, the marker, the label and the actor. The
//! current entry's row is tinted, as the Develop workspace references draw it.

use super::text::{caption, section_label};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::alignment::Horizontal;
use iced::widget::{Space, button, container, mouse_area, row};
use iced::{Alignment, Border, Color, Element, Length, Theme};

/// A list row's marker, drawn as a small circle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Filled accent: the current entry.
    Current,
    /// An accent outline: a previewed entry.
    Previewed,
    /// A hollow, neutral outline: an ordinary entry.
    Plain,
    /// No marker drawn.
    None,
}

/// Plain data for one list row.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRowModel {
    pub marker: Marker,
    /// A short leading caption, e.g. a sequence number.
    pub leading: String,
    pub label: String,
    /// A short trailing caption, e.g. the actor.
    pub trailing: Option<String>,
    /// Dimmed for an undone or branch row.
    pub dimmed: bool,
    /// A short tag, e.g. "branch".
    pub tag: Option<String>,
    pub enabled: bool,
}

/// Renders one list row. `on_context` fires on a right-click.
pub fn list_row<'a, M: Clone + 'a>(
    model: &ListRowModel,
    on_press: Option<M>,
    on_context: Option<M>,
) -> Element<'a, M> {
    let current = model.marker == Marker::Current;
    let label_color = if model.dimmed {
        theme::TEXT_TERTIARY
    } else if current {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_LABEL
    };

    // The label and its tag take what the sequence, the marker and the actor leave, so the actor
    // keeps its full width and the label ends in an ellipsis first. The label hugs its text, so
    // the tag follows it.
    let mut labelled = row![truncated_text(
        model.label.clone(),
        theme::SIZE_CONTROL,
        theme::FONT,
        label_color,
    )]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(tag) = &model.tag {
        labelled = labelled.push(caption(tag.clone()));
    }

    let mut content = row![
        container(caption(model.leading.clone()))
            .width(Length::Fixed(theme::LIST_LEADING_WIDTH))
            .align_x(Horizontal::Right),
        marker_dot(model.marker),
        container(labelled).width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);

    if let Some(trailing) = &model.trailing {
        content = content.push(caption(trailing.clone()));
    }

    let control = button(content.height(Length::Fill))
        .padding([0.0, theme::SPACING])
        .height(Length::Fixed(theme::LIST_ROW_HEIGHT))
        .style(if current {
            theme::list_row_current
        } else {
            theme::button_plain
        })
        .on_press_maybe(model.enabled.then_some(on_press).flatten());

    let area = mouse_area(control);

    match on_context.filter(|_| model.enabled) {
        Some(message) => area.on_right_press(message).into(),
        None => area.into(),
    }
}

/// A list's capitalised heading (History, Recipe), with [`theme::LIST_HEADING_SPACING`] clear
/// under it before the first row, on top of the list's row spacing.
pub fn list_heading<'a, M: Clone + 'a>(label: &str) -> Element<'a, M> {
    container(section_label(label.to_owned()))
        .padding(iced::Padding::default().bottom(theme::LIST_HEADING_SPACING))
        .into()
}

/// A small marker circle: filled, outlined or hollow depending on [`Marker`].
fn marker_dot<'a, M: Clone + 'a>(marker: Marker) -> Element<'a, M> {
    let (background, border_color, border_width) = match marker {
        Marker::Current => (Some(theme::ACCENT), Color::TRANSPARENT, 0.0),
        Marker::Previewed => (None, theme::ACCENT, theme::MARKER_RING_WIDTH),
        Marker::Plain => (None, theme::TEXT_TERTIARY, theme::MARKER_RING_WIDTH),
        Marker::None => (None, Color::TRANSPARENT, 0.0),
    };

    container(Space::new())
        .width(Length::Fixed(theme::MARKER_SIZE))
        .height(Length::Fixed(theme::MARKER_SIZE))
        .style(move |_theme: &Theme| {
            let style = container::Style::default().border(Border {
                radius: (theme::MARKER_SIZE / 2.0).into(),
                width: border_width,
                color: border_color,
            });
            match background {
                Some(color) => style.background(color),
                None => style,
            }
        })
        .into()
}
