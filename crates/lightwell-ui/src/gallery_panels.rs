//! Gallery states for the tools panel's module sections, at the reference panel width, so the
//! gallery smoke shows the band, the group rule, the slider rows, the button row and the tab row
//! exactly as a generated section composes them.

use crate::{
    ButtonSize, ButtonTone, Icon, LabelledButtonModel, RailDecoration, RowPlacement,
    SectionHeaderModel, SliderModel, SubGroupHeaderModel, Tab, TabRowModel, ValueEdit, button_row,
    labelled_button, module_section, section_header, slider, sub_group_header, tab_row, theme,
};
use iced::widget::{Column, Row, column, container};
use iced::{Element, Length};

/// The width of the module references' panels. This frames the examples only; the desktop's own
/// tools panel width is the app's layout constant.
const REFERENCE_PANEL_WIDTH: f32 = 300.0;

fn panel<'a>(content: Element<'a, ()>) -> Element<'a, ()> {
    container(content)
        .width(Length::Fixed(REFERENCE_PANEL_WIDTH))
        .style(theme::panel_surface)
        .into()
}

fn band(
    title: &str,
    expanded: bool,
    hint: Option<&str>,
    unavailable: Option<&str>,
) -> SectionHeaderModel {
    SectionHeaderModel {
        title: title.into(),
        expanded,
        active: expanded || hint.is_some(),
        hint: hint.map(Into::into),
        unavailable: unavailable.map(Into::into),
        reset: true,
        enabled: true,
    }
}

fn group(label: &str, custom: bool, expanded: bool) -> Element<'static, ()> {
    sub_group_header(
        &SubGroupHeaderModel {
            label: label.into(),
            state: Some(if custom { "Custom" } else { "Original" }.into()),
            state_accent: custom,
            expanded: Some(expanded),
            reset: true,
            enabled: true,
        },
        Some(()),
        (),
    )
}

fn row_slider(
    label: &str,
    value: f64,
    display: &str,
    unit: Option<&str>,
    rail: RailDecoration,
) -> Element<'static, ()> {
    let (min, max, zero) = (-100.0, 100.0, Some(0.0));
    slider(
        &SliderModel {
            id: None,
            label: label.into(),
            min,
            max,
            soft_min: min,
            soft_max: max,
            value,
            step: 1.0,
            shift_step: 10.0,
            fine_step: 0.1,
            zero,
            rail,
            over_range: None,
            unit: unit.map(Into::into),
            display: display.into(),
            edit: ValueEdit::Display,
            dragging: false,
            enabled: true,
        },
        |_| (),
        (),
        (),
        |_| (),
        (),
        (),
    )
}

fn temperature() -> RailDecoration {
    RailDecoration::Colors(theme::TEMPERATURE_RAIL.to_vec())
}

fn tint() -> RailDecoration {
    RailDecoration::Colors(theme::TINT_RAIL.to_vec())
}

fn picker(tone: ButtonTone, enabled: bool) -> Element<'static, ()> {
    labelled_button(
        &LabelledButtonModel {
            label: "Neutral picker".into(),
            icon: Some(Icon::Picker),
            key_hint: Some("W".into()),
            tone,
            size: ButtonSize::Compact,
            fill: false,
            enabled,
        },
        Some(()),
    )
}

/// The module-panel states, in gallery order.
pub(crate) fn gallery_panels() -> Vec<Element<'static, ()>> {
    let plain = || RailDecoration::Plain;
    let mut states = Vec::new();

    // -- Basic expanded, as basic.png draws it: three groups with their rules and captions, the
    // -- white-balance rails and the picker's button row.
    let basic = module_section(
        &band("Basic", true, None, None),
        (),
        (),
        Some(vec![
            group("White balance", true, true),
            row_slider("Temperature", 6.0, "+6", None, temperature()),
            row_slider("Tint", -2.0, "\u{2212}2", None, tint()),
            button_row(
                vec![picker(ButtonTone::Control, true)],
                RowPlacement {
                    after_header: false,
                    followed: true,
                },
            ),
            group("Tone", true, true),
            row_slider("Exposure", 7.0, "+0.35", Some("EV"), plain()),
            row_slider("Contrast", 12.0, "+12", None, plain()),
            row_slider("Highlights", -40.0, "\u{2212}40", None, plain()),
            row_slider("Shadows", 25.0, "+25", None, plain()),
            row_slider("Whites", 8.0, "+8", None, plain()),
            row_slider("Blacks", -10.0, "\u{2212}10", None, plain()),
            group("Colour", true, true),
            row_slider("Vibrance", 15.0, "+15", None, plain()),
            row_slider("Saturation", 0.0, "0", None, plain()),
        ]),
    );
    states.push(panel(basic));

    // -- Bands: collapsed with a hint, unavailable, and an expanded section whose group is
    // -- collapsed (the header keeps its caption and drops its rule).
    let bands: Element<'static, ()> = column![
        section_header(
            &band("Presence", false, Some("Texture, clarity and dehaze"), None),
            (),
            ()
        ),
        section_header(
            &band(
                "Lens profile",
                false,
                None,
                Some("Unavailable \u{b7} no provider")
            ),
            (),
            ()
        ),
        module_section(
            &band("Vignette", true, None, None),
            (),
            (),
            Some(vec![group("Vignette", false, false)]),
        ),
    ]
    .into();
    states.push(panel(bands));

    // -- The colour mixer's tab row, one per selected tab, the dot on each Custom group.
    let tabs = |selected| {
        tab_row(
            &TabRowModel {
                tabs: vec![
                    Tab {
                        label: "Hue".into(),
                        custom: true,
                    },
                    Tab {
                        label: "Saturation".into(),
                        custom: true,
                    },
                    Tab {
                        label: "Luminance".into(),
                        custom: false,
                    },
                ],
                selected,
                reset: true,
                enabled: true,
            },
            |_| (),
            (),
        )
    };
    let tab_rows: Element<'static, ()> =
        container(Column::with_children((0..3).map(tabs)).spacing(theme::ROW_SPACING))
            .padding(theme::SECTION_PADDING)
            .into();
    states.push(panel(tab_rows));

    // -- Labelled buttons: resting with its icon and key hint, selected (its mode is active),
    // -- disabled, and a primary action.
    let buttons: Element<'static, ()> = Column::new()
        .push(button_row(
            vec![
                picker(ButtonTone::Control, true),
                picker(ButtonTone::Selected, true),
            ],
            RowPlacement::default(),
        ))
        .push(button_row(
            vec![
                picker(ButtonTone::Control, false),
                labelled_button(
                    &LabelledButtonModel {
                        label: "Apply".into(),
                        icon: None,
                        key_hint: None,
                        tone: ButtonTone::Primary,
                        size: ButtonSize::Compact,
                        fill: false,
                        enabled: true,
                    },
                    Some(()),
                ),
            ],
            RowPlacement::default(),
        ))
        .into();
    states.push(
        Row::new()
            .push(panel(
                container(buttons).padding(theme::SECTION_PADDING).into(),
            ))
            .into(),
    );
    states
}
