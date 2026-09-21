//! The tools panel: one collapsible section per registered module, rendered from [`ToolsModel`]
//! with the widget library. The view knows no tool and no parameter limit; it draws what the
//! section says and publishes messages, exactly as the generated-control mapping in
//! `state::tools` describes it.
use crate::{
    app::{
        fields,
        message::{ClipEndpoint, CropMessage, MenuTarget, Message},
    },
    state::{
        histogram::{HIGHLIGHT_RULE, HistogramModel, SHADOW_RULE},
        tools::{
            ActionControl, ActionControlStyle, ChoiceControlStyle, ColorControl, ColorControlStyle,
            ControlModel, CropSectionModel, CurveControl, EnumControl, GroupControl,
            NumberControlStyle, RailStyle, SectionModel, SliderControl, ToggleControl, ToolsModel,
            ValueEdit,
        },
    },
};
use iced::{
    Alignment, Color, Element, Length,
    widget::{Row, button, column, mouse_area, row, scrollable},
};
use lightwell_ui::{
    BINS, ChipModel, ClipTriangleModel, ColorPickerModel, ColorSwatchModel, ControlKey,
    ControlKeyEvent, CurveEditorModel, CurvePointRow, HistogramChannel, Icon, IconButtonModel,
    MenuChoiceModel, NumberFieldModel, RailDecoration, SectionHeaderModel, SegmentedModel,
    SliderModel, StepperModel, SubGroupHeaderModel, ToggleModel, caption, chip, clip_triangle,
    color_picker, color_swatch, curve_editor, error_caption, focus_control, histogram, icon_button,
    inline_menu, menu_choice, number_field, section_header, section_label, segmented, slider,
    stepper, sub_group_header, theme, toggle, value_input,
};
use serde_json::{Map, Value};

/// Stable identity for evidence scripts that scroll the actual generated tools panel.
pub(crate) fn scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("tools-panel")
}

pub(crate) fn tools_panel<'a>(
    model: &'a ToolsModel,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    if let Some(message) = model.status.message() {
        return scrollable(
            iced::widget::container(
                column![inspector(plot), caption(message)].spacing(theme::SPACING),
            )
            .padding(theme::SPACING),
        )
        .id(scroll_id())
        .height(Length::Fill)
        .into();
    }
    let menu = model.menu.as_ref();
    let mut panel = column![]
        .spacing(theme::SPACING)
        // The scroll handle overlays the content edge. Reserve its width so the number fields
        // remain legible even at the panel's narrowest supported width.
        .padding(iced::Padding {
            right: theme::SPACING * 3.0,
            top: theme::SPACING,
            bottom: theme::SPACING,
            left: theme::SPACING,
        })
        .width(Length::Fill);
    // The histogram sits above the first module section with no header of its own, as the Develop
    // workspace layout reserves.
    panel = panel.push(inspector(plot));
    for section in &model.sections {
        panel = panel.push(section_view(section, menu, plot));
    }
    if !model.developer.is_empty() {
        panel = panel.push(section_label("Developer"));
        for section in &model.developer {
            panel = panel.push(section_view(section, menu, plot));
        }
    }
    scrollable(panel)
        .id(scroll_id())
        .height(Length::Fill)
        .into()
}

/// The histogram inspector: the plot, the two clipping triangles in its bottom corners, the domain
/// caption with the pointer readout, and the textual endpoint counts under it.
///
/// The view decides nothing here. Which channel is which colour, what the counts say, which
/// triangle is tinted and what its tooltip states are all in the model; this turns them into
/// widgets and publishes one semantic message per triangle.
fn inspector(model: &HistogramModel) -> Element<'_, Message> {
    let colours = [
        theme::CHANNEL_RED,
        theme::CHANNEL_GREEN,
        theme::CHANNEL_BLUE,
    ];
    let mut channels = [HistogramChannel::default(); 3];
    for (index, channel) in channels.iter_mut().enumerate() {
        channel.color = colours[index];
        if let Some(bins) = &model.bins {
            channel.bins = bins[index];
        }
    }
    let plot = histogram(&lightwell_ui::HistogramModel {
        channels,
        stale: model.stale,
        version: plot_version(model),
    });
    let triangles = row![
        clip_triangle(
            &ClipTriangleModel {
                icon: Icon::ShadowClipping,
                tooltip: SHADOW_RULE.into(),
                tint: theme::CLIPPING_SHADOW,
                tinted: model.shadow.tinted,
                active: model.shadow.active,
                enabled: model.shadow.enabled,
            },
            Some(Message::ToggleClipping(Some(ClipEndpoint::Shadows))),
        ),
        iced::widget::Space::new().width(Length::Fill),
        clip_triangle(
            &ClipTriangleModel {
                icon: Icon::HighlightClipping,
                tooltip: HIGHLIGHT_RULE.into(),
                tint: theme::CLIPPING_HIGHLIGHT,
                tinted: model.highlight.tinted,
                active: model.highlight.active,
                enabled: model.highlight.enabled,
            },
            Some(Message::ToggleClipping(Some(ClipEndpoint::Highlights))),
        ),
    ]
    .align_y(Alignment::Center);
    let mut block = column![plot, triangles].spacing(theme::SPACING / 2.0);
    // The domain is always named, so an output endpoint count is never read as sensor clipping.
    block = block.push(caption(model.caption_line()));
    match model.notice() {
        // Pending, updating, unavailable: an explicit line rather than a silently empty plot.
        Some(notice) => block = block.push(caption(notice)),
        None => {
            // The counters in words, for a reader who cannot measure the plot's heights.
            block = block.push(caption(model.counters.shadow_text()));
            block = block.push(caption(model.counters.highlight_text()));
            block = block.push(caption(model.counters.both_text()));
        }
    }
    debug_assert_eq!(BINS, 256, "one bin per 8-bit output code");
    block.into()
}

/// A cheap identity for the plot's geometry: it moves exactly when the bins or the dimming would,
/// and holds steady across everything else a re-derive touches (a pointer move, a counter update,
/// the periodic desktop sync that redraws the panel every 500 ms while an asset is open). The
/// histogram widget hashes nothing itself — it takes this number and rebuilds its cached polygons
/// only when it changes — so a redraw with nothing new to plot reuses the tessellated geometry
/// instead of rebuilding three 256-point fills that look identical to the last frame.
///
/// `RenderIdentity` does not derive `Hash`, so this hashes its fields directly rather than adding
/// that derive to the state model.
fn plot_version(model: &HistogramModel) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match &model.identity {
        Some(identity) => {
            identity.entry.hash(&mut hasher);
            identity.draft_revision.hash(&mut hasher);
            identity.generation.hash(&mut hasher);
            identity.width.hash(&mut hasher);
            identity.height.hash(&mut hasher);
        }
        None => 0u8.hash(&mut hasher),
    }
    model.stale.hash(&mut hasher);
    hasher.finish()
}

fn curve_version(version: u64, background: bool, plot: &HistogramModel) -> u64 {
    if !background {
        return version;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    version.hash(&mut hasher);
    plot_version(plot).hash(&mut hasher);
    hasher.finish()
}

fn section_view<'a>(
    section: &'a SectionModel,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    let header = section_header(
        &SectionHeaderModel {
            title: section.title.clone(),
            expanded: section.expanded,
            active: section.active,
            hint: section.hint.clone(),
            unavailable: section.unavailable.clone(),
            reset: section.reset.is_some(),
            enabled: section.enabled,
        },
        Message::ToggleSection(section.module_id.clone()),
        Message::ResetModule(section.module_id.clone()),
    );
    let mut block = column![header].spacing(theme::SPACING);
    // An unavailable module cannot expand, per the design; nothing under it is drawn. Otherwise a
    // disabled section (busy, a historical preview) still shows its values, just not interactive.
    if section.expanded && section.unavailable.is_none() {
        for control in &section.controls {
            block = block.push(control_view(
                &section.module_id,
                section.enabled,
                control,
                menu,
                plot,
            ));
        }
    }
    block.into()
}

fn control_view<'a>(
    module_id: &str,
    enabled: bool,
    control: &'a ControlModel,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    match control {
        ControlModel::Slider(field) => number_view(enabled, field, menu),
        ControlModel::Toggle(toggle) => toggle_view(enabled, toggle, menu),
        ControlModel::Enum(choice) => enum_view(enabled, choice, menu),
        ControlModel::Color(color) => color_view(enabled, color, menu),
        ControlModel::Curve(curve) => curve_view(enabled, curve, menu, plot),
        ControlModel::Group(group) => group_view(module_id, enabled, group, menu, plot),
        ControlModel::Action(action) => action_view(action, menu),
        ControlModel::Unsupported(message) => error_caption(message.clone()),
        ControlModel::CropFrame(frame) => crop_section_view(frame, menu),
    }
}

/// Whether this control is the one whose context menu is open. A patch action's controls are one
/// per field, so the field is part of the identity.
fn menu_open_for_preset(
    menu: Option<&MenuTarget>,
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> bool {
    matches!(
        menu,
        Some(MenuTarget::Control { action: open, parameter: named, preset: saved })
            if open == action && named.as_deref() == parameter && saved.as_ref() == preset
    )
}

/// The control's own context-menu target.
fn control_target_preset(
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> MenuTarget {
    MenuTarget::Control {
        action: action.to_owned(),
        parameter: parameter.map(str::to_owned),
        preset: preset.cloned(),
    }
}

/// The "Copy as JSON request" / "Cancel" menu a generated control's context menu opens, for the
/// exact `edit.<action>` request its current values would send.
fn control_menu_preset(
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> Element<'static, Message> {
    let name = match parameter {
        Some(parameter) => format!("edit.{action} · {parameter}"),
        None => format!("edit.{action}"),
    };
    column![
        caption(name),
        inline_menu(vec![
            (
                "Copy as JSON request".to_owned(),
                Message::CopyRequest {
                    action: action.to_owned(),
                    parameter: parameter.map(str::to_owned),
                    preset: preset.cloned(),
                },
            ),
            ("Cancel".to_owned(), Message::CloseMenu),
        ])
    ]
    .spacing(3.0)
    .into()
}

/// Wrap a generated control so a right-click on it opens its own context menu, and append the
/// menu itself directly under the control when it is the one currently open.
fn with_control_menu<'a>(
    control: Element<'a, Message>,
    action: &str,
    parameter: Option<&str>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    with_control_menu_preset(control, action, parameter, None, menu)
}

fn with_control_menu_preset<'a>(
    control: Element<'a, Message>,
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let area: Element<'a, Message> = mouse_area(control)
        .on_right_press(Message::OpenMenu(control_target_preset(
            action, parameter, preset,
        )))
        .into();
    if menu_open_for_preset(menu, action, parameter, preset) {
        column![area, control_menu_preset(action, parameter, preset)]
            .spacing(4.0)
            .into()
    } else {
        area
    }
}

fn ui_edit(edit: &ValueEdit, display: &str, invalid: &Option<String>) -> lightwell_ui::ValueEdit {
    match edit {
        ValueEdit::Typing(_) => lightwell_ui::ValueEdit::Editing {
            text: edit.text(display).to_owned(),
            invalid: invalid.clone(),
        },
        ValueEdit::None => lightwell_ui::ValueEdit::Display,
    }
}

fn rail_decoration(rail: &RailStyle) -> RailDecoration {
    let colors: Vec<[u8; 3]> = match rail {
        RailStyle::Plain => return RailDecoration::Plain,
        RailStyle::Hue => (0..=6)
            .map(|index| lightwell_ui::hsv_to_rgb([index as f64 / 6.0, 1.0, 1.0]))
            .collect(),
        RailStyle::Temperature => vec![[72, 132, 235], [225, 225, 225], [236, 163, 70]],
        RailStyle::Tint => vec![[87, 168, 96], [225, 225, 225], [207, 99, 168]],
        RailStyle::Gradient(stops) => stops.clone(),
    };
    RailDecoration::Colors(
        colors
            .into_iter()
            .map(|[r, g, b]| Color::from_rgb8(r, g, b))
            .collect(),
    )
}

fn number_view<'a>(
    enabled: bool,
    field: &'a SliderControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let step = field.step;
    let edit = ui_edit(&field.edit, &field.display, &field.invalid);
    let (action, parameter) = (field.action.clone(), field.parameter.clone());
    let edit_start = Message::EditValue {
        action: action.clone(),
        parameter: parameter.clone(),
    };
    let submit = Message::Submit {
        action: action.clone(),
        parameter: Some(parameter.clone()),
    };
    let reset = Message::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    };
    let text_action = action.clone();
    let text_parameter = parameter.clone();
    let on_text = move |text| Message::Field {
        action: text_action.clone(),
        parameter: text_parameter.clone(),
        text,
    };
    let field_model = NumberFieldModel {
        label: field.label.clone(),
        display: field.display.clone(),
        edit: edit.clone(),
        unit: field.unit.clone(),
        enabled,
        id: Some(field.id.clone()),
    };
    let control: Element<'a, Message> = match field.style {
        NumberControlStyle::Slider => {
            let move_action = action.clone();
            let move_parameter = parameter.clone();
            slider(
                &SliderModel {
                    id: Some(field.id.clone()),
                    label: field.label.clone(),
                    min: field.min,
                    max: field.max,
                    soft_min: field.soft_min,
                    soft_max: field.soft_max,
                    value: field.value,
                    step,
                    shift_step: step * 10.0,
                    fine_step: field.fine_step,
                    zero: (field.soft_min..=field.soft_max)
                        .contains(&field.zero)
                        .then_some(field.zero),
                    rail: rail_decoration(&field.rail),
                    over_range: lightwell_ui::geometry::over_range_side(
                        field.soft_min,
                        field.soft_max,
                        field.value,
                    ),
                    unit: field.unit.clone(),
                    display: field.display.clone(),
                    edit,
                    dragging: field.dragging,
                    enabled,
                },
                move |fraction| Message::ControlFraction {
                    action: move_action.clone(),
                    parameter: move_parameter.clone(),
                    fraction,
                },
                Message::ControlReleased {
                    action: action.clone(),
                    parameter: parameter.clone(),
                },
                edit_start,
                on_text,
                submit,
                reset,
            )
        }
        NumberControlStyle::Field => number_field(&field_model, edit_start, on_text, submit, reset),
        NumberControlStyle::Stepper => stepper(
            &StepperModel {
                field: field_model,
                decrement_enabled: field.value > field.min,
                increment_enabled: field.value < field.max,
                decrement_tooltip: "Decrease".into(),
                increment_tooltip: "Increase".into(),
            },
            Message::ControlStep {
                action: action.clone(),
                parameter: parameter.clone(),
                direction: -1,
            },
            Message::ControlStep {
                action: action.clone(),
                parameter: parameter.clone(),
                direction: 1,
            },
            edit_start,
            on_text,
            submit,
            reset,
        ),
    };
    let control = match field.style {
        NumberControlStyle::Slider => control,
        NumberControlStyle::Field => {
            let action = field.action.clone();
            let parameter = field.parameter.clone();
            let enter = if matches!(field.edit, ValueEdit::Typing(_)) {
                Message::Submit {
                    action: action.clone(),
                    parameter: Some(parameter.clone()),
                }
            } else {
                Message::EditValue {
                    action: action.clone(),
                    parameter: parameter.clone(),
                }
            };
            focus_control(control, enabled, move |event| match event {
                ControlKeyEvent::Pressed {
                    key: ControlKey::Enter,
                    ..
                } => Some(enter.clone()),
                ControlKeyEvent::Pressed { key, shift, option } => {
                    key_direction(key).map(|direction| Message::ControlFieldNudge {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        direction,
                        shift,
                        option,
                    })
                }
                _ => None,
            })
        }
        NumberControlStyle::Stepper => {
            let action = field.action.clone();
            let parameter = field.parameter.clone();
            let enter = if matches!(field.edit, ValueEdit::Typing(_)) {
                Message::Submit {
                    action: action.clone(),
                    parameter: Some(parameter.clone()),
                }
            } else {
                Message::EditValue {
                    action: action.clone(),
                    parameter: parameter.clone(),
                }
            };
            focus_control(control, enabled, move |event| match event {
                ControlKeyEvent::Pressed {
                    key: ControlKey::Enter,
                    ..
                } => Some(enter.clone()),
                ControlKeyEvent::Pressed { key, shift, option } => {
                    key_direction(key).map(|direction| Message::ControlKeyNudge {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        direction,
                        shift,
                        option,
                    })
                }
                ControlKeyEvent::Released(key) if key_direction(key).is_some() => {
                    Some(Message::ControlReleased {
                        action: action.clone(),
                        parameter: parameter.clone(),
                    })
                }
                _ => None,
            })
        }
    };
    with_control_menu(control, &field.action, Some(&field.parameter), menu)
}

fn key_direction(key: ControlKey) -> Option<i8> {
    match key {
        ControlKey::Left | ControlKey::Down => Some(-1),
        ControlKey::Right | ControlKey::Up => Some(1),
        _ => None,
    }
}

fn activates(event: ControlKeyEvent) -> bool {
    matches!(
        event,
        ControlKeyEvent::Pressed {
            key: ControlKey::Space | ControlKey::Enter,
            ..
        }
    )
}

fn toggle_view<'a>(
    enabled: bool,
    control: &'a ToggleControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let action = control.action.clone();
    let parameter = control.parameter.clone();
    let widget = toggle(
        &ToggleModel {
            label: control.label.clone(),
            on: control.on,
            enabled,
        },
        move |on| Message::ControlDiscrete {
            action: action.clone(),
            parameter: parameter.clone(),
            value: Value::Bool(on),
        },
    );
    let action = control.action.clone();
    let parameter = control.parameter.clone();
    let on = control.on;
    let widget = focus_control(widget, enabled, move |event| match event {
        ControlKeyEvent::Pressed {
            key: ControlKey::Space | ControlKey::Enter,
            ..
        } => Some(Message::ControlDiscrete {
            action: action.clone(),
            parameter: parameter.clone(),
            value: Value::Bool(!on),
        }),
        _ => None,
    });
    with_control_menu(widget, &control.action, Some(&control.parameter), menu)
}

fn enum_view<'a>(
    enabled: bool,
    choice: &'a EnumControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let (action, parameter, options) = (
        choice.action.clone(),
        choice.parameter.clone(),
        choice.options.clone(),
    );
    let mut field = column![lightwell_ui::label(choice.label.clone())].spacing(4.0);
    match choice.style {
        ChoiceControlStyle::Segmented => {
            field = field.push(segmented(
                &SegmentedModel {
                    options: choice.options.clone(),
                    selected: choice.selected.unwrap_or(0),
                    enabled,
                },
                move |index| Message::ControlDiscrete {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    value: Value::String(options[index].clone()),
                },
            ));
        }
        ChoiceControlStyle::Chips => {
            let chips = choice.options.iter().enumerate().map(|(index, option)| {
                chip(
                    &ChipModel {
                        label: option.clone(),
                        trailing: None,
                        selected: choice.selected == Some(index),
                        enabled,
                    },
                    Some(Message::ControlDiscrete {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        value: Value::String(option.clone()),
                    }),
                    None,
                )
            });
            field = field.push(Row::new().spacing(4.0).extend(chips).wrap());
        }
        ChoiceControlStyle::Menu => {
            let action = choice.action.clone();
            let parameter = choice.parameter.clone();
            let options = choice.options.clone();
            field = field.push(menu_choice(
                &MenuChoiceModel {
                    label: choice.label.clone(),
                    options: options.clone(),
                    selected: choice.selected.unwrap_or(0),
                    enabled,
                },
                move |index| Message::ControlDiscrete {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    value: Value::String(options[index].clone()),
                },
            ));
        }
    }
    let action = choice.action.clone();
    let parameter = choice.parameter.clone();
    let options = choice.options.clone();
    let selected = choice.selected.unwrap_or(0);
    let widget = focus_control(field.into(), enabled, move |event| match event {
        ControlKeyEvent::Pressed { key, .. } => key_direction(key).and_then(|direction| {
            let next = (selected as isize + direction as isize)
                .clamp(0, options.len().saturating_sub(1) as isize) as usize;
            options.get(next).map(|option| Message::ControlDiscrete {
                action: action.clone(),
                parameter: parameter.clone(),
                value: Value::String(option.clone()),
            })
        }),
        _ => None,
    });
    with_control_menu(widget, &choice.action, Some(&choice.parameter), menu)
}

fn color_view<'a>(
    enabled: bool,
    color: &'a ColorControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let swatch = color_swatch(
        &ColorSwatchModel {
            rgb: color.rgb,
            enabled: enabled && color.style == ColorControlStyle::Picker,
            open: color.picker_open,
        },
        Message::TogglePicker {
            action: color.action.clone(),
            parameter: color.parameter.clone(),
        },
    );
    let action = color.action.clone();
    let parameter = color.parameter.clone();
    let swatch = focus_control(
        swatch,
        enabled && color.style == ColorControlStyle::Picker,
        move |event| {
            activates(event).then(|| Message::TogglePicker {
                action: action.clone(),
                parameter: parameter.clone(),
            })
        },
    );
    let mut body = column![
        row![lightwell_ui::label(color.label.clone()), swatch]
            .spacing(4.0)
            .align_y(Alignment::Center)
    ]
    .spacing(4.0);
    match color.style {
        ColorControlStyle::Fields => {
            let mut channels = row![].spacing(4.0).align_y(Alignment::Center);
            for (index, value) in color.channels.iter().enumerate() {
                let (action, parameter, current) = (
                    color.action.clone(),
                    color.parameter.clone(),
                    color.text.clone(),
                );
                channels = channels.push(
                    value_input(
                        fields::CHANNELS[index],
                        value,
                        color.invalid.is_some(),
                        enabled,
                        move |text| Message::Field {
                            action: action.clone(),
                            parameter: parameter.clone(),
                            text: fields::replace_channel(&current, index, &text),
                        },
                        Message::Submit {
                            action: color.action.clone(),
                            parameter: Some(color.parameter.clone()),
                        },
                    )
                    .id(color.ids[index].clone())
                    .width(Length::Fixed(48.0)),
                );
            }
            body = body.push(channels);
        }
        ColorControlStyle::Picker if color.picker_open => {
            let hsv = color
                .picker_hsv
                .unwrap_or_else(|| lightwell_ui::rgb_to_hsv(color.rgb));
            let model = ColorPickerModel {
                hue: hsv[0] as f32,
                saturation: hsv[1] as f32,
                value: hsv[2] as f32,
                rgb: color.rgb,
                channels: std::array::from_fn(|index| {
                    ui_edit(
                        &color.channel_edits[index],
                        &color.channels[index],
                        &color.invalid,
                    )
                }),
                hex: ui_edit(
                    &color.hex_edit,
                    &lightwell_ui::rgb_to_hex(color.rgb),
                    &color.invalid,
                ),
                dragging: color.dragging,
                enabled,
                version: color.version,
            };
            let action = color.action.clone();
            let parameter = color.parameter.clone();
            body = body.push(color_picker(&model, move |event| Message::ControlPicker {
                action: action.clone(),
                parameter: parameter.clone(),
                event,
            }));
        }
        ColorControlStyle::Picker => {}
    }
    if let Some(message) = &color.invalid {
        body = body.push(error_caption(message.clone()));
    }
    with_control_menu(body.into(), &color.action, Some(&color.parameter), menu)
}

fn curve_view<'a>(
    enabled: bool,
    curve: &'a CurveControl,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    let Some(channel) = curve
        .channels
        .get(curve.selected_channel)
        .or_else(|| curve.channels.first())
    else {
        return error_caption(format!("{} has no curve channel", curve.label));
    };
    let background = if curve.background {
        plot.bins.as_ref().map(|bins| {
            std::array::from_fn(|index| {
                bins.iter()
                    .map(|channel| channel[index])
                    .fold(0.0_f32, f32::max)
            })
        })
    } else {
        None
    };
    let point_rows = curve
        .point_rows
        .iter()
        .map(|row| CurvePointRow {
            display: row.display.clone(),
            edit: std::array::from_fn(|axis| ui_edit(&row.edit[axis], &row.display[axis], &None)),
        })
        .collect();
    let version = curve_version(curve.version, curve.background, plot);
    let model = CurveEditorModel {
        points: curve.points.clone(),
        sampled: curve.sampled.clone(),
        point_rows,
        selected: curve.selected_point,
        background,
        identity: curve.identity,
        channels: curve
            .channels
            .iter()
            .map(|channel| channel.label.clone())
            .collect(),
        selected_channel: curve.selected_channel,
        dragging: curve.dragging,
        enabled,
        version,
    };
    let action = curve.action.clone();
    let parameter = channel.parameter.clone();
    let widget = column![
        lightwell_ui::label(curve.label.clone()),
        curve_editor(&model, move |event| Message::ControlCurve {
            action: action.clone(),
            parameter: parameter.clone(),
            event,
        })
    ]
    .spacing(4.0);
    with_control_menu(widget.into(), &curve.action, Some(&channel.parameter), menu)
}

fn group_view<'a>(
    module_id: &str,
    enabled: bool,
    group: &'a GroupControl,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    let header = sub_group_header(
        &SubGroupHeaderModel {
            label: group.label.clone(),
            state: group.state.map(|state| state.caption().to_owned()),
            reset: group.reset.is_some(),
            enabled,
        },
        Message::ResetGroup {
            module_id: module_id.to_owned(),
            path: group.path.clone(),
        },
    );
    let header = if let Some(reset) = &group.reset {
        let module_id = module_id.to_owned();
        let path = group.path.clone();
        let header = focus_control(header, enabled, move |event| {
            activates(event).then(|| Message::ResetGroup {
                module_id: module_id.clone(),
                path: path.clone(),
            })
        });
        with_control_menu_preset(header, &reset.action, None, Some(&reset.preset), menu)
    } else {
        header
    };
    let indent = iced::Padding::default().left(theme::SPACING);
    let mut inner = column![].spacing(theme::SPACING).padding(indent);
    if group.expanded {
        for control in &group.controls {
            inner = inner.push(control_view(module_id, enabled, control, menu, plot));
        }
    }
    let toggle = icon_button(
        &IconButtonModel {
            icon: if group.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            tooltip: format!(
                "{} {}",
                if group.expanded { "Collapse" } else { "Expand" },
                group.label
            ),
            enabled,
            selected: false,
        },
        enabled.then_some(Message::ToggleGroup {
            module_id: module_id.to_owned(),
            path: group.path.clone(),
        }),
    );
    let toggle = focus_control(toggle, enabled, {
        let module_id = module_id.to_owned();
        let path = group.path.clone();
        move |event| {
            activates(event).then(|| Message::ToggleGroup {
                module_id: module_id.clone(),
                path: path.clone(),
            })
        }
    });
    column![row![toggle, header].align_y(Alignment::Center), inner]
        .spacing(theme::SPACING / 2.0)
        .into()
}

fn action_view<'a>(
    action: &'a ActionControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let press = action.runnable.then(|| Message::RunAction {
        action: action.action.clone(),
        preset: action.preset.clone(),
    });
    let control: Element<'a, Message> = match (
        action.style,
        action.icon.as_deref().and_then(Icon::from_name),
    ) {
        (ActionControlStyle::Icon, Some(icon)) => icon_button(
            &IconButtonModel {
                icon,
                tooltip: action.label.clone(),
                enabled: action.runnable,
                selected: false,
            },
            press,
        ),
        (style, _) => button(lightwell_ui::label(action.label.clone()))
            .padding([4.0, 10.0])
            .style(if style == ActionControlStyle::Primary {
                theme::button_accent
            } else {
                theme::button_plain
            })
            .on_press_maybe(press)
            .into(),
    };
    let action_name = action.action.clone();
    let preset = action.preset.clone();
    let control = focus_control(control, action.runnable, move |event| {
        activates(event).then(|| Message::RunAction {
            action: action_name.clone(),
            preset: preset.clone(),
        })
    });
    let control =
        with_control_menu_preset(control, &action.action, None, Some(&action.preset), menu);
    match &action.reason {
        Some(reason) if !action.runnable => iced::widget::tooltip(
            control,
            iced::widget::container(caption(reason.clone()))
                .padding(6.0)
                .style(theme::bar_surface),
            iced::widget::tooltip::Position::Top,
        )
        .into(),
        _ => control,
    }
}

/// The crop draft's own panel, driven by [`CropMessage`]: the API-equivalent path and this panel
/// share the same state machine.
fn crop_section_view<'a>(
    model: &'a CropSectionModel,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let mut panel = column![lightwell_ui::title(model.title.clone())].spacing(theme::SPACING);
    if !model.drafting {
        panel = panel.push(
            button(lightwell_ui::label("Crop & straighten"))
                .padding([4.0, 10.0])
                .style(theme::button_plain)
                .on_press_maybe(model.can_start.then_some(Message::Crop(CropMessage::Start))),
        );
        if model.pending {
            panel = panel.push(caption("Preparing the crop's input stage…"));
        }
        return panel.into();
    }
    if model.conflicted {
        panel = panel.push(error_caption("Changed elsewhere · Discard or Reapply"));
        panel = panel.push(
            row![
                button(lightwell_ui::label("Discard"))
                    .padding([4.0, 10.0])
                    .style(theme::button_plain)
                    .on_press(Message::Crop(CropMessage::Cancel)),
                button(lightwell_ui::label("Reapply"))
                    .padding([4.0, 10.0])
                    .style(theme::button_accent)
                    .on_press_maybe(
                        model
                            .can_reapply
                            .then_some(Message::Crop(CropMessage::Reapply))
                    ),
            ]
            .spacing(theme::SPACING / 2.0),
        );
    }
    if model.paused {
        panel = panel.push(caption(
            "Draft paused during history preview · Return to current",
        ));
    }
    if !model.presets.is_empty() {
        let chips = model.presets.iter().map(|preset| {
            let control = chip(
                &ChipModel {
                    label: preset.label.clone(),
                    trailing: None,
                    selected: preset.chosen,
                    enabled: model.enabled,
                },
                model
                    .enabled
                    .then_some(Message::Crop(CropMessage::Preset(preset.index))),
                None,
            );
            let index = preset.index;
            focus_control(control, model.enabled, move |event| {
                activates(event).then(|| Message::Crop(CropMessage::Preset(index)))
            })
        });
        panel = panel.push(Row::new().spacing(4.0).extend(chips).wrap());
    }
    panel = panel.push(
        row![
            number_field(
                &NumberFieldModel {
                    label: "W".into(),
                    display: model.custom.0.clone(),
                    edit: lightwell_ui::ValueEdit::Editing {
                        text: model.custom.0.clone(),
                        invalid: None
                    },
                    unit: None,
                    enabled: model.enabled,
                    id: Some(model.custom_ids.0.clone()),
                },
                Message::Crop(CropMessage::CustomWidth(model.custom.0.clone())),
                |text| Message::Crop(CropMessage::CustomWidth(text)),
                Message::Crop(CropMessage::CustomWidth(model.custom.0.clone())),
                Message::Crop(CropMessage::CustomWidth(model.custom.0.clone()))
            ),
            number_field(
                &NumberFieldModel {
                    label: "H".into(),
                    display: model.custom.1.clone(),
                    edit: lightwell_ui::ValueEdit::Editing {
                        text: model.custom.1.clone(),
                        invalid: None
                    },
                    unit: None,
                    enabled: model.enabled,
                    id: Some(model.custom_ids.1.clone()),
                },
                Message::Crop(CropMessage::CustomHeight(model.custom.1.clone())),
                |text| Message::Crop(CropMessage::CustomHeight(text)),
                Message::Crop(CropMessage::CustomHeight(model.custom.1.clone())),
                Message::Crop(CropMessage::CustomHeight(model.custom.1.clone()))
            ),
            button(lightwell_ui::label(model.lock_label.clone()))
                .padding([4.0, 10.0])
                .style(theme::button_plain)
                .on_press_maybe(model.enabled.then_some(Message::Crop(CropMessage::Lock))),
            button(lightwell_ui::label("Swap"))
                .padding([4.0, 10.0])
                .style(theme::button_plain)
                .on_press_maybe(model.can_swap.then_some(Message::Crop(CropMessage::Swap))),
        ]
        .spacing(theme::SPACING / 2.0)
        .align_y(Alignment::Center),
    );
    panel = panel.push(
        row![
            focus_control(
                stepper(
                    &StepperModel {
                        field: NumberFieldModel {
                            label: "Angle".into(),
                            display: model.angle.clone(),
                            edit: lightwell_ui::ValueEdit::Editing {
                                text: model.angle.clone(),
                                invalid: None
                            },
                            unit: Some("°".into()),
                            enabled: model.enabled,
                            id: Some(model.angle_id.clone()),
                        },
                        decrement_enabled: model.enabled,
                        increment_enabled: model.enabled,
                        decrement_tooltip: format!("−{}°", model.nudge),
                        increment_tooltip: format!("+{}°", model.nudge),
                    },
                    Message::Crop(CropMessage::NudgeAngle(-model.nudge)),
                    Message::Crop(CropMessage::NudgeAngle(model.nudge)),
                    Message::Crop(CropMessage::AngleText(model.angle.clone())),
                    |text| Message::Crop(CropMessage::AngleText(text)),
                    Message::Crop(CropMessage::SubmitAngle),
                    Message::Crop(CropMessage::AngleText("0".into()))
                ),
                model.enabled,
                {
                    let step = model.nudge;
                    move |event| match event {
                        ControlKeyEvent::Pressed { key, shift, option } => {
                            key_direction(key).map(|direction| {
                                let factor = if shift {
                                    10.0
                                } else if option {
                                    0.1
                                } else {
                                    1.0
                                };
                                Message::Crop(CropMessage::NudgeAngle(
                                    f64::from(direction) * step * factor,
                                ))
                            })
                        }
                        _ => None,
                    }
                }
            ),
            straighten_toggle(model),
        ]
        .spacing(theme::SPACING / 2.0)
        .align_y(Alignment::Center),
    );
    let apply = button(lightwell_ui::label("Apply"))
        .padding([4.0, 10.0])
        .style(theme::button_accent)
        .on_press_maybe(model.can_apply.then_some(Message::Crop(CropMessage::Apply)));
    let apply: Element<'a, Message> = mouse_area(apply)
        .on_right_press(Message::OpenMenu(MenuTarget::Draft))
        .into();
    panel = panel.push(
        row![
            button(lightwell_ui::label("Cancel"))
                .padding([4.0, 10.0])
                .style(theme::button_plain)
                .on_press(Message::Crop(CropMessage::Cancel)),
            apply,
        ]
        .spacing(theme::SPACING / 2.0),
    );
    if matches!(menu, Some(MenuTarget::Draft)) {
        panel = panel.push(inline_menu(vec![
            ("Copy as JSON request".to_owned(), Message::CopyDraftRequest),
            ("Cancel".to_owned(), Message::CloseMenu),
        ]));
    }
    for line in &model.readout {
        panel = panel.push(caption(line.clone()));
    }
    panel.into()
}

fn straighten_toggle(model: &CropSectionModel) -> Element<'_, Message> {
    let control = toggle(
        &ToggleModel {
            label: "Straighten guide".into(),
            on: model.guide,
            enabled: model.enabled,
        },
        |on| Message::Crop(CropMessage::Guide(on)),
    );
    let on = model.guide;
    focus_control(control, model.enabled, move |event| {
        activates(event).then(|| Message::Crop(CropMessage::Guide(!on)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::histogram::RenderIdentity;

    /// The plot's cache key changes exactly when the bins or the dimming would: a new render
    /// identity or a toggled `stale` flag. It holds steady across everything else a re-derive
    /// touches (the readout, the counters, the triangles), which is what lets the histogram widget
    /// skip re-tessellating its polygons on a redraw the periodic desktop sync causes but nothing
    /// visible changed.
    #[test]
    fn the_plot_version_moves_only_with_the_bins_or_the_stale_flag() {
        let identity = RenderIdentity {
            entry: "entry-1".into(),
            draft_revision: None,
            generation: 4,
            width: 2,
            height: 2,
        };
        let base = HistogramModel {
            identity: Some(identity.clone()),
            stale: false,
            ..HistogramModel::default()
        };
        // A pointer move re-derives the readout only: same identity, same stale, same version.
        let moved_pointer = HistogramModel {
            readout: Some("R 1 \u{b7} G 2 \u{b7} B 3 \u{b7} 4, 5".into()),
            ..base.clone()
        };
        assert_eq!(plot_version(&base), plot_version(&moved_pointer));
        // A newer generation is a different render: the version moves.
        let newer = HistogramModel {
            identity: Some(RenderIdentity {
                generation: 5,
                ..identity.clone()
            }),
            ..base.clone()
        };
        assert_ne!(plot_version(&base), plot_version(&newer));
        // Going stale dims every fill's colour, which is baked into the tessellated geometry, so it
        // has to move the version too even though the bins themselves have not changed yet.
        let gone_stale = HistogramModel {
            stale: true,
            ..base.clone()
        };
        assert_ne!(plot_version(&base), plot_version(&gone_stale));
        // No report at all still hashes consistently rather than panicking or colliding by luck
        // with a populated identity.
        assert_eq!(
            plot_version(&HistogramModel::default()),
            plot_version(&HistogramModel::default())
        );
    }

    #[test]
    fn curve_background_cache_key_tracks_histogram_generation() {
        let base = HistogramModel {
            identity: Some(RenderIdentity {
                entry: "entry-1".into(),
                draft_revision: None,
                generation: 4,
                width: 2,
                height: 2,
            }),
            ..HistogramModel::default()
        };
        let newer = HistogramModel {
            identity: Some(RenderIdentity {
                generation: 5,
                ..base.identity.clone().unwrap()
            }),
            ..base.clone()
        };
        assert_ne!(
            curve_version(7, true, &base),
            curve_version(7, true, &newer)
        );
        assert_eq!(
            curve_version(7, false, &base),
            curve_version(7, false, &newer)
        );
    }
}
