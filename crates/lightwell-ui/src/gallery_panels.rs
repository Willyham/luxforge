//! Gallery states for the tools panel's module sections, at the reference panel width, so the
//! gallery smoke shows the band, the group rule, the slider rows, the button row and the tab row
//! exactly as a generated section composes them.

use crate::{
    ButtonSize, ButtonTone, ChipModel, ColorSwatchModel, Icon, IconButtonModel,
    LabelledButtonModel, NumberFieldModel, RailDecoration, RowPlacement, SectionHeaderModel,
    SliderModel, StepperModel, SubGroupHeaderModel, Tab, TabRowModel, ToggleModel, ValueEdit,
    boxed_input, button_row, channel_row, chip, chip_row, color_swatch, equal_button_row,
    icon_button_row, labelled_button, module_section, number_field, readout_card, row_icon_button,
    section_body, section_header, slider, stepper, sub_group_header, sub_group_header_with_actions,
    tab_row, theme, toggle,
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
        status: None,
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

/// The later module-panel states, in gallery order: the icon-button row, the crop section drafting
/// (in its band, for the Draft status) and idle, and the field rows. All but the drafting section
/// are section bodies, so the last gallery page keeps every state on screen.
pub(crate) fn gallery_panel_rows() -> Vec<Element<'static, ()>> {
    let mut states = Vec::new();

    // -- Transforms: actions that each name an icon, as one row of equal icon buttons.
    let cell = |icon, tooltip: &str| {
        row_icon_button(
            &IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: true,
                selected: false,
            },
            Some(()),
        )
    };
    let transforms = section_body(vec![
        group_plain("Exact transforms"),
        icon_button_row(
            vec![
                cell(Icon::RotateLeft, "Rotate left"),
                cell(Icon::RotateRight, "Rotate right"),
                cell(Icon::Mirror, "Mirror horizontal"),
                cell(Icon::Flip, "Flip vertical"),
            ],
            RowPlacement {
                after_header: true,
                followed: false,
            },
        ),
    ]);
    states.push(panel(transforms));

    // -- Crop and straighten, drafting: Draft in the band, the Ratio group's lock (selected) and
    // -- swap, the ratio chips, the angle stepper, the guide switch, the readout and Cancel/Apply.
    let ratio = |label: &str| SubGroupHeaderModel {
        label: label.into(),
        state: None,
        state_accent: false,
        expanded: None,
        reset: false,
        enabled: true,
    };
    let action = |icon, tooltip: &str, selected| {
        (
            IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: true,
                selected,
            },
            Some(()),
        )
    };
    let chips = [
        "Free",
        "Original",
        "1:1",
        "3:2",
        "4:3",
        "16:9",
        "Custom\u{2026}",
    ]
    .into_iter()
    .map(|label| {
        chip(
            &ChipModel {
                label: label.into(),
                trailing: None,
                selected: label == "Original",
                enabled: true,
            },
            Some(()),
            None,
        )
    })
    .collect();
    let wide = |label: &str, hint: &str, tone| {
        labelled_button(
            &LabelledButtonModel {
                label: label.into(),
                icon: None,
                key_hint: Some(hint.into()),
                tone,
                size: ButtonSize::Regular,
                fill: true,
                enabled: true,
            },
            Some(()),
        )
    };
    let drafting = module_section(
        &SectionHeaderModel {
            status: Some("Draft".into()),
            ..band("Crop and straighten", true, None, None)
        },
        (),
        (),
        Some(vec![
            sub_group_header_with_actions(
                &ratio("Ratio"),
                None,
                (),
                vec![
                    action(Icon::Lock, "Unlock ratio", true),
                    action(Icon::Swap, "Swap", false),
                ],
            ),
            chip_row(
                chips,
                RowPlacement {
                    after_header: true,
                    followed: true,
                },
            ),
            sub_group_header(&ratio("Angle"), None, ()),
            stepper(
                &StepperModel {
                    field: NumberFieldModel {
                        id: None,
                        label: String::new(),
                        display: "2.40".into(),
                        edit: ValueEdit::Display,
                        unit: Some("\u{b0}".into()),
                        enabled: true,
                    },
                    decrement_enabled: true,
                    increment_enabled: true,
                    decrement_tooltip: "\u{2212}0.5\u{b0}".into(),
                    increment_tooltip: "+0.5\u{b0}".into(),
                },
                (),
                (),
                (),
                |_| (),
                (),
                (),
            ),
            toggle(
                &ToggleModel {
                    label: "Straighten guide".into(),
                    on: false,
                    enabled: true,
                },
                |_| (),
            ),
            readout_card(
                &[
                    ("Input stage".into(), "3389 \u{d7} 4236".into()),
                    (
                        "Rectangle".into(),
                        "258, 326 \u{b7} 2872 \u{d7} 3590".into(),
                    ),
                    ("Output".into(), "2872 \u{d7} 3590".into()),
                    ("Commits".into(), "edit.crop \u{b7} layer 2".into()),
                ],
                RowPlacement {
                    after_header: false,
                    followed: true,
                },
            ),
            equal_button_row(
                vec![
                    wide("Cancel", "esc", ButtonTone::Control),
                    wide("Apply", "return", ButtonTone::Primary),
                ],
                RowPlacement::default(),
            ),
        ]),
    );
    states.push(panel(drafting));

    // -- Crop and straighten, idle: one regular button with its icon and letter.
    let idle = section_body(vec![button_row(
        vec![labelled_button(
            &LabelledButtonModel {
                label: "Crop".into(),
                icon: Some(Icon::Crop),
                key_hint: Some("R".into()),
                tone: ButtonTone::Control,
                size: ButtonSize::Regular,
                fill: false,
                enabled: true,
            },
            Some(()),
        )],
        RowPlacement::default(),
    )]);
    states.push(panel(idle));

    // -- Field rows, as the pixel proof's request inputs are drawn: a boxed value with a word unit
    // -- outside, a colour's swatch and channel boxes, and a compact picker row with an action.
    let field = |label: &str, display: &str| {
        number_field(
            &NumberFieldModel {
                id: None,
                label: label.into(),
                display: display.into(),
                edit: ValueEdit::Display,
                unit: Some("px".into()),
                enabled: true,
            },
            (),
            |_| (),
            (),
            (),
        )
    };
    let channel = |value: &str| -> Element<'static, ()> {
        boxed_input(
            "",
            value,
            theme::CHANNEL_FIELD_WIDTH,
            false,
            true,
            |_| (),
            (),
        )
        .into()
    };
    let fields = section_body(vec![
        group_plain("Pixel proof"),
        field("X", "1204"),
        field("Y", "877"),
        channel_row(
            "RGB".into(),
            true,
            color_swatch(
                &ColorSwatchModel {
                    rgb: [220, 40, 40],
                    enabled: false,
                    open: false,
                },
                (),
            ),
            vec![channel("220"), channel("40"), channel("40")],
        ),
        button_row(
            vec![
                picker(ButtonTone::Control, true),
                labelled_button(
                    &LabelledButtonModel {
                        label: "Apply pixel".into(),
                        icon: None,
                        key_hint: None,
                        tone: ButtonTone::Control,
                        size: ButtonSize::Compact,
                        fill: false,
                        enabled: true,
                    },
                    Some(()),
                ),
            ],
            RowPlacement::default(),
        ),
    ]);
    states.push(panel(fields));
    states
}

/// A group of request inputs: its label and rule, with no state caption or reset.
fn group_plain(label: &str) -> Element<'static, ()> {
    sub_group_header(
        &SubGroupHeaderModel {
            label: label.into(),
            state: None,
            state_accent: false,
            expanded: Some(true),
            reset: false,
            enabled: true,
        },
        Some(()),
        (),
    )
}
