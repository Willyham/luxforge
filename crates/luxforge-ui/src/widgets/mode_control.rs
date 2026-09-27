//! A mask component's mode as a compact three-glyph segmented control, `+ − ∩`, as the
//! mask-panels board draws `.mode`: [`theme::MODE_SEGMENT_SIZE`] segments inset
//! [`theme::MODE_INSET`] on a small track. The chosen segment is raised; a chosen Subtract reads in
//! [`theme::CLIPPING_HIGHLIGHT`] and a chosen Intersect in [`theme::CLIPPING_SHADOW`] — the control
//! never sits over the photograph, so the two clipping hues cannot be mistaken for clipping there.
//!
//! A fixed mode (a mask's first component, which is always Add) shows its one glyph alone, dimmed,
//! with the reason as its tooltip. The same control stands alone in the Add row, after an `as`
//! caption, for the next component's mode.

use super::icon_button::{Icon, icon, with_tooltip};
use crate::theme;
use iced::widget::{Row, button, container, tooltip};
use iced::{Alignment, Color, Element, Length};

/// How a component combines with the ones above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombineMode {
    Add,
    Subtract,
    Intersect,
}

impl CombineMode {
    /// The three modes, in the control's order.
    pub const ALL: [Self; 3] = [Self::Add, Self::Subtract, Self::Intersect];

    /// The glyph the control draws for the mode.
    pub const fn icon(self) -> Icon {
        match self {
            Self::Add => Icon::Plus,
            Self::Subtract => Icon::Minus,
            Self::Intersect => Icon::Intersect,
        }
    }
}

/// Plain data for one mode control.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeControlModel {
    pub selected: CombineMode,
    /// The reason the mode cannot change. `Some` draws `selected` alone, dimmed, with the reason as
    /// the tooltip.
    pub fixed: Option<String>,
    /// The tooltip while the mode can change, such as `Add · Subtract · Intersect`.
    pub tooltip: String,
    pub enabled: bool,
}

/// A glyph's ink: the chosen Subtract in the highlight clipping red, the chosen Intersect in the
/// shadow clipping blue, the chosen Add in bright text; the others tertiary. A fixed mode's glyph
/// is dimmed, and a disabled control's glyphs are all faint.
pub fn mode_ink(mode: CombineMode, selected: bool, fixed: bool, enabled: bool) -> Color {
    match (enabled, fixed, selected, mode) {
        (false, ..) => theme::TEXT_FAINT,
        (true, true, _, _) => theme::MODE_FIXED_INK,
        (true, false, false, _) => theme::TEXT_TERTIARY,
        (true, false, true, CombineMode::Add) => theme::TEXT_BRIGHT,
        (true, false, true, CombineMode::Subtract) => theme::CLIPPING_HIGHLIGHT,
        (true, false, true, CombineMode::Intersect) => theme::CLIPPING_SHADOW,
    }
}

/// The control's width for `segments` glyphs: the segments and the insets around and between them.
pub const fn mode_control_width(segments: usize) -> f32 {
    segments as f32 * theme::MODE_SEGMENT_SIZE + (segments as f32 + 1.0) * theme::MODE_INSET
}

/// Renders one mode control. A press on a segment publishes `on_select` with its mode.
pub fn mode_control<'a, M: Clone + 'a>(
    model: &ModeControlModel,
    on_select: impl Fn(CombineMode) -> M + 'a,
) -> Element<'a, M> {
    build(model, Some(&on_select))
}

/// A mode control that publishes nothing, as a disabled row draws it.
pub(crate) fn inert_mode_control<'a, M: Clone + 'a>(model: &ModeControlModel) -> Element<'a, M> {
    build(
        &ModeControlModel {
            enabled: false,
            ..model.clone()
        },
        None::<&dyn Fn(CombineMode) -> M>,
    )
}

fn build<'a, M: Clone + 'a>(
    model: &ModeControlModel,
    on_select: Option<&dyn Fn(CombineMode) -> M>,
) -> Element<'a, M> {
    let fixed = model.fixed.is_some();
    let modes: &[CombineMode] = if fixed {
        std::slice::from_ref(&model.selected)
    } else {
        &CombineMode::ALL
    };
    let segments = modes.iter().map(|&mode| {
        let selected = mode == model.selected;
        let ink = mode_ink(mode, selected, fixed, model.enabled);
        button(container(icon(mode.icon(), theme::MODE_GLYPH_SIZE, ink)).center(Length::Fill))
            .padding(0)
            .width(Length::Fixed(theme::MODE_SEGMENT_SIZE))
            .height(Length::Fixed(theme::MODE_SEGMENT_SIZE))
            .style(theme::mode_segment(selected, fixed))
            .on_press_maybe(
                on_select
                    .filter(|_| model.enabled && !fixed && !selected)
                    .map(|on_select| on_select(mode)),
            )
            .into()
    });
    let track = container(
        Row::with_children(segments)
            .spacing(theme::MODE_INSET)
            .align_y(Alignment::Center),
    )
    .padding(theme::MODE_INSET)
    .style(theme::mode_track(fixed));
    let tip = model.fixed.clone().unwrap_or_else(|| model.tooltip.clone());
    with_tooltip(track, tip, tooltip::Position::Top)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mask-panels.html: three 18 pt segments inset 1 pt, 58 pt across; a fixed one is 20 pt.
    #[test]
    fn the_control_is_its_segments_and_insets() {
        assert_eq!(mode_control_width(3), 58.0);
        assert_eq!(mode_control_width(1), 20.0);
    }

    #[test]
    fn a_chosen_subtract_is_red_and_a_chosen_intersect_blue() {
        assert_eq!(
            mode_ink(CombineMode::Subtract, true, false, true),
            theme::CLIPPING_HIGHLIGHT
        );
        assert_eq!(
            mode_ink(CombineMode::Intersect, true, false, true),
            theme::CLIPPING_SHADOW
        );
        assert_eq!(
            mode_ink(CombineMode::Add, true, false, true),
            theme::TEXT_BRIGHT
        );
        for mode in CombineMode::ALL {
            assert_eq!(mode_ink(mode, false, false, true), theme::TEXT_TERTIARY);
            assert_eq!(mode_ink(mode, true, true, true), theme::MODE_FIXED_INK);
            assert_eq!(mode_ink(mode, true, false, false), theme::TEXT_FAINT);
        }
    }

    #[test]
    fn every_state_builds() {
        for selected in CombineMode::ALL {
            for fixed in [
                None,
                Some("A mask's first component is always Add".to_string()),
            ] {
                for enabled in [true, false] {
                    let _: Element<'_, CombineMode> = mode_control(
                        &ModeControlModel {
                            selected,
                            fixed: fixed.clone(),
                            tooltip: "Add \u{b7} Subtract \u{b7} Intersect".into(),
                            enabled,
                        },
                        |mode| mode,
                    );
                }
            }
        }
    }
}
