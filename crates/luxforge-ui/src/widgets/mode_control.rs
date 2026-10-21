//! A mask component's mode as one chip, as the range-rows board draws `.mode`: the mode's own
//! glyph and a chevron on a small track. A press asks for the caller's menu of the three modes
//! ([`mode_menu`]), which the caller drops under the chip with [`crate::popover`]; the chip itself
//! only shows the mode. A Subtract glyph reads in [`theme::CLIPPING_HIGHLIGHT`] and an Intersect in
//! [`theme::CLIPPING_SHADOW`] — the chip never sits over the photograph, so the two clipping hues
//! cannot be mistaken for clipping there.
//!
//! A mask's first component is always Add and draws no chip. The same chip ends the Add row, after
//! an `as` caption, for the next component's mode.

use super::dropdown::{MenuEntry, MenuItem};
use super::icon_button::{Icon, icon, with_tooltip};
use crate::theme;
use crate::{Element, Ink, Token};
use iced::widget::{button, row, tooltip};
use iced::{Alignment, Length};

/// How a component combines with the ones above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombineMode {
    Add,
    Subtract,
    Intersect,
}

impl CombineMode {
    /// The three modes, in the menu's order.
    pub const ALL: [Self; 3] = [Self::Add, Self::Subtract, Self::Intersect];

    /// The glyph the chip and the menu draw for the mode.
    pub(crate) const fn icon(self) -> Icon {
        match self {
            Self::Add => Icon::Plus,
            Self::Subtract => Icon::Minus,
            Self::Intersect => Icon::Intersect,
        }
    }

    /// What the menu calls the mode.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
        }
    }
}

/// Plain data for one mode chip.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeControlModel {
    pub selected: CombineMode,
    /// The chip's tooltip, such as `Subtract · change how this component combines`.
    pub tooltip: String,
    /// The chip's menu is open: the chip is drawn raised.
    pub open: bool,
    pub enabled: bool,
}

/// A glyph's ink: Subtract in the highlight clipping red, Intersect in the shadow clipping blue,
/// Add in bright text, and every glyph faint on a disabled chip. The two clipping colours are fixed,
/// the same in every theme, as the mask-panels board draws them.
pub(crate) fn mode_ink(mode: CombineMode, enabled: bool) -> Ink {
    match (enabled, mode) {
        (false, _) => Token::TextFaint.into(),
        (true, CombineMode::Add) => Token::TextBright.into(),
        (true, CombineMode::Subtract) => theme::CLIPPING_HIGHLIGHT.into(),
        (true, CombineMode::Intersect) => theme::CLIPPING_SHADOW.into(),
    }
}

/// Renders one mode chip. A press publishes `on_open`, or nothing when it is `None` or the chip is
/// disabled.
pub fn mode_control<'a, M: Clone + 'a>(
    model: &ModeControlModel,
    on_open: Option<M>,
) -> Element<'a, M> {
    let content = row![
        icon(
            model.selected.icon(),
            theme::MODE_GLYPH_SIZE,
            mode_ink(model.selected, model.enabled)
        ),
        icon(
            Icon::ChevronDown,
            theme::MODE_CHEVRON_SIZE,
            if model.enabled {
                Token::TextTertiary
            } else {
                Token::TextFaint
            }
        ),
    ]
    .spacing(theme::MODE_CHIP_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    let chip = button(content)
        .padding([0.0, theme::MODE_CHIP_PADDING])
        .height(Length::Fixed(theme::MODE_CHIP_HEIGHT))
        .style(theme::mode_chip(model.open))
        .on_press_maybe(on_open.filter(|_| model.enabled));
    with_tooltip(chip, model.tooltip.clone(), tooltip::Position::Top)
}

/// The chip's menu: the three modes with their glyphs. A press on another mode publishes
/// `on_select` with it; a press on the current one publishes `on_keep`, which leaves it as it is.
pub fn mode_menu<M: Clone>(
    selected: CombineMode,
    on_select: impl Fn(CombineMode) -> M,
    on_keep: M,
) -> Vec<MenuEntry<M>> {
    CombineMode::ALL
        .into_iter()
        .map(|mode| {
            MenuEntry::Item(MenuItem {
                icon: Some(mode.icon()),
                label: mode.label().to_owned(),
                trailing: (mode == selected).then(|| "current".to_owned()),
                on_press: Some(if mode == selected {
                    on_keep.clone()
                } else {
                    on_select(mode)
                }),
                reason: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subtract_is_red_and_an_intersect_blue() {
        assert_eq!(
            mode_ink(CombineMode::Subtract, true),
            theme::CLIPPING_HIGHLIGHT.into()
        );
        assert_eq!(
            mode_ink(CombineMode::Intersect, true),
            theme::CLIPPING_SHADOW.into()
        );
        assert_eq!(mode_ink(CombineMode::Add, true), Token::TextBright.into());
        for mode in CombineMode::ALL {
            assert_eq!(mode_ink(mode, false), Token::TextFaint.into());
        }
    }

    #[test]
    fn the_menu_offers_every_mode_and_keeps_the_current_one() {
        let entries = mode_menu(CombineMode::Subtract, Some, None);
        let pressed: Vec<_> = entries
            .iter()
            .map(|entry| match entry {
                MenuEntry::Item(item) => (item.label.as_str(), item.on_press),
                _ => panic!("the mode menu holds only items"),
            })
            .collect();
        assert_eq!(
            pressed,
            vec![
                ("Add", Some(Some(CombineMode::Add))),
                ("Subtract", Some(None)),
                ("Intersect", Some(Some(CombineMode::Intersect))),
            ]
        );
    }

    #[test]
    fn every_state_builds() {
        for selected in CombineMode::ALL {
            for (open, enabled) in [(false, true), (true, true), (false, false)] {
                let _: Element<'_, ()> = mode_control(
                    &ModeControlModel {
                        selected,
                        tooltip: selected.label().into(),
                        open,
                        enabled,
                    },
                    Some(()),
                );
            }
        }
    }
}
