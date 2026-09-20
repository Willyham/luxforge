//! The floating mode strip: canvas modes plus a separated group of view toggles.

use super::floating_bar::floating_bar;
use crate::theme;
use iced::widget::{Space, button, container, text, tooltip};
use iced::{Element, Length, Theme};

/// One canvas-mode entry (pointer, crop, neutral picker, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeEntry {
    pub label: String,
    /// A single-letter keyboard shortcut, shown in the entry's tooltip.
    pub shortcut: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}

/// One view-overlay toggle (e.g. thirds), in the strip's separated second group.
#[derive(Debug, Clone, PartialEq)]
pub struct ToggleEntry {
    pub label: String,
    /// A single-letter keyboard shortcut, shown in the entry's tooltip.
    pub shortcut: Option<String>,
    pub on: bool,
}

/// Renders the mode strip: `modes` in registry order, then a separator, then `toggles`.
pub fn mode_strip<'a, M: Clone + 'a>(
    modes: &[ModeEntry],
    on_select: impl Fn(usize) -> M + 'a,
    toggles: &[ToggleEntry],
    on_toggle: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let mut children: Vec<Element<'a, M>> = Vec::new();

    for (index, mode) in modes.iter().enumerate() {
        let style = if mode.selected {
            theme::button_selected
        } else {
            theme::button_icon
        };
        let control = button(text(mode.label.clone()).size(theme::SIZE_CONTROL))
            .padding([4.0, 10.0])
            .style(style)
            .on_press_maybe(mode.enabled.then(|| on_select(index)));

        children.push(with_shortcut(control, &mode.label, &mode.shortcut));
    }

    if !modes.is_empty() && !toggles.is_empty() {
        children.push(separator());
    }

    for (index, toggle) in toggles.iter().enumerate() {
        let style = if toggle.on {
            theme::button_selected
        } else {
            theme::button_icon
        };
        let control = button(text(toggle.label.clone()).size(theme::SIZE_CONTROL))
            .padding([4.0, 10.0])
            .style(style)
            .on_press(on_toggle(index));

        children.push(with_shortcut(control, &toggle.label, &toggle.shortcut));
    }

    floating_bar(children)
}

/// Wraps a control in a tooltip naming its label and shortcut, when one is given.
fn with_shortcut<'a, M: Clone + 'a>(
    control: impl Into<Element<'a, M>>,
    label: &str,
    shortcut: &Option<String>,
) -> Element<'a, M> {
    let control = control.into();

    match shortcut {
        Some(key) => tooltip(
            control,
            container(
                text(format!("{label} \u{00b7} {key}"))
                    .size(theme::SIZE_CAPTION)
                    .color(theme::TEXT_PRIMARY),
            )
            .padding(6.0)
            .style(theme::bar_surface),
            tooltip::Position::Top,
        )
        .into(),
        None => control,
    }
}

fn separator<'a, M: Clone + 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|_theme: &Theme| iced::widget::container::Style::default().background(theme::BORDER))
        .into()
}
