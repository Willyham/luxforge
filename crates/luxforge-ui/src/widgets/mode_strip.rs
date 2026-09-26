//! The floating mode strip: canvas modes plus a separated group of view toggles.

use super::icon_button::{Icon, icon};
use crate::theme;
use iced::widget::{Row, Space, button, container, text, tooltip};
use iced::{Alignment, Element, Length, Padding, Theme};

/// One canvas-mode entry (pointer, crop, mask, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeEntry {
    pub label: String,
    /// The mode's icon. A mode without one shows its label instead.
    pub icon: Option<Icon>,
    /// A single-letter keyboard shortcut, shown in the entry's tooltip.
    pub shortcut: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}

/// One view-overlay toggle (e.g. thirds), in the strip's separated second group.
#[derive(Debug, Clone, PartialEq)]
pub struct ToggleEntry {
    pub label: String,
    /// The toggle's icon. A toggle without one shows its label instead.
    pub icon: Option<Icon>,
    /// A single-letter keyboard shortcut, shown in the entry's tooltip.
    pub shortcut: Option<String>,
    pub on: bool,
}

/// Renders the mode strip: `modes` in registry order, then a rule, then `toggles`. Each entry is an
/// icon-only [`theme::STRIP_TOOL_WIDTH`] × [`theme::STRIP_TOOL_HEIGHT`] tool whose tooltip names it
/// and its letter; a selected mode and an overlay that is on take the accent.
pub fn mode_strip<'a, M: Clone + 'a>(
    modes: &[ModeEntry],
    on_select: impl Fn(usize) -> M + 'a,
    toggles: &[ToggleEntry],
    on_toggle: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::STRIP_SPACING)
        .align_y(Alignment::Center);

    for (index, mode) in modes.iter().enumerate() {
        content = content.push(tool(
            &mode.label,
            mode.icon,
            &mode.shortcut,
            mode.selected,
            mode.enabled.then(|| on_select(index)),
        ));
    }

    if !modes.is_empty() && !toggles.is_empty() {
        content = content.push(rule());
    }

    for (index, toggle) in toggles.iter().enumerate() {
        content = content.push(tool(
            &toggle.label,
            toggle.icon,
            &toggle.shortcut,
            toggle.on,
            Some(on_toggle(index)),
        ));
    }

    // The border is drawn inside the container, so it is added to the padding to keep the strip's
    // padding clear of it, as the board's CSS border does.
    container(content)
        .padding(theme::STRIP_PADDING + theme::BORDER_WIDTH)
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::STRIP_RADIUS))
        .into()
}

/// One tool: its icon (or its label when it has none) in a fixed-height button, with a tooltip
/// naming it and its letter.
fn tool<'a, M: Clone + 'a>(
    label: &str,
    glyph: Option<Icon>,
    shortcut: &Option<String>,
    selected: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match (on_press.is_some(), selected) {
        (false, _) => theme::TEXT_TERTIARY,
        (true, true) => theme::ACCENT,
        (true, false) => theme::STRIP_ICON,
    };
    let (content, width, padding): (Element<'a, M>, Length, Padding) = match glyph {
        Some(glyph) => (
            container(icon(glyph, theme::ICON_SIZE, ink))
                .center(Length::Fill)
                .into(),
            Length::Fixed(theme::STRIP_TOOL_WIDTH),
            Padding::ZERO,
        ),
        None => (
            container(text(label.to_owned()).size(theme::SIZE_CONTROL).color(ink))
                .center_y(Length::Fill)
                .into(),
            Length::Shrink,
            Padding::from([0.0, theme::BUTTON_PADDING]),
        ),
    };
    let control = button(content)
        .padding(padding)
        .width(width)
        .height(Length::Fixed(theme::STRIP_TOOL_HEIGHT))
        .style(theme::strip_tool(selected))
        .on_press_maybe(on_press);
    tooltip(
        control,
        container(
            text(tooltip_text(label, shortcut))
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_PRIMARY),
        )
        .padding(theme::TOOLTIP_PADDING)
        .style(theme::bar_surface),
        tooltip::Position::Top,
    )
    .into()
}

/// What a tool's tooltip says: its name, and its letter when it has one ("Crop (R)").
pub fn tooltip_text(label: &str, shortcut: &Option<String>) -> String {
    match shortcut {
        Some(key) => format!("{label} ({key})"),
        None => label.to_owned(),
    }
}

/// The 1 × 16 pt rule between the modes and the view toggles.
fn rule<'a, M: Clone + 'a>() -> Element<'a, M> {
    container(
        container(Space::new())
            .width(Length::Fixed(theme::BORDER_WIDTH))
            .height(Length::Fixed(theme::STRIP_RULE_HEIGHT))
            .style(|_: &Theme| container::Style::default().background(theme::STRIP_RULE)),
    )
    .padding(Padding::from([0.0, theme::STRIP_RULE_MARGIN]))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tooltip_names_the_mode_and_its_letter() {
        assert_eq!(tooltip_text("Crop", &Some("R".into())), "Crop (R)");
        assert_eq!(tooltip_text("Pick pixel", &None), "Pick pixel");
    }

    #[test]
    fn a_strip_builds_with_icons_and_with_labels() {
        let modes = [
            ModeEntry {
                label: "Pointer".into(),
                icon: Some(Icon::Pointer),
                shortcut: Some("V".into()),
                selected: true,
                enabled: true,
            },
            ModeEntry {
                label: "Frame".into(),
                icon: None,
                shortcut: None,
                selected: false,
                enabled: false,
            },
        ];
        let toggles = [ToggleEntry {
            label: "Thirds".into(),
            icon: Some(Icon::Thirds),
            shortcut: Some("O".into()),
            on: true,
        }];
        let _: Element<'_, ()> = mode_strip(&modes, |_| (), &toggles, |_| ());
    }
}
