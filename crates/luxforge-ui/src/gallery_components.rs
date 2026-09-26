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
    // The icons in two columns, so the whole board fits one gallery page beside a curve.
    let half = Icon::NAMED.len().div_ceil(2);
    let mut columns = iced::widget::row![].spacing(24.0);
    for chunk in Icon::NAMED.chunks(half) {
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
