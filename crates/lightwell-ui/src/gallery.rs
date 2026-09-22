//! Builds one instance of every widget in every state shown on the components board.
//!
//! Kept out of the `widgets` module (and thus out of `pub use widgets::*`) so it never becomes
//! part of the public widget API; [`crate::gallery_states`] is the only path to it.

use crate::{
    BINS, ChipModel, ClipTriangleModel, HistogramChannel, HistogramModel, Icon, IconButtonModel,
    ListRowModel, Marker, ModeEntry, NoticeCardModel, RailDecoration, SectionHeaderModel,
    SegmentedModel, SliderModel, SubGroupHeaderModel, ToggleEntry, Tone, ValueEdit, caption, chip,
    clip_triangle, double_click, error_caption, floating_bar, histogram, icon_button, inline_menu,
    label, list_row, mode_strip, notice_card, section_header, section_label, segmented, slider,
    sub_group_header, theme, title, value_text,
};
use iced::Element;

/// One instance of every widget in every state the components board shows.
// A single `vec![...]` literal would bury each state's own comment inside one giant expression;
// building the list with one `push` per named, commented state keeps it readable instead.
#[allow(clippy::vec_init_then_push)]
pub fn gallery() -> Vec<Element<'static, ()>> {
    let mut states: Vec<Element<'static, ()>> = Vec::new();

    // -- Slider states: rest (bipolar negative), dragging (bipolar positive), typing, invalid,
    // -- unipolar-at-rest, disabled.
    states.push(slider(
        &SliderModel {
            id: None,
            label: "Highlights".into(),
            min: -100.0,
            max: 100.0,
            soft_min: -100.0,
            soft_max: 100.0,
            value: -40.0,
            step: 1.0,
            shift_step: 10.0,
            fine_step: 0.1,
            rail: RailDecoration::Plain,
            over_range: None,
            zero: Some(0.0),
            unit: None,
            display: "-40".into(),
            edit: ValueEdit::Display,
            dragging: false,
            enabled: true,
        },
        |_: f64| (),
        (),
        (),
        |_: String| (),
        (),
        (),
    ));
    states.push(slider(
        &SliderModel {
            id: None,
            label: "Exposure".into(),
            min: -5.0,
            max: 5.0,
            soft_min: -5.0,
            soft_max: 5.0,
            value: 0.62,
            step: 0.01,
            shift_step: 0.1,
            fine_step: 0.001,
            rail: RailDecoration::Plain,
            over_range: None,
            zero: Some(0.0),
            unit: None,
            display: "+0.62".into(),
            edit: ValueEdit::Display,
            dragging: true,
            enabled: true,
        },
        |_: f64| (),
        (),
        (),
        |_: String| (),
        (),
        (),
    ));
    states.push(slider(
        &SliderModel {
            id: None,
            label: "Contrast".into(),
            min: -100.0,
            max: 100.0,
            soft_min: -100.0,
            soft_max: 100.0,
            value: 18.0,
            step: 1.0,
            shift_step: 10.0,
            fine_step: 0.1,
            rail: RailDecoration::Plain,
            over_range: None,
            zero: Some(0.0),
            unit: None,
            display: "18".into(),
            edit: ValueEdit::Editing {
                text: "18".into(),
                invalid: None,
            },
            dragging: false,
            enabled: true,
        },
        |_: f64| (),
        (),
        (),
        |_: String| (),
        (),
        (),
    ));
    states.push(slider(
        &SliderModel {
            id: None,
            label: "Temperature".into(),
            min: -100.0,
            max: 100.0,
            soft_min: -100.0,
            soft_max: 100.0,
            value: 6.0,
            step: 1.0,
            shift_step: 10.0,
            fine_step: 0.1,
            rail: RailDecoration::Plain,
            over_range: None,
            zero: Some(0.0),
            unit: None,
            display: "140".into(),
            edit: ValueEdit::Editing {
                text: "140".into(),
                invalid: Some("Range is -100 to 100".into()),
            },
            dragging: false,
            enabled: true,
        },
        |_: f64| (),
        (),
        (),
        |_: String| (),
        (),
        (),
    ));
    states.push(slider(
        &SliderModel {
            id: None,
            label: "Saturation".into(),
            min: -100.0,
            max: 100.0,
            soft_min: -100.0,
            soft_max: 100.0,
            value: 0.0,
            step: 1.0,
            shift_step: 10.0,
            fine_step: 0.1,
            rail: RailDecoration::Plain,
            over_range: None,
            zero: None,
            unit: None,
            display: "0".into(),
            edit: ValueEdit::Display,
            dragging: false,
            enabled: false,
        },
        |_: f64| (),
        (),
        (),
        |_: String| (),
        (),
        (),
    ));

    // -- Section headers: expanded + active + reset, collapsed with a hint, unavailable.
    states.push(section_header(
        &SectionHeaderModel {
            title: "Basic".into(),
            expanded: true,
            active: true,
            hint: None,
            unavailable: None,
            reset: true,
            enabled: true,
        },
        (),
        (),
    ));
    states.push(section_header(
        &SectionHeaderModel {
            title: "Detail".into(),
            expanded: false,
            active: false,
            hint: Some("Sharpen \u{b7} Noise".into()),
            unavailable: None,
            reset: false,
            enabled: true,
        },
        (),
        (),
    ));
    states.push(section_header(
        &SectionHeaderModel {
            title: "Lens profile".into(),
            expanded: false,
            active: false,
            hint: None,
            unavailable: Some("Unavailable \u{b7} no provider".into()),
            reset: false,
            enabled: true,
        },
        (),
        (),
    ));

    // -- Sub-group header, with a reset.
    states.push(sub_group_header(
        &SubGroupHeaderModel {
            label: "Tone".into(),
            state: Some("Custom".into()),
            state_accent: true,
            expanded: Some(true),
            reset: true,
            enabled: true,
        },
        Some(()),
        (),
    ));

    // -- Icon button: plain, selected, disabled.
    states.push(icon_button(
        &IconButtonModel {
            icon: Icon::RotateRight,
            tooltip: "Rotate right".into(),
            enabled: true,
            selected: false,
        },
        Some(()),
    ));
    states.push(icon_button(
        &IconButtonModel {
            icon: Icon::Crop,
            tooltip: "Crop & straighten".into(),
            enabled: true,
            selected: true,
        },
        Some(()),
    ));
    states.push(icon_button(
        &IconButtonModel {
            icon: Icon::Reset,
            tooltip: "Reset".into(),
            enabled: false,
            selected: false,
        },
        None,
    ));

    // -- Segmented control (crop ratio presets).
    states.push(segmented(
        &SegmentedModel {
            options: vec![
                "Free".into(),
                "Original".into(),
                "1:1".into(),
                "3:2".into(),
                "4:3".into(),
                "16:9".into(),
            ],
            selected: 1,
            enabled: true,
        },
        |_: usize| (),
    ));

    // -- Chips: selected (a version) and ordinary.
    states.push(chip(
        &ChipModel {
            label: "Warm".into(),
            trailing: Some("5".into()),
            selected: true,
            enabled: true,
        },
        Some(()),
        Some(()),
    ));
    states.push(chip(
        &ChipModel {
            label: "Print draft".into(),
            trailing: Some("3".into()),
            selected: false,
            enabled: true,
        },
        Some(()),
        Some(()),
    ));

    // -- List rows: current, previewed, plain, dimmed branch.
    states.push(list_row(
        &ListRowModel {
            marker: Marker::Current,
            leading: "5".into(),
            label: "Shadows +25".into(),
            trailing: Some("you".into()),
            dimmed: false,
            tag: None,
            enabled: true,
        },
        Some(()),
        Some(()),
    ));
    states.push(list_row(
        &ListRowModel {
            marker: Marker::Previewed,
            leading: "4".into(),
            label: "Vibrance +15".into(),
            trailing: Some("agent \u{b7} lw-assist".into()),
            dimmed: false,
            tag: None,
            enabled: true,
        },
        Some(()),
        Some(()),
    ));
    states.push(list_row(
        &ListRowModel {
            marker: Marker::Plain,
            leading: "3".into(),
            label: "Crop 4:5".into(),
            trailing: Some("you".into()),
            dimmed: false,
            tag: None,
            enabled: true,
        },
        Some(()),
        Some(()),
    ));
    states.push(list_row(
        &ListRowModel {
            marker: Marker::Plain,
            leading: "6".into(),
            label: "Rotate right".into(),
            trailing: None,
            dimmed: true,
            tag: Some("branch".into()),
            enabled: true,
        },
        Some(()),
        Some(()),
    ));

    // -- Notice cards: neutral and warning tone.
    states.push(notice_card(
        &NoticeCardModel {
            title: "Original not found".into(),
            body: "Edits and history are kept. Locate\u{2026} to point at the moved file.".into(),
            tone: Tone::Neutral,
        },
        vec![("Locate\u{2026}".to_string(), ())],
    ));
    states.push(notice_card(
        &NoticeCardModel {
            title: "Changed elsewhere".into(),
            body: "Draft kept. Discard or Reapply.".into(),
            tone: Tone::Warning,
        },
        vec![
            ("Discard draft".to_string(), ()),
            ("Reapply".to_string(), ()),
        ],
    ));

    // -- A floating bar holding an arbitrary child.
    states.push(floating_bar(vec![label::<()>("Crop")]));

    // -- The double-click wrapper. It has no appearance of its own — it delegates size, layout and
    // -- drawing to its content — so the board shows what wrapping costs visually: nothing.
    states.push(double_click(label::<()>("Double-click to reset"), ()));

    // -- The mode strip: canvas modes plus a separated toggle group.
    states.push(mode_strip(
        &[
            ModeEntry {
                label: "Pointer".into(),
                shortcut: Some("V".into()),
                selected: true,
                enabled: true,
            },
            ModeEntry {
                label: "Crop".into(),
                shortcut: Some("R".into()),
                selected: false,
                enabled: true,
            },
        ],
        |_: usize| (),
        &[ToggleEntry {
            label: "Thirds".into(),
            shortcut: Some("O".into()),
            on: false,
        }],
        |_: usize| (),
    ));

    // -- An inline menu.
    states.push(inline_menu(vec![
        ("Copy as JSON request".to_string(), ()),
        ("Show in schema".to_string(), ()),
    ]));

    // -- The histogram: a ready plot, and the same plot marked stale while a newer reduction runs.
    let mut ready = HistogramModel::default();
    for (index, channel) in ready.channels.iter_mut().enumerate() {
        for code in 0..BINS {
            // A different lobe per channel, so the overlap the contract asks for is visible.
            let centre = 64.0 + 64.0 * index as f32;
            let spread = 48.0;
            let offset = (code as f32 - centre) / spread;
            channel.bins[code] = (-offset * offset).exp();
        }
    }
    states.push(histogram::<()>(&ready));
    states.push(histogram::<()>(&HistogramModel {
        stale: true,
        ..ready
    }));
    // -- Empty: every bin zero, which is not the same thing as no result at all.
    states.push(histogram::<()>(&HistogramModel {
        channels: [HistogramChannel {
            bins: [0.0; BINS],
            color: theme::CHANNEL_RED,
        }; 3],
        stale: false,
        version: 0,
    }));

    // -- The clipping triangles: untinted (no endpoint pixels), tinted, tinted and active.
    for (tinted, active) in [(false, false), (true, false), (true, true)] {
        states.push(clip_triangle(
            &ClipTriangleModel {
                icon: Icon::ShadowClipping,
                tooltip: "Any channel at 0 \u{b7} blue; both endpoints \u{b7} magenta".into(),
                tint: theme::CLIPPING_SHADOW,
                tinted,
                active,
                enabled: true,
            },
            Some(()),
        ));
    }
    states.push(clip_triangle(
        &ClipTriangleModel {
            icon: Icon::HighlightClipping,
            tooltip: "Any channel at 255 \u{b7} red; both endpoints \u{b7} magenta".into(),
            tint: theme::CLIPPING_HIGHLIGHT,
            tinted: true,
            active: true,
            enabled: false,
        },
        None,
    ));

    // -- Text helpers, standalone.
    states.push(title::<()>("Basic"));
    states.push(label::<()>("White balance"));
    states.push(caption::<()>("Output \u{b7} sRGB \u{b7} after crop"));
    states.push(section_label::<()>("Tone"));
    states.push(error_caption::<()>("Range is -100 to 100"));
    states.push(value_text::<()>("+0.62"));

    states.extend(crate::gallery_components::gallery_components());
    states.extend(crate::gallery_panels::gallery_panels());
    states
}
