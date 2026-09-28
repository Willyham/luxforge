//! The Masks panel's overlay row (`.ov` on the mask-panels board): the `OVERLAY` label, a
//! four-way segmented control of glyphs — off, tint over the photograph, the selection on black,
//! the photograph through the selection — then the two tint swatches and the key hint.
//!
//! The segments carry the modes' names as tooltips, from the model. The swatches choose the tint
//! and are dimmed and inert while the overlay is not a tint, since they would change nothing.

use super::icon_button::{Icon, icon, with_tooltip};
use super::segmented::segment_track;
use super::swatch_slots::{Chip, chip_spacing};
use super::text::section_label;
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, button, container, row, text, tooltip};
use iced::{Alignment, Color, Element, Length, Size};

/// How the selected mask is shown over the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    Off,
    Tint,
    /// The selection, white on black.
    Mask,
    /// The photograph through the selection, on black.
    Image,
}

impl OverlayMode {
    /// The four modes, in the control's order.
    pub const ALL: [Self; 4] = [Self::Off, Self::Tint, Self::Mask, Self::Image];

    pub(crate) const fn icon(self) -> Icon {
        match self {
            Self::Off => Icon::OverlayOff,
            Self::Tint => Icon::OverlayTint,
            Self::Mask => Icon::OverlayMask,
            Self::Image => Icon::OverlayImage,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Tint => 1,
            Self::Mask => 2,
            Self::Image => 3,
        }
    }
}

/// The tint the overlay draws a selection in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayTint {
    Green,
    White,
}

impl OverlayTint {
    /// The two tints, in the control's order.
    pub const ALL: [Self; 2] = [Self::Green, Self::White];

    pub const fn color(self) -> Color {
        match self {
            Self::Green => theme::MASK_OVERLAY_GREEN,
            Self::White => theme::MASK_OVERLAY_WHITE,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Green => 0,
            Self::White => 1,
        }
    }
}

/// Plain data for the overlay row.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayControlModel {
    /// The row's label, drawn capitalised (`Overlay`).
    pub label: String,
    pub mode: OverlayMode,
    /// Each mode's name, in [`OverlayMode::ALL`]'s order, for its tooltip.
    pub mode_names: [String; 4],
    pub tint: OverlayTint,
    /// Each tint's name, in [`OverlayTint::ALL`]'s order, for its tooltip.
    pub tint_names: [String; 2],
    /// The key that cycles the mode (`⇧M`).
    pub hint: Option<String>,
    pub enabled: bool,
}

/// Whether the tint swatches can be chosen: only while the overlay is a tint.
pub(crate) fn tints_enabled(model: &OverlayControlModel) -> bool {
    model.enabled && model.mode == OverlayMode::Tint
}

/// Renders the overlay row. A press on a segment publishes `on_mode`, on a swatch `on_tint`.
pub fn overlay_control<'a, M: Clone + 'a>(
    model: &OverlayControlModel,
    on_mode: impl Fn(OverlayMode) -> M + 'a,
    on_tint: impl Fn(OverlayTint) -> M + 'a,
) -> Element<'a, M> {
    let segments = OverlayMode::ALL
        .into_iter()
        .map(|mode| {
            let selected = mode == model.mode;
            let ink = glyph_ink(mode, selected, model.enabled);
            let control = button(
                container(icon(mode.icon(), theme::OVERLAY_GLYPH_SIZE, ink)).center(Length::Fill),
            )
            .padding([0.0, theme::OVERLAY_SEGMENT_PADDING])
            .width(Length::Fixed(
                theme::OVERLAY_GLYPH_SIZE + 2.0 * theme::OVERLAY_SEGMENT_PADDING,
            ))
            .height(Length::Fixed(theme::OVERLAY_SEGMENT_HEIGHT))
            .style(theme::segment(selected))
            .on_press_maybe((model.enabled && !selected).then(|| on_mode(mode)));
            with_tooltip(
                control,
                model.mode_names[mode.index()].clone(),
                tooltip::Position::Top,
            )
        })
        .collect();

    let swatches_enabled = tints_enabled(model);
    let swatches = Row::with_children(OverlayTint::ALL.into_iter().map(|tint| {
        let chip = Chip {
            fill: Some(tint.color()),
            size: Size::new(theme::OVERLAY_SWATCH_SIZE, theme::OVERLAY_SWATCH_SIZE),
            radius: theme::SWATCH_RADIUS,
            ring: tint == model.tint,
            opacity: if swatches_enabled {
                1.0
            } else {
                theme::SWATCH_DISABLED_OPACITY
            },
        };
        with_tooltip(
            chip.button(swatches_enabled.then(|| on_tint(tint))),
            model.tint_names[tint.index()].clone(),
            tooltip::Position::Top,
        )
    }))
    .spacing(chip_spacing(theme::OVERLAY_SPACING))
    .align_y(Alignment::Center);

    let mut content = row![
        container(section_label(model.label.clone()))
            .width(Length::Fixed(theme::OVERLAY_LABEL_WIDTH)),
        segment_track(segments),
        Space::new().width(Length::Fill),
        swatches,
    ]
    .spacing(theme::OVERLAY_SPACING)
    .align_y(Alignment::Center);
    if let Some(hint) = &model.hint {
        content = content.push(
            text(hint.clone())
                .size(theme::SIZE_SMALL_CAPTION)
                .color(theme::TEXT_TERTIARY)
                .wrapping(Wrapping::None),
        );
    }
    container(content)
        .padding([0.0, theme::OVERLAY_PADDING])
        .height(Length::Fixed(theme::OVERLAY_ROW_HEIGHT))
        .width(Length::Fill)
        .center_y(Length::Fixed(theme::OVERLAY_ROW_HEIGHT))
        .into()
}

/// A mode glyph's ink. The off ring takes the segment's text colour; the tint square shows the
/// green tint at the board's 60%; the two black squares keep their own colours, and the ink only
/// dims them while the control is disabled.
fn glyph_ink(mode: OverlayMode, selected: bool, enabled: bool) -> Color {
    let dim = |color: Color| Color {
        a: if enabled {
            color.a
        } else {
            color.a * theme::SWATCH_DISABLED_OPACITY
        },
        ..color
    };
    match mode {
        OverlayMode::Off => match (enabled, selected) {
            (false, _) => theme::TEXT_TERTIARY,
            (true, true) => theme::TEXT_BRIGHT,
            (true, false) => theme::TEXT_SECONDARY,
        },
        OverlayMode::Tint => dim(Color {
            a: theme::OVERLAY_TINT_GLYPH_OPACITY,
            ..theme::MASK_OVERLAY_GREEN
        }),
        OverlayMode::Mask | OverlayMode::Image => dim(Color::WHITE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(mode: OverlayMode, enabled: bool) -> OverlayControlModel {
        OverlayControlModel {
            label: "Overlay".into(),
            mode,
            mode_names: [
                "Overlay off".into(),
                "Tint over the photo".into(),
                "Selection on black".into(),
                "Photo through the selection".into(),
            ],
            tint: OverlayTint::Green,
            tint_names: ["Green".into(), "White".into()],
            hint: Some("\u{21e7}M".into()),
            enabled,
        }
    }

    #[test]
    fn the_swatches_choose_only_while_the_overlay_is_a_tint() {
        assert!(tints_enabled(&model(OverlayMode::Tint, true)));
        for mode in [OverlayMode::Off, OverlayMode::Mask, OverlayMode::Image] {
            assert!(!tints_enabled(&model(mode, true)));
        }
        assert!(!tints_enabled(&model(OverlayMode::Tint, false)));
    }

    #[test]
    fn the_tints_are_the_overlay_tokens() {
        assert_eq!(OverlayTint::Green.color(), theme::MASK_OVERLAY_GREEN);
        assert_eq!(OverlayTint::White.color(), theme::MASK_OVERLAY_WHITE);
    }

    #[test]
    fn every_state_builds() {
        for mode in OverlayMode::ALL {
            for tint in OverlayTint::ALL {
                for enabled in [true, false] {
                    let _: Element<'_, ()> = overlay_control(
                        &OverlayControlModel {
                            tint,
                            ..model(mode, enabled)
                        },
                        |_| (),
                        |_| (),
                    );
                }
            }
        }
    }
}
