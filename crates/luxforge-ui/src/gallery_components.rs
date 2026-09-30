//! States added with the module control vocabulary.

use crate::*;
use iced::Element;

pub(crate) fn gallery_components() -> Vec<Element<'static, ()>> {
    let mut states = Vec::new();
    let rail = SliderModel {
        id: None,
        label: "Hue".into(),
        min: -180.0,
        max: 180.0,
        soft_min: -90.0,
        soft_max: 90.0,
        value: 0.0,
        step: 1.0,
        shift_step: 10.0,
        fine_step: 0.1,
        zero: Some(0.0),
        rail: RailDecoration::Colors(vec![
            iced::Color::from_rgb8(255, 0, 0),
            iced::Color::from_rgb8(255, 255, 0),
            iced::Color::from_rgb8(0, 255, 0),
            iced::Color::from_rgb8(0, 255, 255),
            iced::Color::from_rgb8(0, 0, 255),
            iced::Color::from_rgb8(255, 0, 255),
        ]),
        over_range: None,
        unit: Some("°".into()),
        display: "0".into(),
        edit: ValueEdit::Display,
        dragging: false,
        enabled: true,
    };
    states.push(slider(&rail, |_| (), (), (), |_| (), (), ()));
    states.push(slider(
        &SliderModel {
            value: -120.0,
            display: "-120".into(),
            over_range: Some(geometry::Side::Low),
            ..rail.clone()
        },
        |_| (),
        (),
        (),
        |_| (),
        (),
        (),
    ));
    states.push(slider(
        &SliderModel {
            value: 120.0,
            display: "+120".into(),
            over_range: Some(geometry::Side::High),
            ..rail
        },
        |_| (),
        (),
        (),
        |_| (),
        (),
        (),
    ));
    let field = |label: &str, edit: ValueEdit, enabled: bool| NumberFieldModel {
        id: None,
        label: label.into(),
        display: "12".into(),
        edit,
        unit: Some("°".into()),
        enabled,
    };
    for edit in [
        ValueEdit::Display,
        ValueEdit::Editing {
            text: "12".into(),
            invalid: None,
        },
        ValueEdit::Editing {
            text: "999".into(),
            invalid: Some("Range is -90 to 90".into()),
        },
    ] {
        states.push(number_field(
            &field("Angle", edit, true),
            (),
            |_| (),
            (),
            (),
        ));
    }
    states.push(number_field(
        &field("Angle", ValueEdit::Display, false),
        (),
        |_| (),
        (),
        (),
    ));
    states.push(stepper(
        &StepperModel {
            field: field("Angle", ValueEdit::Display, true),
            decrement_enabled: true,
            increment_enabled: true,
            decrement_tooltip: "Decrease".into(),
            increment_tooltip: "Increase".into(),
            rail: None,
        },
        (),
        (),
        (),
        |_| (),
        (),
        (),
        None,
    ));
    states.push(stepper(
        &StepperModel {
            field: field("Angle", ValueEdit::Display, false),
            decrement_enabled: false,
            increment_enabled: false,
            decrement_tooltip: "Decrease".into(),
            increment_tooltip: "Increase".into(),
            rail: None,
        },
        (),
        (),
        (),
        |_| (),
        (),
        (),
        None,
    ));
    // The angle as crop-and-straighten.png draws it while drafting: minus, the rail with its
    // accent handle in its halo, plus, and the value with its symbol unit in the box.
    states.push(stepper(
        &StepperModel {
            field: NumberFieldModel {
                label: String::new(),
                display: "2.4".into(),
                ..field("Angle", ValueEdit::Display, true)
            },
            decrement_enabled: true,
            increment_enabled: true,
            decrement_tooltip: "\u{2212}0.5\u{b0}".into(),
            increment_tooltip: "+0.5\u{b0}".into(),
            rail: Some(StepperRail {
                soft_min: -45.0,
                soft_max: 45.0,
                value: 2.4,
                step: 0.05,
                zero: Some(0.0),
                dragging: true,
            }),
        },
        (),
        (),
        (),
        |_| (),
        (),
        (),
        Some(StepperRailMessages {
            on_change: Box::new(|_| ()),
            on_release: (),
        }),
    ));
    for (on, enabled) in [(false, true), (true, true), (true, false)] {
        states.push(toggle(
            &ToggleModel {
                label: "Straighten".into(),
                on,
                enabled,
            },
            |_| (),
        ));
    }
    for (selected, enabled) in [(0, true), (2, true), (2, false)] {
        states.push(menu_choice(
            &MenuChoiceModel {
                label: "Mode".into(),
                options: vec!["RGB".into(), "HSL".into(), "Lab".into()],
                selected,
                enabled,
            },
            |_| (),
        ));
    }
    for (rgb, open, enabled) in [
        ([204, 80, 40], false, true),
        ([204, 80, 40], true, true),
        ([80, 80, 80], false, false),
    ] {
        states.push(color_swatch(&ColorSwatchModel { rgb, open, enabled }, ()));
    }
    let picker = ColorPickerModel {
        hue: 0.08,
        saturation: 0.72,
        value: 0.85,
        rgb: [217, 106, 61],
        channels: [ValueEdit::Display, ValueEdit::Display, ValueEdit::Display],
        hex: ValueEdit::Display,
        dragging: false,
        enabled: true,
        version: 0,
    };
    states.push(color_picker(&picker, |_| ()));
    states.push(color_picker(
        &ColorPickerModel {
            dragging: true,
            version: 1,
            ..picker.clone()
        },
        |_| (),
    ));
    states.push(color_picker(
        &ColorPickerModel {
            enabled: false,
            version: 2,
            ..picker
        },
        |_| (),
    ));
    let curve = CurveEditorModel {
        points: vec![[0.0, 0.0], [0.5, 0.62], [1.0, 1.0]],
        sampled: vec![
            [0.0, 0.0],
            [0.25, 0.3],
            [0.5, 0.62],
            [0.75, 0.82],
            [1.0, 1.0],
        ],
        point_rows: vec![
            CurvePointRow {
                display: ["0".into(), "0".into()],
                edit: [ValueEdit::Display, ValueEdit::Display],
            },
            CurvePointRow {
                display: ["0.5".into(), "0.62".into()],
                edit: [ValueEdit::Display, ValueEdit::Display],
            },
            CurvePointRow {
                display: ["1".into(), "1".into()],
                edit: [ValueEdit::Display, ValueEdit::Display],
            },
        ],
        selected: None,
        background: None,
        identity: true,
        channels: vec!["RGB".into()],
        selected_channel: 0,
        dragging: false,
        enabled: true,
        version: 0,
    };
    states.push(curve_editor(&curve, |_| ()));
    states.push(curve_editor(
        &CurveEditorModel {
            selected: Some(1),
            dragging: true,
            version: 1,
            ..curve.clone()
        },
        |_| (),
    ));
    let mut heights = [0.0; 256];
    for (i, height) in heights.iter_mut().enumerate() {
        *height = (1.0 - ((i as f32 - 128.0) / 128.0).abs()).max(0.0);
    }
    states.push(curve_editor(
        &CurveEditorModel {
            selected: Some(1),
            background: Some(heights),
            channels: vec!["RGB".into(), "Red".into()],
            selected_channel: 1,
            version: 2,
            ..curve.clone()
        },
        |_| (),
    ));
    states.push(curve_editor(
        &CurveEditorModel {
            enabled: false,
            version: 3,
            ..curve
        },
        |_| (),
    ));
    // The icons in three columns, so the whole board fits one gallery page beside a curve.
    let third = Icon::NAMED.len().div_ceil(3);
    let mut columns = iced::widget::row![].spacing(24.0);
    for chunk in Icon::NAMED.chunks(third) {
        let mut icons = iced::widget::column![].spacing(6.0);
        for &(name, symbol) in chunk {
            icons = icons.push(
                iced::widget::row![
                    iced::widget::text(name)
                        .size(theme::SIZE_CAPTION)
                        .width(120.0),
                    icon::<()>(symbol, 12.0, theme::TEXT_PRIMARY),
                    icon::<()>(symbol, 16.0, theme::TEXT_PRIMARY),
                ]
                .spacing(12.0)
                .align_y(iced::Alignment::Center),
            );
        }
        columns = columns.push(icons);
    }
    states.push(columns.into());
    states
}

/// The labelled widgets: a tab row per selected tab, labelled buttons in each tone, and one-line
/// truncation in section headers and history rows, framed narrow enough that the long ones end in
/// their ellipsis.
pub(crate) fn gallery_labels() -> Vec<Element<'static, ()>> {
    // The tab row, one per selected tab, the dot on each Custom tab.
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
    // Labelled buttons: resting with its icon and key hint, selected (its mode is active),
    // disabled, and a primary action.
    let button = |label: &str, icon, key_hint: Option<&str>, tone, enabled| {
        labelled_button(
            &LabelledButtonModel {
                label: label.into(),
                icon,
                key_hint: key_hint.map(Into::into),
                tone,
                size: ButtonSize::Compact,
                fill: false,
                enabled,
            },
            Some(()),
        )
    };
    let picker = |tone, enabled| {
        button(
            "Neutral picker",
            Some(Icon::Picker),
            Some("W"),
            tone,
            enabled,
        )
    };
    // Section headers whose hint or unavailable reason does not fit end in an ellipsis; on a
    // scoped band the hint gives way to the scope chip, and an expanded one keeps the chip before
    // its reset.
    let band = |title: &str, expanded, hint: Option<&str>, unavailable: Option<&str>, scope| {
        section_header(
            &SectionHeaderModel {
                title: title.into(),
                expanded,
                active: expanded || hint.is_some(),
                hint: hint.map(Into::into),
                unavailable: unavailable.map(Into::into),
                reset: true,
                status: None,
                scope,
                enabled: true,
            },
            (),
            (),
        )
    };
    let mixer_hint = Some("Hue, saturation and luminance by range");
    // History rows: a long label ends in an ellipsis before the actor, which keeps its full
    // width; the tag still follows a truncated label.
    let history = |leading: &str, label: &str, actor: Option<&str>, tag: Option<&str>| {
        list_row(
            &ListRowModel {
                marker: if tag.is_some() {
                    Marker::Plain
                } else {
                    Marker::Current
                },
                leading: leading.into(),
                label: label.into(),
                trailing: actor.map(Into::into),
                dimmed: tag.is_some(),
                tag: tag.map(Into::into),
                enabled: true,
            },
            Some(()),
            Some(()),
        )
    };
    vec![
        iced::widget::Column::with_children((0..3).map(tabs))
            .spacing(theme::ROW_SPACING)
            .into(),
        iced::widget::column![
            button_row(
                vec![
                    picker(ButtonTone::Control, true),
                    picker(ButtonTone::Selected, true),
                ],
                RowPlacement::default(),
            ),
            button_row(
                vec![
                    picker(ButtonTone::Control, false),
                    button("Apply", None, None, ButtonTone::Primary, true),
                ],
                RowPlacement::default(),
            ),
        ]
        .into(),
        gallery::narrow(
            iced::widget::column![
                band("Colour mixer", false, mixer_hint, None, None),
                band(
                    "Vignette",
                    false,
                    Some("Darken or lighten the corners after the crop"),
                    None,
                    None
                ),
                band(
                    "Lens profile",
                    false,
                    None,
                    Some("Unavailable \u{b7} disabled by --disable-module lens-profile"),
                    None
                ),
                band("Colour mixer", false, mixer_hint, None, Some("Face".into())),
                band("Presence", true, None, None, Some("Face".into())),
            ]
            .into(),
        ),
        gallery::narrow(
            iced::widget::column![
                history(
                    "12",
                    "Blue saturation \u{2212}40",
                    Some("agent \u{b7} lf-assist"),
                    None
                ),
                history("11", "Reset White balance", Some("you"), None),
                history("10", "Hue, saturation and luminance", None, Some("branch")),
            ]
            .spacing(theme::LIST_ROW_SPACING)
            .into(),
        ),
    ]
}

/// The range: a luminance band on its black-to-white rail with both shoulders open, the same band
/// with its high edge dragged, and a disabled band on the plain rail with no shoulders declared.
pub(crate) fn gallery_ranges() -> Vec<Element<'static, ()>> {
    let band = RangeSliderModel {
        label: "Range".into(),
        readout: "62 – 88".into(),
        min: 0.0,
        max: 100.0,
        values: RangeValues {
            low: 62.0,
            high: 88.0,
            low_feather: Some(10.0),
            high_feather: Some(6.0),
        },
        step: 1.0,
        rail: RailDecoration::Colors(vec![
            iced::Color::from_rgb8(0, 0, 0),
            iced::Color::from_rgb8(255, 255, 255),
        ]),
        dragging: None,
        enabled: true,
    };
    vec![
        range_slider(&band, |_, _| (), |_| (), |_| ()),
        range_slider(
            &RangeSliderModel {
                readout: "62 – 91".into(),
                values: RangeValues {
                    high: 91.0,
                    ..band.values
                },
                dragging: Some(RangeGrip::High),
                ..band.clone()
            },
            |_, _| (),
            |_| (),
            |_| (),
        ),
        range_slider(
            &RangeSliderModel {
                readout: "20 – 45".into(),
                values: RangeValues {
                    low: 20.0,
                    high: 45.0,
                    low_feather: None,
                    high_feather: None,
                },
                rail: RailDecoration::Plain,
                enabled: false,
                ..band
            },
            |_, _| (),
            |_| (),
            |_| (),
        ),
    ]
}
