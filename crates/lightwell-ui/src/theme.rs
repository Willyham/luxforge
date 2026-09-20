//! Design tokens for the Develop workspace and the styling functions built from them.
//!
//! Every colour, size, spacing and radius a widget uses comes from this module, so the app never
//! writes an ad hoc colour or size. Values are copied from the visual language table in
//! `docs/design/develop-workspace.md`; the unit tests in this module assert the copy is exact.

use crate::geometry::{Fill, FillStops, Segment};
use iced::widget::{button, container, slider, text_input};
use iced::{Background, Border, Color, Degrees, Gradient, Shadow, Theme, gradient};

// -- Surfaces ---------------------------------------------------------------------------------

/// The darkest surface; the photograph sits on it.
pub const CANVAS: Color = Color::from_rgb8(0x19, 0x19, 0x1b);
/// Side panels and status bar.
pub const PANEL: Color = Color::from_rgb8(0x20, 0x20, 0x23);
/// Title bar, floating strips, notices.
pub const BAR: Color = Color::from_rgb8(0x23, 0x23, 0x26);
/// Buttons, chips, text fields.
pub const CONTROL: Color = Color::from_rgb8(0x2c, 0x2c, 0x31);
/// All dividers and outlines: 6% white.
pub const BORDER: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.06,
};

// -- Text ---------------------------------------------------------------------------------------

/// Primary text.
pub const TEXT_PRIMARY: Color = Color::from_rgb8(0xe8, 0xe8, 0xea);
/// Secondary text.
pub const TEXT_SECONDARY: Color = Color::from_rgb8(0xa8, 0xa8, 0xae);
/// Tertiary text.
pub const TEXT_TERTIARY: Color = Color::from_rgb8(0x77, 0x77, 0x7f);

// -- Ink and accent -------------------------------------------------------------------------

/// The one warm accent. Used only for: the current history entry, an active canvas mode, a
/// non-neutral module dot, a slider being dragged, and Apply. Never for a slider's rail fill at
/// rest, which uses [`TEXT_SECONDARY`] instead (see [`slider_style`]).
pub const ACCENT: Color = Color::from_rgb8(0xe2, 0xb4, 0x6a);
/// Highlight clipping indicator. Reserved for clipping; never reused as a general warning tint.
pub const CLIPPING_HIGHLIGHT: Color = Color::from_rgb8(0xe5, 0x53, 0x4b);
/// Shadow clipping indicator. Reserved for clipping.
pub const CLIPPING_SHADOW: Color = Color::from_rgb8(0x4c, 0x8b, 0xe0);

// -- Type sizes -----------------------------------------------------------------------------

/// Control text.
pub const SIZE_CONTROL: f32 = 12.0;
/// Semibold module and section titles.
pub const SIZE_TITLE: f32 = 13.0;
/// Captions.
pub const SIZE_CAPTION: f32 = 11.0;
/// Capitalised section labels.
pub const SIZE_SECTION_LABEL: f32 = 10.5;

// -- Grid ------------------------------------------------------------------------------------

/// The 8 pt spacing grid unit.
pub const SPACING: f32 = 8.0;
/// The corner radius used across controls, chips and cards.
pub const RADIUS: f32 = 6.0;
/// The 1 px border width used across dividers and outlines.
pub const BORDER_WIDTH: f32 = 1.0;
/// A fixed width for a right-aligned value field, wide enough for a signed value with a decimal
/// and a short unit (see [`crate::value_text`]'s tabular-numeral note).
pub const VALUE_WIDTH: f32 = 64.0;

/// Builds the dark, custom Lightwell theme from the tokens above. There is no light theme yet;
/// see the [visual language](../../../docs/design/develop-workspace.md#visual-language) decision.
pub fn theme() -> Theme {
    Theme::custom(
        "Lightwell".to_string(),
        iced::theme::Palette {
            background: PANEL,
            text: TEXT_PRIMARY,
            primary: ACCENT,
            success: ACCENT,
            warning: ACCENT,
            danger: CLIPPING_HIGHLIGHT,
        },
    )
}

fn surface(background: Color) -> container::Style {
    container::Style::default().background(background)
}

fn bordered_surface(background: Color) -> container::Style {
    surface(background).border(Border {
        color: BORDER,
        width: BORDER_WIDTH,
        radius: RADIUS.into(),
    })
}

/// The canvas surface: flat, no border (the photograph's own edge reads as the boundary).
pub fn canvas_surface(_theme: &Theme) -> container::Style {
    surface(CANVAS)
}

/// A side panel or the status bar: flat, no border.
pub fn panel_surface(_theme: &Theme) -> container::Style {
    surface(PANEL)
}

/// A floating bar, notice or title bar: bordered, rounded.
pub fn bar_surface(_theme: &Theme) -> container::Style {
    bordered_surface(BAR)
}

/// A control surface (chip, menu, card body): bordered, rounded.
pub fn control_surface(_theme: &Theme) -> container::Style {
    bordered_surface(CONTROL)
}

/// A control surface with its border tinted [`ACCENT`], for a warning notice's card.
pub fn warning_surface(_theme: &Theme) -> container::Style {
    surface(BAR).border(Border {
        color: ACCENT,
        width: BORDER_WIDTH,
        radius: RADIUS.into(),
    })
}

/// A plain, background-free button: list rows, chips and inline menu items.
pub fn button_plain(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(CONTROL)),
        button::Status::Active | button::Status::Disabled => None,
    };

    button::Style {
        background,
        text_color: text_color_for(status),
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// The one accent-filled button per surface: Apply, a selected mode or a primary action.
pub fn button_accent(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Disabled => CONTROL,
        _ => ACCENT,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: match status {
            button::Status::Disabled => TEXT_TERTIARY,
            _ => CANVAS,
        },
        border: Border {
            radius: RADIUS.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A square icon-glyph button: mode strip entries, header actions, context toggles.
pub fn button_icon(_theme: &Theme, status: button::Status) -> button::Style {
    button_plain(_theme, status)
}

/// A selected icon button or chip: tinted with the accent.
pub fn button_selected(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(Color { a: 0.18, ..ACCENT })),
        text_color: if matches!(status, button::Status::Disabled) {
            TEXT_TERTIARY
        } else {
            ACCENT
        },
        border: Border {
            radius: RADIUS.into(),
            width: BORDER_WIDTH,
            color: ACCENT,
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

fn text_color_for(status: button::Status) -> Color {
    match status {
        button::Status::Disabled => TEXT_TERTIARY,
        _ => TEXT_PRIMARY,
    }
}

/// The value field's text input: transparent until focused or invalid.
pub fn text_input_style(invalid: bool) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_theme, status| {
        let border_color = if invalid {
            CLIPPING_HIGHLIGHT
        } else {
            match status {
                text_input::Status::Focused { .. } => ACCENT,
                _ => BORDER,
            }
        };

        text_input::Style {
            background: Background::Color(CONTROL),
            border: Border {
                color: border_color,
                width: BORDER_WIDTH,
                radius: RADIUS.into(),
            },
            icon: TEXT_TERTIARY,
            placeholder: TEXT_TERTIARY,
            value: TEXT_PRIMARY,
            selection: Color { a: 0.35, ..ACCENT },
        }
    }
}

/// The colour a rail [`Fill`] draws with at rest. The fill never uses [`ACCENT`]: the visual
/// language reserves the accent for a dragging handle, not the rail underneath it.
fn fill_color(fill: Fill) -> Color {
    match fill {
        Fill::Empty => CONTROL,
        Fill::Filled => TEXT_SECONDARY,
    }
}

fn segment_background(segment: Segment) -> Background {
    match segment {
        Segment::Solid(fill) => Background::Color(fill_color(fill)),
        Segment::Split { at, before, after } => {
            // A gradient with two color stops packed close together, rather than one shared
            // offset: `Linear::add_stop` keeps only the last stop written at a given offset, so a
            // literal hard edge (both stops at the same fraction) would silently lose one colour.
            let at = at as f32;
            let epsilon = 0.004;
            let low = (at - epsilon).max(0.0);
            let high = (at + epsilon).min(1.0);
            let before = fill_color(before);
            let after = fill_color(after);

            Background::Gradient(Gradient::Linear(
                gradient::Linear::new(Degrees(90.0))
                    .add_stop(0.0, before)
                    .add_stop(low, before)
                    .add_stop(high, after)
                    .add_stop(1.0, after),
            ))
        }
    }
}

/// Builds the slider rail and handle style from its fill geometry and whether its handle is being
/// dragged (an open draft). The handle turns [`ACCENT`] only while dragging; the rail fill stays
/// neutral in every state, per the visual language's accent list.
pub fn slider_style(
    fill: FillStops,
    dragging: bool,
) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_theme, status| {
        let active = dragging || matches!(status, slider::Status::Dragged);

        slider::Style {
            rail: slider::Rail {
                backgrounds: (
                    segment_background(fill.left),
                    segment_background(fill.right),
                ),
                width: 2.0,
                border: Border {
                    radius: 1.0.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 6.0 },
                background: Background::Color(if active { ACCENT } else { TEXT_PRIMARY }),
                border_color: Color::TRANSPARENT,
                border_width: 0.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_tokens_match_the_visual_language_table() {
        assert_eq!(CANVAS, Color::from_rgb8(0x19, 0x19, 0x1b));
        assert_eq!(PANEL, Color::from_rgb8(0x20, 0x20, 0x23));
        assert_eq!(BAR, Color::from_rgb8(0x23, 0x23, 0x26));
        assert_eq!(CONTROL, Color::from_rgb8(0x2c, 0x2c, 0x31));
    }

    #[test]
    fn border_token_is_six_percent_white() {
        assert_eq!(BORDER.r, 1.0);
        assert_eq!(BORDER.g, 1.0);
        assert_eq!(BORDER.b, 1.0);
        assert!((BORDER.a - 0.06).abs() < f32::EPSILON);
    }

    #[test]
    fn text_tokens_match_the_visual_language_table() {
        assert_eq!(TEXT_PRIMARY, Color::from_rgb8(0xe8, 0xe8, 0xea));
        assert_eq!(TEXT_SECONDARY, Color::from_rgb8(0xa8, 0xa8, 0xae));
        assert_eq!(TEXT_TERTIARY, Color::from_rgb8(0x77, 0x77, 0x7f));
    }

    #[test]
    fn accent_and_clipping_tokens_match_the_visual_language_table() {
        assert_eq!(ACCENT, Color::from_rgb8(0xe2, 0xb4, 0x6a));
        assert_eq!(CLIPPING_HIGHLIGHT, Color::from_rgb8(0xe5, 0x53, 0x4b));
        assert_eq!(CLIPPING_SHADOW, Color::from_rgb8(0x4c, 0x8b, 0xe0));
    }

    #[test]
    fn type_sizes_match_the_visual_language_table() {
        assert_eq!(SIZE_CONTROL, 12.0);
        assert_eq!(SIZE_TITLE, 13.0);
        assert_eq!(SIZE_CAPTION, 11.0);
        assert_eq!(SIZE_SECTION_LABEL, 10.5);
    }

    #[test]
    fn grid_matches_the_visual_language_table() {
        assert_eq!(SPACING, 8.0);
        assert_eq!(RADIUS, 6.0);
        assert_eq!(BORDER_WIDTH, 1.0);
    }

    #[test]
    fn dragging_handle_turns_accent() {
        let style = slider_style(
            FillStops {
                left: Segment::Solid(Fill::Empty),
                right: Segment::Solid(Fill::Empty),
            },
            true,
        )(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(ACCENT));
    }

    #[test]
    fn resting_handle_is_not_accent() {
        let style = slider_style(
            FillStops {
                left: Segment::Solid(Fill::Empty),
                right: Segment::Solid(Fill::Empty),
            },
            false,
        )(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(TEXT_PRIMARY));
    }

    #[test]
    fn solid_segments_use_flat_colors_not_gradients() {
        let style = slider_style(
            FillStops {
                left: Segment::Solid(Fill::Filled),
                right: Segment::Solid(Fill::Empty),
            },
            false,
        )(&theme(), slider::Status::Active);
        assert_eq!(style.rail.backgrounds.0, Background::Color(TEXT_SECONDARY));
        assert_eq!(style.rail.backgrounds.1, Background::Color(CONTROL));
    }
}
