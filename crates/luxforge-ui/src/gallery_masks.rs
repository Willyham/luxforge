//! Gallery states for the Masks panel's widgets, at the reference panel width, as the mask-panels
//! board draws them: the mask and component rows, the mode and overlay controls, the kind menu,
//! the group rules, the two-column fields, the toggle rows with hints, the colour range's swatches
//! and a brush component's strokes. Widget states only: no composed panel.

use crate::{
    CombineMode, ComponentRowMessages, ComponentRowModel, CoverageThumbnailModel,
    DropdownButtonModel, GridField, GroupRuleModel, Icon, MaskRowMessages, MaskRowModel, MenuEntry,
    MenuItem, ModeControlModel, NumberFieldModel, OverlayControlModel, OverlayMode, OverlayTint,
    POPOVER_GAP, StrokeRowModel, SwatchSlotsModel, ToggleModel, ValueEdit, caption, compact_toggle,
    component_note, component_row, coverage_thumbnail, dropdown_button, field_grid, group_rule,
    mask_row, menu_list, mode_control, overlay_control, stroke_row, swatch_slots, theme,
};
use iced::widget::{Column, Row, Space, container, row};
use iced::{Alignment, Element, Length, Padding};
use std::sync::Arc;

/// The width of the board's panels; the rows sit in its body's 12 pt side padding.
const REFERENCE_PANEL_WIDTH: f32 = 300.0;

fn panel(rows: Vec<Element<'static, ()>>) -> Element<'static, ()> {
    container(Column::with_children(rows).spacing(theme::ROW_SPACING))
        .padding(Padding {
            top: 4.0,
            right: 12.0,
            bottom: 10.0,
            left: 12.0,
        })
        .width(Length::Fixed(REFERENCE_PANEL_WIDTH))
        .style(theme::panel_surface)
        .into()
}

/// A 28 × 19 coverage grid from `coverage(x, y)` over the unit square.
fn grid(version: u64, coverage: impl Fn(f32, f32) -> f32) -> CoverageThumbnailModel {
    let (width, height) = (28, 19);
    let cells: Arc<[u8]> = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let value = coverage(
                (x as f32 + 0.5) / width as f32,
                (y as f32 + 0.5) / height as f32,
            );
            (value.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();
    CoverageThumbnailModel {
        cells: Some(cells),
        width,
        height,
        version,
    }
}

fn sky() -> CoverageThumbnailModel {
    grid(1, |_, y| (0.55 - y) * 6.0)
}

fn face() -> CoverageThumbnailModel {
    grid(2, |x, y| {
        let (dx, dy) = ((x - 0.5) / 0.3, (y - 0.45) / 0.35);
        1.0 - (dx * dx + dy * dy).sqrt()
    })
}

fn foreground() -> CoverageThumbnailModel {
    grid(3, |x, y| (y - 0.7 + (x - 0.5) * 0.35) * 5.0)
}

fn mask(name: &str, coverage: CoverageThumbnailModel, amount: &str) -> MaskRowModel {
    MaskRowModel {
        name: name.into(),
        coverage,
        active: true,
        amount: amount.into(),
        visible: true,
        visibility_tooltip: "Hide this mask's overlay".into(),
        menu_tooltip: "Mask actions".into(),
        selected: false,
        menu_open: false,
        enabled: true,
    }
}

fn mask_messages() -> MaskRowMessages<()> {
    MaskRowMessages {
        on_select: Some(()),
        on_toggle_visibility: Some(()),
        on_menu: Some(()),
        on_drag_start: Some(()),
    }
}

const FIXED: &str = "A mask's first component is always Add";
const MODES: &str = "Add \u{b7} Subtract \u{b7} Intersect";

fn mode(selected: CombineMode) -> ModeControlModel {
    ModeControlModel {
        selected,
        fixed: None,
        tooltip: MODES.into(),
        enabled: true,
    }
}

fn component(name: &str, icon: Icon, mode: ModeControlModel) -> ComponentRowModel {
    ComponentRowModel {
        name: name.into(),
        icon: Some(icon),
        mode,
        inverted: false,
        invert_tooltip: "Invert".into(),
        menu_tooltip: "Component actions".into(),
        selected: false,
        hovered: false,
        menu_open: false,
        enabled: true,
    }
}

fn fixed() -> ModeControlModel {
    ModeControlModel {
        fixed: Some(FIXED.into()),
        ..mode(CombineMode::Add)
    }
}

fn component_messages() -> ComponentRowMessages<'static, ()> {
    ComponentRowMessages {
        on_select: Some(()),
        on_mode: Some(Box::new(|_| ())),
        on_invert: Some(()),
        on_menu: Some(()),
        on_drag_start: Some(()),
        on_hover_enter: Some(()),
        on_hover_exit: Some(()),
    }
}

fn overlay(mode: OverlayMode, tint: OverlayTint) -> OverlayControlModel {
    OverlayControlModel {
        label: "Overlay".into(),
        mode,
        mode_names: [
            "Overlay off".into(),
            "Tint over the photo".into(),
            "Selection on black".into(),
            "Photo through the selection".into(),
        ],
        tint,
        tint_names: ["Green tint".into(), "White tint".into()],
        hint: Some("\u{21e7}M".into()),
        enabled: true,
    }
}

fn field(
    label: &str,
    display: &str,
    unit: Option<&str>,
    edit: ValueEdit,
) -> GridField<'static, ()> {
    GridField {
        model: NumberFieldModel {
            id: None,
            label: label.into(),
            display: display.into(),
            edit,
            unit: unit.map(Into::into),
            enabled: true,
        },
        on_edit_start: (),
        on_text: Box::new(|_| ()),
        on_submit: (),
        on_reset: (),
    }
}

fn item(icon: Icon, label: &str, trailing: Option<&str>, enabled: bool) -> MenuEntry<()> {
    MenuEntry::Item(MenuItem {
        icon: Some(icon),
        label: label.into(),
        trailing: trailing.map(Into::into),
        on_press: enabled.then_some(()),
    })
}

fn stroke(index: &str, label: &str, enabled: bool) -> Element<'static, ()> {
    stroke_row(
        &StrokeRowModel {
            index: index.into(),
            label: label.into(),
            delete_tooltip: if enabled {
                "Delete this stroke".into()
            } else {
                "A brush component keeps at least one stroke".into()
            },
            delete_enabled: enabled,
        },
        Some(()),
    )
}

/// The Masks panel's widget states, in the order `gallery_named_states` names them.
pub(crate) fn gallery_masks() -> Vec<Element<'static, ()>> {
    let mut states = Vec::new();

    // -- Mask rows: at rest, the open mask, and a mask whose overlay is hidden and whose layers are
    // -- neutral.
    states.push(panel(vec![
        mask_row(&mask("Sky", sky(), "100"), mask_messages()),
        mask_row(
            &MaskRowModel {
                selected: true,
                ..mask("Face", face(), "80")
            },
            mask_messages(),
        ),
        mask_row(
            &MaskRowModel {
                visible: false,
                active: false,
                visibility_tooltip: "Show this mask's overlay".into(),
                ..mask(
                    "Foreground with a name too long for its row",
                    foreground(),
                    "100",
                )
            },
            mask_messages(),
        ),
    ]));
    // -- A mask row with its menu open, and a disabled one.
    states.push(panel(vec![
        mask_row(
            &MaskRowModel {
                menu_open: true,
                ..mask("Sky", sky(), "100")
            },
            mask_messages(),
        ),
        mask_row(
            &MaskRowModel {
                enabled: false,
                ..mask("Face", face(), "80")
            },
            mask_messages(),
        ),
    ]));
    // -- Coverage thumbnails: a grid, a finer grid sampled down, and the pending placeholder.
    let fine: Arc<[u8]> = (0..112 * 76)
        .map(|index| {
            if (index % 112 / 8 + index / 112 / 8) % 2 == 0 {
                255
            } else {
                40
            }
        })
        .collect();
    states.push(
        row![
            coverage_thumbnail(&face()),
            coverage_thumbnail(&CoverageThumbnailModel {
                cells: Some(fine),
                width: 112,
                height: 76,
                version: 4,
            }),
            coverage_thumbnail(&CoverageThumbnailModel::default()),
        ]
        .spacing(theme::SPACING)
        .into(),
    );

    // -- Component rows: the first, fixed to Add; an inverted Add; a Subtract; an Intersect; one of
    // -- each kind icon.
    states.push(panel(vec![
        component_row(
            &component("Linear 1", Icon::Linear, fixed()),
            component_messages(),
        ),
        component_row(
            &ComponentRowModel {
                inverted: true,
                ..component("Radial 1", Icon::Radial, mode(CombineMode::Add))
            },
            component_messages(),
        ),
        component_row(
            &component("Brush 1", Icon::Brush, mode(CombineMode::Subtract)),
            component_messages(),
        ),
        component_row(
            &component("Luminance 1", Icon::Luminance, mode(CombineMode::Intersect)),
            component_messages(),
        ),
        component_row(
            &component("Colour 1", Icon::Colour, mode(CombineMode::Intersect)),
            component_messages(),
        ),
    ]));
    // -- Component rows: selected, hovered (its coverage in the overlay), with its menu open, and
    // -- disabled; then a refusal as a note under a row.
    states.push(panel(vec![
        component_row(
            &ComponentRowModel {
                selected: true,
                ..component("Radial 1", Icon::Radial, fixed())
            },
            component_messages(),
        ),
        component_row(
            &ComponentRowModel {
                hovered: true,
                ..component("Brush 1", Icon::Brush, mode(CombineMode::Subtract))
            },
            component_messages(),
        ),
        component_row(
            &ComponentRowModel {
                menu_open: true,
                ..component("Luminance 1", Icon::Luminance, mode(CombineMode::Intersect))
            },
            component_messages(),
        ),
        component_row(
            &ComponentRowModel {
                enabled: false,
                ..component("Brush 2", Icon::Brush, mode(CombineMode::Subtract))
            },
            ComponentRowMessages::default(),
        ),
        component_note("Cannot move above Linear 1: a mask's first component is always Add"),
    ]));
    // -- The mode control on its own: each mode chosen, fixed, and disabled.
    let mut modes = Row::new()
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);
    for selected in CombineMode::ALL {
        modes = modes.push(mode_control(&mode(selected), |_| ()));
    }
    modes = modes.push(mode_control(&fixed(), |_| ()));
    modes = modes.push(mode_control(
        &ModeControlModel {
            enabled: false,
            ..mode(CombineMode::Subtract)
        },
        |_| (),
    ));
    states.push(modes.into());
    // -- The overlay row: tint in green, the selection on black with white chosen (the swatches
    // -- dimmed), and off.
    states.push(panel(vec![
        overlay_control(
            &overlay(OverlayMode::Tint, OverlayTint::Green),
            |_| (),
            |_| (),
        ),
        overlay_control(
            &overlay(OverlayMode::Mask, OverlayTint::White),
            |_| (),
            |_| (),
        ),
        overlay_control(
            &overlay(OverlayMode::Off, OverlayTint::Green),
            |_| (),
            |_| (),
        ),
    ]));

    // -- New mask: the button, its count, and its kind menu with a disabled item.
    let new_mask = DropdownButtonModel {
        label: "New mask".into(),
        compact: false,
        enabled: true,
    };
    states.push(
        Column::new()
            .push(
                row![
                    dropdown_button(&new_mask, Some(())),
                    Space::new().width(Length::Fill),
                    caption("2 of 16"),
                ]
                .align_y(Alignment::Center)
                .width(Length::Fixed(REFERENCE_PANEL_WIDTH - 24.0)),
            )
            .push(menu_list(vec![
                item(Icon::Linear, "Linear gradient", Some("L"), true),
                item(Icon::Radial, "Radial gradient", Some("R"), true),
                item(Icon::Brush, "Brush", Some("B"), true),
                MenuEntry::Separator,
                item(Icon::Luminance, "Luminance range", None, true),
                item(Icon::Colour, "Colour range", None, true),
                MenuEntry::Separator,
                item(Icon::Spark, "Background", Some("install\u{2026}"), false),
            ]))
            .spacing(POPOVER_GAP)
            .into(),
    );
    // -- The Add row: Add component, then the next component's mode after `as`; and the refused
    // -- New mask at the limit.
    let add = DropdownButtonModel {
        label: "Add component".into(),
        compact: true,
        enabled: true,
    };
    states.push(panel(vec![
        row![
            dropdown_button(&add, Some(())),
            Space::new().width(Length::Fill),
            caption("as"),
            mode_control(&mode(CombineMode::Subtract), |_| ()),
        ]
        .spacing(theme::OVERLAY_SPACING)
        .align_y(Alignment::Center)
        .into(),
        row![
            dropdown_button(
                &DropdownButtonModel {
                    enabled: false,
                    ..new_mask
                },
                None,
            ),
            Space::new().width(Length::Fill),
            caption("16 of 16"),
        ]
        .align_y(Alignment::Center)
        .into(),
    ]));
    // -- Group rules: the open mask with its count and menu, and the armed brush.
    states.push(panel(vec![
        group_rule(
            &GroupRuleModel {
                label: "Face".into(),
                caption: Some("3 components".into()),
                caption_accent: false,
                menu_tooltip: Some("Mask actions".into()),
                menu_open: false,
                enabled: true,
            },
            Some(()),
        ),
        group_rule(
            &GroupRuleModel {
                label: "Brush".into(),
                caption: Some("armed \u{b7} Esc puts it down".into()),
                caption_accent: true,
                menu_tooltip: None,
                menu_open: false,
                enabled: true,
            },
            None,
        ),
    ]));
    // -- A radial's six fields in two columns under its selected row.
    states.push(panel(vec![
        component_row(
            &ComponentRowModel {
                selected: true,
                ..component("Radial 1", Icon::Radial, fixed())
            },
            component_messages(),
        ),
        field_grid(vec![
            field("x", "0.52", None, ValueEdit::Display),
            field("y", "0.31", None, ValueEdit::Display),
            field("radius x", "0.180", None, ValueEdit::Display),
            field("radius y", "0.240", None, ValueEdit::Display),
            field("angle", "\u{2212}12", Some("\u{b0}"), ValueEdit::Display),
            field("feather", "60", None, ValueEdit::Display),
        ]),
    ]));
    // -- A linear's four fields, one being typed and one refused.
    states.push(panel(vec![field_grid(vec![
        field(
            "x0",
            "0.10",
            None,
            ValueEdit::Editing {
                text: "0.1".into(),
                invalid: None,
            },
        ),
        field("y0", "0.92", None, ValueEdit::Display),
        field(
            "x1",
            "0.14",
            None,
            ValueEdit::Editing {
                text: "1.4".into(),
                invalid: Some("x1 is 0 to 1".into()),
            },
        ),
        field("y1", "0.38", None, ValueEdit::Display),
    ])]));
    // -- Toggle rows with hints, and the plain compact row the open mask's Invert uses.
    let toggle = |label: &str, on: bool| ToggleModel {
        label: label.into(),
        on,
        enabled: true,
    };
    states.push(panel(vec![
        compact_toggle(&toggle("Erase", true), Some("hold \u{2325}".into()), |_| ()),
        compact_toggle(
            &toggle("Limit to colour", true),
            Some("refine 50".into()),
            |_| (),
        ),
        compact_toggle(&toggle("Invert mask", false), None, |_| ()),
        compact_toggle(
            &ToggleModel {
                enabled: false,
                ..toggle("Erase", false)
            },
            Some("hold \u{2325}".into()),
            |_| (),
        ),
    ]));
    // -- A colour range's swatches: three of five held, then picking with one chosen, then full.
    let held = vec![[0x6f, 0x9b, 0xd1], [0x5d, 0x8b, 0xc4], [0x8f, 0xb3, 0xe0]];
    let swatches = SwatchSlotsModel {
        swatches: held.clone(),
        limit: 5,
        selected: None,
        picking: false,
        pick_label: "Pick".into(),
        count: "3 of 5".into(),
        pick_enabled: true,
        enabled: true,
    };
    let mut full = held;
    full.extend([[0x4a, 0x78, 0xb0], [0xa5, 0xc4, 0xea]]);
    states.push(panel(vec![
        swatch_slots(&swatches, |_| (), ()),
        swatch_slots(
            &SwatchSlotsModel {
                picking: true,
                selected: Some(1),
                ..swatches.clone()
            },
            |_| (),
            (),
        ),
        swatch_slots(
            &SwatchSlotsModel {
                swatches: full,
                count: "5 of 5".into(),
                pick_enabled: false,
                ..swatches
            },
            |_| (),
            (),
        ),
    ]));
    // -- A brush component's strokes: two deletable, and one whose delete is refused.
    states.push(panel(vec![
        component_row(
            &ComponentRowModel {
                selected: true,
                ..component("Brush 1", Icon::Brush, mode(CombineMode::Subtract))
            },
            component_messages(),
        ),
        Column::new()
            .push(stroke("1", "add \u{b7} 0.06 \u{b7} f50", true))
            .push(stroke("2", "add \u{b7} 0.06 \u{b7} f50", true))
            .push(stroke(
                "3",
                "erase \u{b7} 0.04 \u{b7} f30 \u{b7} colour-held",
                false,
            ))
            .spacing(theme::STROKE_LIST_SPACING)
            .into(),
    ]));
    states
}
