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
        histogram::{HIGHLIGHT_GLYPH, HIGHLIGHT_RULE, HistogramModel, SHADOW_GLYPH, SHADOW_RULE},
        tools::{
            ActionControl, ColorControl, ControlModel, CropSectionModel, EnumControl, GroupControl,
            PickerControl, SectionModel, SliderControl, ToolsModel, ValueEdit,
        },
    },
};
use iced::{
    Alignment, Element, Length,
    widget::{Row, button, column, mouse_area, row, scrollable, text_input},
};
use lightwell_ui::{
    BINS, ChipModel, ClipTriangleModel, HistogramChannel, IconButtonModel, SectionHeaderModel,
    SegmentedModel, SliderModel, SubGroupHeaderModel, caption, chip, clip_triangle, error_caption,
    histogram, icon_button, inline_menu, section_header, section_label, segmented, slider,
    sub_group_header, theme,
};

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
        .height(Length::Fill)
        .into();
    }
    let menu = model.menu.as_ref();
    let mut panel = column![]
        .spacing(theme::SPACING)
        .padding(theme::SPACING)
        .width(Length::Fill);
    // The histogram sits above the first module section with no header of its own, as the Develop
    // workspace layout reserves.
    panel = panel.push(inspector(plot));
    for section in &model.sections {
        panel = panel.push(section_view(section, menu));
    }
    if !model.developer.is_empty() {
        panel = panel.push(section_label("Developer"));
        for section in &model.developer {
            panel = panel.push(section_view(section, menu));
        }
    }
    scrollable(panel).height(Length::Fill).into()
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
                glyph: SHADOW_GLYPH.into(),
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
                glyph: HIGHLIGHT_GLYPH.into(),
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

fn section_view<'a>(
    section: &'a SectionModel,
    menu: Option<&'a MenuTarget>,
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
) -> Element<'a, Message> {
    match control {
        ControlModel::Slider(field) => slider_view(enabled, field, menu),
        ControlModel::Enum(choice) => enum_view(enabled, choice, menu),
        ControlModel::Color(color) => color_view(enabled, color, menu),
        ControlModel::Group(group) => group_view(module_id, enabled, group, menu),
        ControlModel::Action(action) => action_view(action, menu),
        ControlModel::Picker(picker) => picker_view(picker, menu),
        ControlModel::Unsupported(message) => error_caption(message.clone()),
        ControlModel::CropFrame(frame) => crop_section_view(frame, menu),
    }
}

/// Whether this control is the one whose context menu is open. A patch action's controls are one
/// per field, so the field is part of the identity.
fn menu_open_for(menu: Option<&MenuTarget>, action: &str, parameter: Option<&str>) -> bool {
    matches!(
        menu,
        Some(MenuTarget::Control { action: open, parameter: named })
            if open == action && named.as_deref() == parameter
    )
}

/// The control's own context-menu target.
fn control_target(action: &str, parameter: Option<&str>) -> MenuTarget {
    MenuTarget::Control {
        action: action.to_owned(),
        parameter: parameter.map(str::to_owned),
    }
}

/// The "Copy as JSON request" / "Cancel" menu a generated control's context menu opens, for the
/// exact `edit.<action>` request its current values would send.
fn control_menu(action: &str, parameter: Option<&str>) -> Element<'static, Message> {
    inline_menu(vec![
        (
            "Copy as JSON request".to_owned(),
            Message::CopyRequest {
                action: action.to_owned(),
                parameter: parameter.map(str::to_owned),
            },
        ),
        ("Cancel".to_owned(), Message::CloseMenu),
    ])
}

/// Wrap a generated control so a right-click on it opens its own context menu, and append the
/// menu itself directly under the control when it is the one currently open.
fn with_control_menu<'a>(
    control: Element<'a, Message>,
    action: &str,
    parameter: Option<&str>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let area: Element<'a, Message> = mouse_area(control)
        .on_right_press(Message::OpenMenu(control_target(action, parameter)))
        .into();
    if menu_open_for(menu, action, parameter) {
        column![area, control_menu(action, parameter)]
            .spacing(4.0)
            .into()
    } else {
        area
    }
}

/// The generic step for a non-integer slider: a fraction of its range, rounded to a power of ten.
fn generic_step(min: f64, max: f64) -> f64 {
    let span = (max - min).abs();
    if !span.is_finite() || span <= 0.0 {
        return 0.01;
    }
    10f64.powf((span / 200.0).log10().round())
}

fn slider_view<'a>(
    enabled: bool,
    field: &'a SliderControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let step = if field.integer {
        1.0
    } else {
        generic_step(field.min, field.max)
    };
    let zero = (field.min < 0.0 && field.max > 0.0).then_some(field.zero);
    // `ValueEdit::text` is the one place that resolves "what a field shows right now" (what was
    // typed, else the formatted value); the widget's own `Editing`/`Display` split follows it.
    let edit = match &field.edit {
        ValueEdit::Typing(_) => lightwell_ui::ValueEdit::Editing {
            text: field.edit.text(&field.display).to_owned(),
            invalid: field.invalid.clone(),
        },
        ValueEdit::None => lightwell_ui::ValueEdit::Display,
    };
    let (action, parameter) = (field.action.clone(), field.parameter.clone());
    let (change_action, change_parameter) = (action.clone(), parameter.clone());
    let (text_action, text_parameter) = (action.clone(), parameter.clone());
    let (release_action, release_parameter) = (action.clone(), parameter.clone());
    let (submit_action, submit_parameter) = (action.clone(), parameter.clone());
    let reset_message = Message::ResetField { action, parameter };
    let control: Element<'a, Message> = slider(
        &SliderModel {
            label: field.label.clone(),
            min: field.min,
            max: field.max,
            value: field.value,
            step,
            shift_step: step * 10.0,
            zero,
            unit: field.unit.clone(),
            display: field.display.clone(),
            edit,
            dragging: field.dragging,
            enabled,
        },
        move |value| Message::SliderMoved {
            action: change_action.clone(),
            parameter: change_parameter.clone(),
            value,
        },
        Message::SliderReleased {
            action: release_action,
            parameter: release_parameter,
        },
        Message::EditValue {
            action: text_action.clone(),
            parameter: text_parameter.clone(),
        },
        move |text| Message::Field {
            action: text_action.clone(),
            parameter: text_parameter.clone(),
            text,
        },
        Message::Submit {
            action: submit_action,
            parameter: Some(submit_parameter),
        },
        reset_message,
    );
    with_control_menu(control, &field.action, Some(&field.parameter), menu)
}

fn enum_view<'a>(
    enabled: bool,
    choice: &'a EnumControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let label = mouse_area(lightwell_ui::label(choice.label.clone())).on_right_press(
        Message::OpenMenu(control_target(&choice.action, Some(&choice.parameter))),
    );
    let mut field = column![label].spacing(4.0);
    let (action, parameter) = (choice.action.clone(), choice.parameter.clone());
    if choice.segmented {
        let options = choice.options.clone();
        field = field.push(segmented(
            &SegmentedModel {
                options: choice.options.clone(),
                selected: choice.selected.unwrap_or(0),
                enabled,
            },
            move |index| Message::Field {
                action: action.clone(),
                parameter: parameter.clone(),
                text: options[index].clone(),
            },
        ));
    } else {
        let chips = choice.options.iter().enumerate().map(|(index, option)| {
            chip(
                &ChipModel {
                    label: option.clone(),
                    trailing: None,
                    selected: choice.selected == Some(index),
                    enabled,
                },
                Some(Message::Field {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    text: option.clone(),
                }),
                None,
            )
        });
        field = field.push(Row::new().spacing(4.0).extend(chips).wrap());
    }
    if menu_open_for(menu, &choice.action, Some(&choice.parameter)) {
        field = field.push(control_menu(&choice.action, Some(&choice.parameter)));
    }
    field.into()
}

fn color_view<'a>(
    enabled: bool,
    color: &'a ColorControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let label = mouse_area(lightwell_ui::label(color.label.clone())).on_right_press(
        Message::OpenMenu(control_target(&color.action, Some(&color.parameter))),
    );
    let mut channels = row![label].spacing(4.0).align_y(Alignment::Center);
    for (index, value) in color.channels.iter().enumerate() {
        let (action, parameter, current) = (
            color.action.clone(),
            color.parameter.clone(),
            color.text.clone(),
        );
        channels = channels.push(
            text_input(fields::CHANNELS[index], value)
                .id(color.ids[index].clone())
                .style(theme::text_input_style(color.invalid.is_some()))
                .size(theme::SIZE_CONTROL)
                .width(Length::Fixed(48.0))
                .on_input_maybe(enabled.then_some(move |text: String| Message::Field {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    text: fields::replace_channel(&current, index, &text),
                }))
                .on_submit(Message::Submit {
                    action: color.action.clone(),
                    parameter: Some(color.parameter.clone()),
                }),
        );
    }
    let mut field = column![channels].spacing(4.0);
    if let Some(message) = &color.invalid {
        field = field.push(error_caption(message.clone()));
    }
    if menu_open_for(menu, &color.action, Some(&color.parameter)) {
        field = field.push(control_menu(&color.action, Some(&color.parameter)));
    }
    field.into()
}

fn group_view<'a>(
    module_id: &str,
    enabled: bool,
    group: &'a GroupControl,
    menu: Option<&'a MenuTarget>,
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
    let indent = iced::Padding::default().left(theme::SPACING);
    let mut inner = column![].spacing(theme::SPACING).padding(indent);
    for control in &group.controls {
        inner = inner.push(control_view(module_id, enabled, control, menu));
    }
    column![header, inner].spacing(theme::SPACING / 2.0).into()
}

fn action_view<'a>(
    action: &'a ActionControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let control = button(lightwell_ui::label(action.label.clone()))
        .padding([4.0, 10.0])
        .style(theme::button_plain)
        .on_press_maybe(action.runnable.then(|| Message::RunAction {
            action: action.action.clone(),
            preset: action.preset.clone(),
        }));
    let control = with_control_menu(control.into(), &action.action, None, menu);
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

/// A module's picker: the button that enters that module's canvas pick mode, drawn beside the
/// controls the pick fills. It reads selected while the mode is active and clicking it then returns
/// to the pointer, so the mode is always leavable from the same place it was entered. It commits
/// nothing: the gesture is one `workspace.set`, which is what its context menu copies.
fn picker_view<'a>(
    picker: &'a PickerControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let style = if picker.selected {
        theme::button_selected
    } else {
        theme::button_plain
    };
    let control = button(lightwell_ui::label(picker.label.clone()))
        .padding([4.0, 10.0])
        .style(style)
        .on_press_maybe(
            picker
                .enabled
                .then(|| Message::SetMode(picker.target.clone())),
        );
    // The mode strip named the mode and its letter in a tooltip; the panel says the same thing.
    let control: Element<'a, Message> = match &picker.shortcut {
        Some(key) => iced::widget::tooltip(
            control,
            iced::widget::container(caption(format!("{} \u{00b7} {key}", picker.title)))
                .padding(6.0)
                .style(theme::bar_surface),
            iced::widget::tooltip::Position::Top,
        )
        .into(),
        None => control.into(),
    };
    let target = MenuTarget::Mode(picker.module_id.clone());
    let area: Element<'a, Message> = mouse_area(control)
        .on_right_press(Message::OpenMenu(target.clone()))
        .into();
    if menu == Some(&target) {
        column![
            area,
            inline_menu(vec![
                (
                    "Copy as JSON request".to_owned(),
                    Message::CopyModeRequest(picker.module_id.clone()),
                ),
                ("Cancel".to_owned(), Message::CloseMenu),
            ])
        ]
        .spacing(4.0)
        .into()
    } else {
        area
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
            chip(
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
            )
        });
        panel = panel.push(Row::new().spacing(4.0).extend(chips).wrap());
    }
    panel = panel.push(
        row![
            text_input("W", &model.custom.0)
                .id(model.custom_ids.0.clone())
                .style(theme::text_input_style(false))
                .size(theme::SIZE_CONTROL)
                .width(Length::Fixed(48.0))
                .on_input(|text| Message::Crop(CropMessage::CustomWidth(text))),
            text_input("H", &model.custom.1)
                .id(model.custom_ids.1.clone())
                .style(theme::text_input_style(false))
                .size(theme::SIZE_CONTROL)
                .width(Length::Fixed(48.0))
                .on_input(|text| Message::Crop(CropMessage::CustomHeight(text))),
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
            icon_button(
                &IconButtonModel {
                    glyph: "\u{2212}".into(),
                    tooltip: format!("−{}°", model.nudge),
                    enabled: model.enabled,
                    selected: false,
                },
                model
                    .enabled
                    .then_some(Message::Crop(CropMessage::NudgeAngle(-model.nudge))),
            ),
            text_input("0", &model.angle)
                .id(model.angle_id.clone())
                .style(theme::text_input_style(false))
                .size(theme::SIZE_CONTROL)
                .width(Length::Fixed(60.0))
                .on_input(|text| Message::Crop(CropMessage::AngleText(text)))
                .on_submit(Message::Crop(CropMessage::SubmitAngle)),
            icon_button(
                &IconButtonModel {
                    glyph: "+".into(),
                    tooltip: format!("+{}°", model.nudge),
                    enabled: model.enabled,
                    selected: false,
                },
                model
                    .enabled
                    .then_some(Message::Crop(CropMessage::NudgeAngle(model.nudge))),
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
    let style = if model.guide {
        theme::button_selected
    } else {
        theme::button_plain
    };
    button(lightwell_ui::label("Straighten guide"))
        .padding([4.0, 10.0])
        .style(style)
        .on_press_maybe(
            model
                .enabled
                .then_some(Message::Crop(CropMessage::Guide(!model.guide))),
        )
        .into()
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
}
