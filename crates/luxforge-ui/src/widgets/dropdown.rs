//! A button that opens a menu under it, and the menu, as the mask-panels board draws New mask and
//! Add component (`.tb` with a plus and a chevron) and the kind menu (`.menu`).
//!
//! The menu is a [`theme::MENU_WIDTH`] list of [`theme::MENU_ITEM_HEIGHT`] items — an icon, a
//! label and a right-aligned key or tag (`L`, `model`) — with separators between groups and
//! disabled items in tertiary ink, each carrying its refusal as a tooltip. [`menu_list`] is the
//! list alone; the caller drops it under [`dropdown_button`], or anchors it to a row, through
//! [`crate::popover`].

use super::icon_button::{Icon, icon, with_tooltip};
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Space, button, container, row, text};
use iced::{Alignment, Length};

/// Plain data for a dropdown button.
#[derive(Debug, Clone, PartialEq)]
pub struct DropdownButtonModel {
    pub label: String,
    /// [`theme::COMPACT_BUTTON_HEIGHT`] rather than [`theme::DROPDOWN_HEIGHT`], as Add component
    /// is under the components.
    pub compact: bool,
    pub enabled: bool,
}

/// One entry of a menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry<M> {
    Item(MenuItem<M>),
    /// A rule between two groups of items.
    Separator,
}

/// One menu item. With no message it is drawn disabled.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem<M> {
    pub icon: Option<Icon>,
    pub label: String,
    /// A key that does the same (`L`) or a short tag (`model`, `install…`), right-aligned.
    pub trailing: Option<String>,
    pub on_press: Option<M>,
    /// Why a disabled item is refused, in the host's words, shown as the item's tooltip so a
    /// greyed item always says what would make it available. Ignored while the item is enabled.
    pub reason: Option<String>,
}

/// Renders the button: a plus, the label and a chevron on the Control surface.
pub fn dropdown_button<'a, M: Clone + 'a>(
    model: &DropdownButtonModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = if model.enabled {
        Token::Text
    } else {
        Token::TextTertiary
    };
    let (height, padding) = if model.compact {
        (
            theme::COMPACT_BUTTON_HEIGHT,
            theme::COMPACT_DROPDOWN_PADDING,
        )
    } else {
        (theme::DROPDOWN_HEIGHT, theme::DROPDOWN_PADDING)
    };
    let content = row![
        icon(Icon::Plus, theme::DROPDOWN_ICON_SIZE, ink),
        text(model.label.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .style(theme::ink(ink)),
        icon(Icon::ChevronDown, theme::DROPDOWN_CHEVRON_SIZE, ink),
    ]
    .spacing(theme::BUTTON_ICON_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    button(content)
        .padding([0.0, padding])
        .height(Length::Fixed(height))
        .style(theme::button_control)
        .on_press_maybe(on_press.filter(|_| model.enabled))
        .into()
}

/// Renders a menu's list on its own lifted surface.
pub fn menu_list<'a, M: Clone + 'a>(entries: Vec<MenuEntry<M>>) -> Element<'a, M> {
    let list = Column::with_children(entries.into_iter().map(|entry| {
        match entry {
            MenuEntry::Item(item) => menu_item(item),
            MenuEntry::Separator => container(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fixed(theme::BORDER_WIDTH))
                    .style(theme::menu_separator),
            )
            .padding(theme::MENU_SEPARATOR_MARGIN)
            .width(Length::Fill)
            .into(),
        }
    }));
    container(list)
        .padding(theme::MENU_PADDING)
        .width(Length::Fixed(theme::MENU_WIDTH))
        .style(theme::menu_surface)
        .into()
}

fn menu_item<'a, M: Clone + 'a>(item: MenuItem<M>) -> Element<'a, M> {
    let enabled = item.on_press.is_some();
    let ink = if enabled {
        Token::Text
    } else {
        Token::TextTertiary
    };
    let glyph: Element<'a, M> = match item.icon {
        Some(glyph) => icon(glyph, theme::HEADER_ICON_SIZE, ink),
        None => Space::new()
            .width(Length::Fixed(theme::HEADER_ICON_SIZE))
            .into(),
    };
    let mut content = row![
        glyph,
        text(item.label)
            .size(theme::SIZE_CONTROL)
            .wrapping(Wrapping::None)
            .style(theme::ink(ink)),
        Space::new().width(Length::Fill),
    ]
    .spacing(theme::MENU_ITEM_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    if let Some(trailing) = item.trailing {
        content = content.push(
            text(trailing)
                .size(theme::SIZE_SMALL_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    let reason = item.reason.filter(|_| !enabled);
    let control = button(content)
        .padding([0.0, theme::MENU_ITEM_PADDING])
        .width(Length::Fill)
        .height(Length::Fixed(theme::MENU_ITEM_HEIGHT))
        .style(theme::menu_item)
        .on_press_maybe(item.on_press);
    match reason {
        // The menu sits at the panel's right edge, so the reason opens to its left.
        Some(reason) => with_tooltip(control, reason, iced::widget::tooltip::Position::Left),
        None => control.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_builds() {
        for (compact, enabled) in [(false, true), (true, true), (false, false)] {
            let model = DropdownButtonModel {
                label: "New mask".into(),
                compact,
                enabled,
            };
            let entries = vec![
                MenuEntry::Item(MenuItem {
                    icon: Some(Icon::Linear),
                    label: "Linear gradient".into(),
                    trailing: Some("L".into()),
                    on_press: Some(1),
                    reason: None,
                }),
                MenuEntry::Separator,
                MenuEntry::Item(MenuItem {
                    icon: None,
                    label: "Background".into(),
                    trailing: Some("install\u{2026}".into()),
                    on_press: None,
                    reason: Some("Selection model not installed".into()),
                }),
            ];
            let _: Element<'_, u8> = dropdown_button(&model, Some(0));
            let _: Element<'_, u8> = menu_list(entries);
        }
    }
}
