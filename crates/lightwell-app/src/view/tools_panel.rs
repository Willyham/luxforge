//! The tools panel: one block per registered module, every control drawn from its model. The view
//! knows no tool and no parameter limit; it draws what the section says and publishes messages.
use crate::{
    app::message::{CropMessage, Message},
    state::tools::{
        ColorControl, ControlModel, CropSectionModel, GroupControl, SectionModel, SliderControl,
        ToolsModel,
    },
};
use iced::{
    Alignment, Element,
    widget::{Column, button, column, row, text, text_input},
};

/// Ratio presets per row in the sidebar.
const PRESETS_PER_ROW: usize = 3;

pub(crate) fn tools_panel(model: &ToolsModel) -> Element<'_, Message> {
    if let Some(message) = model.status.message() {
        return text(message).size(14).into();
    }
    let mut panel = column![].spacing(18);
    for section in model.all() {
        panel = panel.push(block(section));
    }
    panel.into()
}

/// One module's block. The plain arrangement has no header: an unavailable module names its reason
/// and every available one shows its controls directly.
fn block(section: &SectionModel) -> Element<'_, Message> {
    let mut block = column![].spacing(18);
    if let Some(reason) = &section.unavailable {
        block = block.push(text(format!("{} · unavailable: {reason}", section.title)).size(14));
    }
    if !section.expanded {
        return block.into();
    }
    for control in &section.controls {
        block = block.push(element(control));
    }
    block.into()
}

fn element(control: &ControlModel) -> Element<'_, Message> {
    match control {
        ControlModel::Group(group) => group_element(group),
        ControlModel::Slider(slider) => slider_element(slider),
        ControlModel::Color(color) => color_element(color),
        ControlModel::Enum(choice) => {
            // No registered module declares an enum control yet; its values are still reachable.
            let mut options = row![text(&choice.label).size(12).width(110)]
                .spacing(6)
                .align_y(Alignment::Center);
            for (index, option) in choice.options.iter().enumerate() {
                let label = if choice.selected == Some(index) {
                    format!("● {option}")
                } else {
                    option.clone()
                };
                let (action, parameter, text_value) = (
                    choice.action.clone(),
                    choice.parameter.clone(),
                    option.clone(),
                );
                options = options.push(button(text(label).size(12)).on_press(Message::Field {
                    action,
                    parameter,
                    text: text_value,
                }));
            }
            options.into()
        }
        ControlModel::Action(action) => button(text(&action.label))
            .on_press_maybe(action.runnable.then(|| Message::RunAction {
                action: action.action.clone(),
                preset: action.preset.clone(),
            }))
            .into(),
        ControlModel::Unsupported(message) => text(message).size(12).into(),
        ControlModel::CropFrame(frame) => crop_section(frame),
    }
}

fn group_element(group: &GroupControl) -> Element<'_, Message> {
    let mut children: Vec<Element<'_, Message>> = vec![text(&group.label).size(18).into()];
    children.extend(group.controls.iter().map(element));
    Column::with_children(children).spacing(8).into()
}

fn slider_element(slider: &SliderControl) -> Element<'_, Message> {
    let (action, parameter) = (slider.action.clone(), slider.parameter.clone());
    let input = text_input(&slider.label, slider.edit.text(&slider.display))
        .id(slider.id.clone())
        .on_input(move |text| Message::Field {
            action: action.clone(),
            parameter: parameter.clone(),
            text,
        })
        .on_submit(Message::Submit {
            action: slider.action.clone(),
        })
        .width(90);
    let mut field = column![
        row![text(&slider.label).size(12).width(110), input]
            .spacing(6)
            .align_y(Alignment::Center)
    ]
    .spacing(4);
    if let Some(message) = &slider.invalid {
        field = field.push(text(message).size(11));
    }
    field.into()
}

fn color_element(color: &ColorControl) -> Element<'_, Message> {
    let mut channels = row![text(&color.label).size(12).width(110)]
        .spacing(6)
        .align_y(Alignment::Center);
    for (index, value) in color.channels.iter().enumerate() {
        let (action, parameter, current) = (
            color.action.clone(),
            color.parameter.clone(),
            color.text.clone(),
        );
        channels = channels.push(
            text_input(crate::app::fields::CHANNELS[index], value)
                .id(color.ids[index].clone())
                .on_input(move |text| Message::Field {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    text: crate::app::fields::replace_channel(&current, index, &text),
                })
                .on_submit(Message::Submit {
                    action: color.action.clone(),
                })
                .width(55),
        );
    }
    let mut field = column![channels].spacing(4);
    if let Some(message) = &color.invalid {
        field = field.push(text(message).size(11));
    }
    field.into()
}

/// The crop draft's own controls. Every one of them is a [`CropMessage`], so the API-equivalent
/// path and this panel drive the same state machine.
fn crop_section(model: &CropSectionModel) -> Element<'_, Message> {
    let mut panel = column![text(&model.title).size(18)].spacing(8);
    if !model.drafting {
        panel = panel.push(
            button("Crop")
                .on_press_maybe(model.can_start.then_some(Message::Crop(CropMessage::Start))),
        );
        if model.pending {
            panel = panel.push(text("Preparing the crop's input stage…").size(12));
        }
        return panel.into();
    }
    if model.conflicted {
        panel = panel.push(text("Changed elsewhere: Discard or Reapply").size(12));
        panel = panel.push(
            row![
                button("Discard").on_press(Message::Crop(CropMessage::Cancel)),
                button("Reapply").on_press_maybe(
                    model
                        .can_reapply
                        .then_some(Message::Crop(CropMessage::Reapply))
                ),
            ]
            .spacing(6),
        );
    }
    if model.paused {
        panel =
            panel.push(text("Draft paused during history preview · Return to current").size(12));
    }
    for chunk in model.presets.chunks(PRESETS_PER_ROW) {
        let mut buttons = row![].spacing(6);
        for preset in chunk {
            let label = if preset.chosen {
                format!("● {}", preset.label)
            } else {
                preset.label.clone()
            };
            buttons = buttons.push(
                button(text(label).size(12)).on_press_maybe(
                    model
                        .enabled
                        .then_some(Message::Crop(CropMessage::Preset(preset.index))),
                ),
            );
        }
        panel = panel.push(buttons);
    }
    panel = panel.push(
        row![
            text("Custom").size(12).width(56),
            text_input("W", &model.custom.0)
                .id(model.custom_ids.0.clone())
                .on_input(|text| Message::Crop(CropMessage::CustomWidth(text)))
                .width(48),
            text_input("H", &model.custom.1)
                .id(model.custom_ids.1.clone())
                .on_input(|text| Message::Crop(CropMessage::CustomHeight(text)))
                .width(48),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    );
    panel = panel.push(
        row![
            button(text(&model.lock_label).size(12))
                .on_press_maybe(model.enabled.then_some(Message::Crop(CropMessage::Lock))),
            button(text("Swap").size(12))
                .on_press_maybe(model.can_swap.then_some(Message::Crop(CropMessage::Swap))),
        ]
        .spacing(6),
    );
    panel = panel.push(
        row![
            text("Angle (deg)").size(12).width(78),
            text_input("0", &model.angle)
                .id(model.angle_id.clone())
                .on_input(|text| Message::Crop(CropMessage::AngleText(text)))
                .on_submit(Message::Crop(CropMessage::SubmitAngle))
                .width(60),
            button(text(format!("−{}°", model.nudge)).size(12)).on_press_maybe(
                model
                    .enabled
                    .then_some(Message::Crop(CropMessage::NudgeAngle(-model.nudge)))
            ),
            button(text(format!("+{}°", model.nudge)).size(12)).on_press_maybe(
                model
                    .enabled
                    .then_some(Message::Crop(CropMessage::NudgeAngle(model.nudge)))
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    );
    let guide = if model.guide {
        "Straighten guide: on"
    } else {
        "Straighten guide: off"
    };
    panel = panel.push(
        button(text(guide).size(12)).on_press(Message::Crop(CropMessage::Guide(!model.guide))),
    );
    panel = panel.push(
        row![
            button("Apply")
                .on_press_maybe(model.can_apply.then_some(Message::Crop(CropMessage::Apply))),
            button("Cancel").on_press(Message::Crop(CropMessage::Cancel)),
        ]
        .spacing(6),
    );
    for line in &model.readout {
        panel = panel.push(text(line).size(11));
    }
    panel = panel.push(
        text("Drag handles to resize, inside to move, Option for one scale about the centre, Space to pan. Enter applies, Escape cancels.")
            .size(11),
    );
    panel.into()
}
