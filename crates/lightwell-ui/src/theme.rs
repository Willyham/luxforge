//! Design tokens for the Develop workspace and the styling functions built from them.
//!
//! Every colour, size, spacing and radius a widget uses comes from this module, so the app never
//! writes an ad hoc colour or size. Values are copied from the visual language table in
//! `docs/design/develop-workspace.md`; the unit tests in this module assert the copy is exact.

use iced::widget::{button, container, slider, text_input};
use iced::{Background, Border, Color, Padding, Shadow, Theme};

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
/// A slider or field label: a step under primary, so the value on the same line reads first.
pub const TEXT_LABEL: Color = Color::from_rgb8(0xc9, 0xc9, 0xce);

// -- Slider rail and rules ----------------------------------------------------------------------

/// The empty rail.
pub const RAIL: Color = Color::from_rgb8(0x3a, 0x3a, 0x40);
/// The rail's fill between the zero tick (or the rail's start) and the handle, in every state.
pub const RAIL_FILL: Color = Color::from_rgb8(0xa3, 0xa3, 0xaa);
/// The zero tick across the rail.
pub const ZERO_TICK: Color = Color::from_rgb8(0x5a, 0x5a, 0x62);
/// The resting handle.
pub const THUMB: Color = Color::from_rgb8(0xec, 0xec, 0xee);
/// The dark ring around the handle that separates it from a light or colour rail.
pub const THUMB_OUTLINE: Color = Color::from_rgb8(0x11, 0x11, 0x13);
/// A group header's hairline rule. Opaque rather than a white alpha like [`BORDER`]: Iced blends
/// in linear light, which renders a small white alpha far brighter than the references do.
pub const RULE: Color = Color::from_rgb8(0x31, 0x31, 0x34);
/// The 1 px border above each module band, opaque for the same reason as [`RULE`].
pub const BAND_BORDER: Color = Color::from_rgb8(0x2f, 0x2f, 0x32);
/// A value's inset from the right edge of its box, so a typed value does not jump when the field
/// opens for editing.
pub const VALUE_INSET: f32 = 3.0;

// -- Ink and accent -------------------------------------------------------------------------

/// The one warm accent. Used only for: the current history entry, an active canvas mode, a
/// non-neutral module dot, a slider being dragged, a Custom group caption, and Apply. Never for a
/// slider's rail fill, which uses [`RAIL_FILL`] instead (see [`slider_style`]).
pub const ACCENT: Color = Color::from_rgb8(0xe2, 0xb4, 0x6a);
/// Highlight clipping indicator. Reserved for clipping; never reused as a general warning tint.
pub const CLIPPING_HIGHLIGHT: Color = Color::from_rgb8(0xe5, 0x53, 0x4b);
/// Shadow clipping indicator. Reserved for clipping.
pub const CLIPPING_SHADOW: Color = Color::from_rgb8(0x4c, 0x8b, 0xe0);
/// The overlay colour of a cell that holds both endpoints. It is not a third invented colour: it
/// takes its red and green from [`CLIPPING_HIGHLIGHT`] and its blue from [`CLIPPING_SHADOW`], which
/// is exactly what "red and blue at once" means and reads as magenta over a photograph. The
/// composition is asserted in this module's tests rather than written out twice.
pub const CLIPPING_BOTH: Color = Color {
    r: CLIPPING_HIGHLIGHT.r,
    g: CLIPPING_HIGHLIGHT.g,
    b: CLIPPING_SHADOW.b,
    a: 1.0,
};

/// The three histogram channel fills. They are the plain additive primaries rather than tinted
/// versions of them, because the plot's whole job is to say which channel a count belongs to and
/// what their overlap is; the alpha below is what makes the overlap readable.
pub const CHANNEL_RED: Color = Color::from_rgb8(0xff, 0x4d, 0x4d);
pub const CHANNEL_GREEN: Color = Color::from_rgb8(0x4d, 0xff, 0x7a);
pub const CHANNEL_BLUE: Color = Color::from_rgb8(0x4d, 0x9a, 0xff);
/// How opaque one channel fill is. Three overlapping fills at this alpha keep each channel
/// readable on its own and turn a full overlap into the grey the contract describes.
pub const CHANNEL_ALPHA: f32 = 0.55;

/// The colour of a composition guide drawn over the photograph (the thirds overlay, the crop
/// overlay's own thirds). It is [`BORDER`]'s white at the opacity a line needs to stay readable
/// over an image rather than over a panel, which is why it is its own token and not a reuse of a
/// chrome colour.
pub const GUIDE: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.30,
};

// -- Type sizes -----------------------------------------------------------------------------

/// Control text.
pub const SIZE_CONTROL: f32 = 12.0;
/// Semibold module and section titles.
pub const SIZE_TITLE: f32 = 13.0;
/// Captions.
pub const SIZE_CAPTION: f32 = 11.0;
/// Capitalised section labels.
pub const SIZE_SECTION_LABEL: f32 = 10.5;
/// A caption's line: a whole number of points, so the rows stacked under a block of captions (the
/// histogram's readout) start on a whole point and their 1 px rules stay sharp.
pub const CAPTION_LINE_HEIGHT: f32 = 14.0;

// -- Grid ------------------------------------------------------------------------------------

/// The 8 pt spacing grid unit.
pub const SPACING: f32 = 8.0;
/// The corner radius used across controls, chips and cards.
pub const RADIUS: f32 = 6.0;
/// The 1 px border width used across dividers and outlines.
pub const BORDER_WIDTH: f32 = 1.0;
/// A fixed width for a right-aligned value field (see [`crate::value_text`]'s tabular-numeral
/// note). A value with a word unit, such as `+0.35 EV`, may run past the box's left edge rather
/// than wrap, so the right edge, where the units digit sits, never moves.
pub const VALUE_WIDTH: f32 = 48.0;
/// A square icon button: mode strip entries, title-bar actions, context toggles.
pub const ICON_BUTTON_SIZE: f32 = 28.0;
/// The icon inside an [`ICON_BUTTON_SIZE`] button.
pub const ICON_SIZE: f32 = 16.0;
/// Every vector icon's stroke, in points whatever the icon's size, so a 14 pt reset and a 16 pt
/// mode icon draw the same line.
pub const ICON_STROKE_WIDTH: f32 = 1.2;
/// The histogram plot's height at the top of the tools panel, from the Develop workspace layout.
pub const HISTOGRAM_HEIGHT: f32 = 96.0;

// -- Module panel density ---------------------------------------------------------------------
//
// The tools panel's rows, from the Density table of the Develop workspace design's Module panels
// section, as measured on its module references. Every section, group and control row takes its
// size from here.

/// A module band: the section header row on the Bar surface, the same height expanded or collapsed.
pub const MODULE_HEADER_HEIGHT: f32 = 32.0;
/// The band's inset before its disclosure.
pub const MODULE_HEADER_PADDING_LEFT: f32 = 10.0;
/// The band's inset after its hint or reset.
pub const MODULE_HEADER_PADDING_RIGHT: f32 = 6.0;
/// The accent dot that marks a non-neutral module, or a tab whose group is Custom.
pub const DOT_SIZE: f32 = 6.0;
/// Between the band's disclosure, title and dot.
pub const MODULE_HEADER_SPACING: f32 = 7.0;
/// The expanded section's body: 4 pt top, 12 pt sides, 10 pt bottom.
pub const SECTION_PADDING: Padding = Padding {
    top: 4.0,
    right: 12.0,
    bottom: 10.0,
    left: 12.0,
};
/// Between consecutive rows in a section body: sliders are separated only by this gap.
pub const ROW_SPACING: f32 = 2.0;
/// A sub-group header row.
pub const GROUP_HEADER_HEIGHT: f32 = 24.0;
/// The margin above a sub-group header, on top of [`ROW_SPACING`].
pub const GROUP_MARGIN: f32 = 4.0;
/// Between a group header's disclosure and its label.
pub const GROUP_HEADER_SPACING: f32 = 7.0;
/// Between a group header's label, rule and caption.
pub const GROUP_RULE_SPACING: f32 = 7.0;
/// The rule's extra lead after the label, which has no right side bearing to give it air.
pub const GROUP_RULE_LEAD: f32 = 1.0;
/// Between a group header's caption and its reset.
pub const GROUP_RESET_SPACING: f32 = 6.0;
/// The disclosure chevron in a band or a group header.
pub const DISCLOSURE_SIZE: f32 = 11.0;
/// The reset icon in a band or a group header.
pub const HEADER_ICON_SIZE: f32 = 14.0;
/// The reset button's square hit box in a band or a group header.
pub const HEADER_BUTTON_SIZE: f32 = 20.0;
/// A slider's label line: the label and, right-aligned, its value.
pub const SLIDER_LABEL_HEIGHT: f32 = 14.0;
/// Between a slider's label line and its rail line.
pub const SLIDER_GAP: f32 = 2.0;
/// A slider's rail line: the rail, the zero tick and the handle.
pub const SLIDER_RAIL_HEIGHT: f32 = 12.0;
/// A whole slider row.
pub const SLIDER_ROW_HEIGHT: f32 = SLIDER_LABEL_HEIGHT + SLIDER_GAP + SLIDER_RAIL_HEIGHT;
/// A plain rail's thickness.
pub const RAIL_WIDTH: f32 = 2.0;
/// A colour rail's thickness, a little heavier so its colours read.
pub const DECORATED_RAIL_WIDTH: f32 = 3.0;
/// The handle's radius including its outline ring: a 12 pt handle inside a 1 pt ring.
pub const THUMB_RADIUS: f32 = 7.0;
/// The ring around the handle.
pub const THUMB_OUTLINE_WIDTH: f32 = 1.0;
/// The zero tick's height across the rail.
pub const ZERO_TICK_HEIGHT: f32 = 6.0;
/// The zero tick's width.
pub const ZERO_TICK_WIDTH: f32 = 1.0;
/// The accent mark at a rail's end when the value lies beyond the soft range on that side.
pub const OVER_RANGE_MARK: iced::Size = iced::Size {
    width: 2.0,
    height: 9.0,
};
/// A labelled button in a section: a picker or an action.
pub const BUTTON_HEIGHT: f32 = 22.0;
/// A labelled button's inset before its icon or label.
pub const BUTTON_PADDING_LEFT: f32 = 7.0;
/// A labelled button's inset after its label or key hint.
pub const BUTTON_PADDING_RIGHT: f32 = 7.0;
/// Between a labelled button's icon and label.
pub const BUTTON_ICON_SPACING: f32 = 5.0;
/// Between a labelled button's label and its key hint.
pub const BUTTON_HINT_SPACING: f32 = 10.0;
/// A labelled button's icon.
pub const BUTTON_ICON_SIZE: f32 = 14.0;
/// The margin above a button row, on top of [`ROW_SPACING`].
pub const BUTTON_ROW_MARGIN: f32 = 4.0;
/// The margin under a button row, on top of [`ROW_SPACING`], before the group header that follows.
pub const BUTTON_ROW_BOTTOM: f32 = 2.0;
/// Between buttons in one row.
pub const BUTTON_ROW_SPACING: f32 = 6.0;
/// A segmented tab row that stands in for a module's group headers.
pub const TAB_ROW_HEIGHT: f32 = 24.0;
/// The margin above and below a tab row, on top of [`ROW_SPACING`].
pub const TAB_ROW_MARGIN: f32 = 4.0;
/// The inset between a tab row's track and its selected pill.
pub const TAB_INSET: f32 = 2.0;
/// A tab row's track.
pub const TAB_TRACK: Color = Color::from_rgb8(0x28, 0x28, 0x2c);
/// A tab row's selected pill.
pub const TAB_SELECTED: Color = Color::from_rgb8(0x3b, 0x3b, 0x41);
/// The tools panel's scrollbar. It overlays the section padding's right edge, so it is thin
/// enough to clear a band's reset and a slider's value.
pub const PANEL_SCROLLBAR_WIDTH: f32 = 4.0;
/// The scrollbar's inset from the panel's right edge.
pub const PANEL_SCROLLBAR_MARGIN: f32 = 1.0;
/// A history, version or recipe row.
pub const LIST_ROW_HEIGHT: f32 = 26.0;

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

/// The tools panel's thin overlay scrollbar.
pub fn panel_scrollbar() -> iced::widget::scrollable::Direction {
    iced::widget::scrollable::Direction::Vertical(
        iced::widget::scrollable::Scrollbar::new()
            .width(PANEL_SCROLLBAR_WIDTH)
            .scroller_width(PANEL_SCROLLBAR_WIDTH)
            .margin(PANEL_SCROLLBAR_MARGIN),
    )
}

/// A button with no surface in any state, for a disclosure that is read as text.
pub fn button_bare(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: text_color_for(status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A module band's surface: the Bar colour, flat, in every state. The band is a disclosure, so
/// it keeps one colour rather than flashing a hover fill across the panel.
pub fn button_band(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(BAR)),
        text_color: text_color_for(status),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// A labelled button in a section (a picker or an action): the Control surface, borderless.
pub fn button_control(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Color {
            a: 0.18,
            ..TEXT_PRIMARY
        },
        button::Status::Active | button::Status::Disabled => CONTROL,
    };
    button::Style {
        background: Some(Background::Color(background)),
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

/// A group header's hairline rule.
pub fn rule_surface(_theme: &Theme) -> container::Style {
    surface(RULE)
}

/// The 1 px border above a module band.
pub fn band_border_surface(_theme: &Theme) -> container::Style {
    surface(BAND_BORDER)
}

/// The slider's handle. The rail, its fill and its zero tick are drawn under it by the slider row
/// itself (see [`crate::geometry::rail_geometry`]), so Iced's own rail is transparent. The handle
/// turns [`ACCENT`] only while dragging.
pub fn slider_style(dragging: bool) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_theme, status| {
        let active = dragging || matches!(status, slider::Status::Dragged);

        slider::Style {
            rail: slider::Rail {
                backgrounds: (
                    Background::Color(Color::TRANSPARENT),
                    Background::Color(Color::TRANSPARENT),
                ),
                width: RAIL_WIDTH,
                border: Border::default(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle {
                    radius: THUMB_RADIUS,
                },
                background: Background::Color(if active { ACCENT } else { THUMB }),
                border_color: THUMB_OUTLINE,
                border_width: THUMB_OUTLINE_WIDTH,
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
    fn the_guide_token_is_white_at_thirty_percent() {
        assert_eq!((GUIDE.r, GUIDE.g, GUIDE.b), (BORDER.r, BORDER.g, BORDER.b));
        assert!((GUIDE.a - 0.30).abs() < f32::EPSILON);
    }

    /// The both-endpoint overlay colour is composed from the two clipping tokens, never written as
    /// its own literal: a change to either token carries into it.
    #[test]
    fn the_both_endpoint_colour_is_the_two_clipping_tokens_combined() {
        assert_eq!(CLIPPING_BOTH.r, CLIPPING_HIGHLIGHT.r);
        assert_eq!(CLIPPING_BOTH.g, CLIPPING_HIGHLIGHT.g);
        assert_eq!(CLIPPING_BOTH.b, CLIPPING_SHADOW.b);
        assert_eq!(CLIPPING_BOTH.a, 1.0);
        // It is visibly neither of the two it is made from, which is the point of a third class.
        assert_ne!(CLIPPING_BOTH, CLIPPING_HIGHLIGHT);
        assert_ne!(CLIPPING_BOTH, CLIPPING_SHADOW);
    }

    #[test]
    fn channel_fills_are_distinct_and_translucent() {
        for (a, b) in [
            (CHANNEL_RED, CHANNEL_GREEN),
            (CHANNEL_GREEN, CHANNEL_BLUE),
            (CHANNEL_RED, CHANNEL_BLUE),
        ] {
            assert_ne!(a, b);
        }
        assert!((0.0..1.0).contains(&CHANNEL_ALPHA), "the fills overlap");
        assert_eq!(HISTOGRAM_HEIGHT, 96.0);
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
        let style = slider_style(true)(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(ACCENT));
    }

    #[test]
    fn resting_handle_is_not_accent_and_iced_draws_no_rail() {
        let style = slider_style(false)(&theme(), slider::Status::Active);
        assert_eq!(style.handle.background, Background::Color(THUMB));
        let clear = Background::Color(Color::TRANSPARENT);
        assert_eq!(style.rail.backgrounds, (clear, clear));
    }

    /// The Density table of the Module panels design, pinned: a change here moves every section.
    #[test]
    fn module_panel_density_matches_the_design() {
        assert_eq!(MODULE_HEADER_HEIGHT, 32.0);
        assert_eq!(
            (
                SECTION_PADDING.top,
                SECTION_PADDING.right,
                SECTION_PADDING.bottom,
                SECTION_PADDING.left
            ),
            (4.0, 12.0, 10.0, 12.0)
        );
        assert_eq!((GROUP_HEADER_HEIGHT, GROUP_MARGIN), (24.0, 4.0));
        assert_eq!(
            (SLIDER_LABEL_HEIGHT, SLIDER_GAP, SLIDER_RAIL_HEIGHT),
            (14.0, 2.0, 12.0)
        );
        assert_eq!(SLIDER_ROW_HEIGHT, 28.0);
        assert_eq!(SLIDER_ROW_HEIGHT + ROW_SPACING, 30.0, "the slider pitch");
        assert_eq!(VALUE_WIDTH, 48.0);
        assert_eq!(RAIL_WIDTH, 2.0);
        assert_eq!(ZERO_TICK_HEIGHT, 6.0);
        assert_eq!(
            2.0 * (THUMB_RADIUS - THUMB_OUTLINE_WIDTH),
            12.0,
            "a 12 pt thumb"
        );
        assert_eq!((BUTTON_HEIGHT, BUTTON_ROW_MARGIN), (22.0, 4.0));
        assert_eq!(TAB_ROW_HEIGHT, 24.0);
        assert_eq!(LIST_ROW_HEIGHT, 26.0);
    }

    #[test]
    fn rail_tokens_match_the_module_panel_references() {
        assert_eq!(RAIL, Color::from_rgb8(0x3a, 0x3a, 0x40));
        assert_eq!(RAIL_FILL, Color::from_rgb8(0xa3, 0xa3, 0xaa));
        assert_eq!(THUMB, Color::from_rgb8(0xec, 0xec, 0xee));
        assert_eq!(BAND_BORDER, Color::from_rgb8(0x2f, 0x2f, 0x32));
        assert_eq!(RULE, Color::from_rgb8(0x31, 0x31, 0x34));
        assert_eq!(ZERO_TICK, Color::from_rgb8(0x5a, 0x5a, 0x62));
        assert_eq!(THUMB_OUTLINE, Color::from_rgb8(0x11, 0x11, 0x13));
        assert_eq!(TEXT_LABEL, Color::from_rgb8(0xc9, 0xc9, 0xce));
    }
}
