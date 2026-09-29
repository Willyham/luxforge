//! A module's capability block and task control, drawn from [`CapabilityModel`] and
//! [`TaskControl`]. The view knows no module and no setting: it lays out what the model says, with
//! the widget library's control kinds, and publishes the one [`CapabilityMessage`] each control
//! stands for.
use crate::state::MenuTarget;
use crate::{
    app::message::{Message, capability::CapabilityMessage, view::ViewMessage},
    state::capabilities::{
        CapabilityModel, FieldKindModel, FieldModel, ResourceAction, ResourceRowModel, TaskControl,
        TaskControlState,
    },
};
use iced::{
    Alignment, Element, Length,
    widget::{Column, Space, button, container, mouse_area, row},
};
use luxforge_ui::{
    NumberFieldModel, SegmentedModel, ToggleModel, ValueEdit, caption, error_caption, inline_menu,
    label, number_field, segmented, theme, toggle, value_input,
};
use serde_json::Value;

fn capability(message: CapabilityMessage) -> Message {
    Message::Capability(message)
}

/// A small text button in the panel's plain or accent style.
fn text_button<'a>(text: &str, accent: bool, press: Option<Message>) -> Element<'a, Message> {
    button(label(text.to_owned()))
        .padding([4.0, 10.0])
        .style(if accent {
            theme::button_accent
        } else {
            theme::button_plain
        })
        .on_press_maybe(press)
        .into()
}

fn fill<'a>() -> Element<'a, Message> {
    Space::new().width(Length::Fill).into()
}

/// The block above a capability module's controls: a row per resource, the settings form, the
/// permission counts with Revoke all, and the one status line.
pub(crate) fn block(model: &CapabilityModel) -> Element<'_, Message> {
    if model.loading {
        return caption("Reading the module's settings and status…");
    }
    let module_id = &model.module_id;
    let mut body = Column::new().spacing(theme::SPACING / 2.0);
    for resource in &model.resources {
        body = body.push(resource_view(module_id, resource, model.enabled));
    }
    for field in &model.fields {
        body = body.push(field_view(module_id, field, model.enabled));
    }
    body = body.push(
        row![
            caption(model.permissions.clone()),
            fill(),
            text_button(
                "Revoke all",
                false,
                (model.enabled && model.revoke_all)
                    .then(|| capability(CapabilityMessage::RevokeAll(module_id.clone()))),
            ),
        ]
        .align_y(Alignment::Center),
    );
    if let Some(line) = &model.status_line {
        body = body.push(error_caption(line.clone()));
    }
    container(body)
        .padding(theme::SPACING)
        .style(theme::bar_surface)
        .width(Length::Fill)
        .into()
}

fn resource_view<'a>(
    module_id: &str,
    resource: &ResourceRowModel,
    enabled: bool,
) -> Element<'a, Message> {
    let (text, message) = match resource.action {
        ResourceAction::Download => (
            "Download",
            Some(CapabilityMessage::Install {
                module_id: module_id.to_owned(),
                resource: resource.id.clone(),
            }),
        ),
        ResourceAction::Remove => (
            "Remove",
            Some(CapabilityMessage::Remove {
                module_id: module_id.to_owned(),
                resource: resource.id.clone(),
            }),
        ),
        ResourceAction::Cancel => (
            "Cancel",
            resource.job.clone().map(|job| CapabilityMessage::Cancel {
                module_id: module_id.to_owned(),
                job,
            }),
        ),
    };
    // Cancel stops work already running, so it stays pressable while other requests are out.
    let press = message
        .filter(|_| enabled || resource.action == ResourceAction::Cancel)
        .map(capability);
    row![
        label(resource.title.clone()),
        caption(format!("{} · {}", resource.detail, resource.state)),
        fill(),
        text_button(text, false, press),
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center)
    .into()
}

/// One setting, drawn as the tools panel draws a control of its kind. A typed field commits on
/// Enter, a toggle or a choice at once.
fn field_view<'a>(module_id: &str, field: &'a FieldModel, enabled: bool) -> Element<'a, Message> {
    let text = {
        let (module, name) = (module_id.to_owned(), field.field.clone());
        move |text| {
            capability(CapabilityMessage::FieldText {
                module_id: module.clone(),
                field: name.clone(),
                text,
            })
        }
    };
    let commit = capability(CapabilityMessage::FieldCommit {
        module_id: module_id.to_owned(),
        field: field.field.clone(),
    });
    let value = {
        let (module, name) = (module_id.to_owned(), field.field.clone());
        move |value: Value| {
            capability(CapabilityMessage::FieldValue {
                module_id: module.clone(),
                field: name.clone(),
                value,
            })
        }
    };
    match &field.kind {
        FieldKindModel::Number { display, typing } => number_field(
            &NumberFieldModel {
                id: Some(field.id.clone()),
                label: field.label.clone(),
                display: display.clone(),
                edit: match typing {
                    Some(typed) => ValueEdit::Editing {
                        text: typed.clone(),
                        invalid: None,
                    },
                    None => ValueEdit::Display,
                },
                unit: None,
                enabled,
            },
            // Clicking the value starts typing over the value it shows.
            text(display.clone()),
            text,
            commit,
            // A double-click on the label returns the setting to its default.
            value(Value::Null),
        ),
        FieldKindModel::Toggle { on } => toggle(
            &ToggleModel {
                label: field.label.clone(),
                on: *on,
                enabled,
            },
            move |on| value(Value::Bool(on)),
        ),
        FieldKindModel::Choice { options, selected } => {
            let options = options.clone();
            Column::new()
                .spacing(2.0)
                .push(label(field.label.clone()))
                .push(segmented(
                    &SegmentedModel {
                        options: options.clone(),
                        selected: selected.unwrap_or(usize::MAX),
                        enabled,
                    },
                    move |index| value(Value::from(options[index].clone())),
                ))
                .into()
        }
        FieldKindModel::Text { display, typing } => Column::new()
            .spacing(2.0)
            .push(label(field.label.clone()))
            .push(
                value_input(
                    "Not set",
                    typing.as_deref().unwrap_or(display),
                    false,
                    enabled,
                    text,
                    commit,
                )
                .id(iced::widget::Id::from(field.id.clone()))
                .width(Length::Fill),
            )
            .into(),
    }
}

/// A task control: the button, and the newest run's state with Cancel while it runs and Apply once
/// it has succeeded. A right-click offers the `task.<id>` request the button sends.
pub(crate) fn task_view<'a>(
    task: &'a TaskControl,
    enabled: bool,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let module_id = task.module_id.clone();
    let mut body = Column::new().spacing(theme::SPACING / 2.0);
    let run = text_button(
        &task.label,
        true,
        (enabled && task.runnable).then(|| {
            capability(CapabilityMessage::RunTask {
                module_id: module_id.clone(),
                task: task.task.clone(),
            })
        }),
    );
    let mut buttons = row![run].spacing(theme::SPACING / 2.0);
    match &task.state {
        TaskControlState::Running { job, .. } => {
            buttons = buttons.push(text_button(
                "Cancel",
                false,
                Some(capability(CapabilityMessage::Cancel {
                    module_id: module_id.clone(),
                    job: job.clone(),
                })),
            ));
        }
        TaskControlState::Succeeded { apply: Some(_), .. } => {
            buttons = buttons.push(text_button(
                "Apply",
                false,
                enabled.then(|| {
                    capability(CapabilityMessage::Apply {
                        module_id: module_id.clone(),
                        task: task.task.clone(),
                    })
                }),
            ));
        }
        _ => {}
    }
    let target = MenuTarget::Task {
        module_id: module_id.clone(),
        task: task.task.clone(),
    };
    let area: Element<'a, Message> = mouse_area(buttons)
        .on_right_press(Message::View(ViewMessage::OpenMenu(target.clone())))
        .into();
    body = body.push(area);
    if menu == Some(&target) {
        body = body.push(inline_menu(vec![
            (
                "Copy as JSON request".to_owned(),
                capability(CapabilityMessage::CopyTaskRequest {
                    module_id: module_id.clone(),
                    task: task.task.clone(),
                }),
            ),
            ("Cancel".to_owned(), Message::View(ViewMessage::CloseMenu)),
        ]));
    }
    let line: Option<Element<'a, Message>> = match &task.state {
        TaskControlState::Idle => None,
        TaskControlState::Requesting => Some(caption("Sending…")),
        TaskControlState::Consent => Some(caption("Waiting for your answer above the photo")),
        TaskControlState::Running { text, .. } => Some(caption(text.clone())),
        TaskControlState::Succeeded { summary, .. } => Some(caption(summary.clone())),
        TaskControlState::Failed(message) => Some(error_caption(message.clone())),
    };
    body = body.extend(line);
    // A run in progress already says what the button is waiting for.
    let in_progress = matches!(
        task.state,
        TaskControlState::Requesting | TaskControlState::Consent | TaskControlState::Running { .. }
    );
    if let (Some(reason), false, false) = (&task.reason, task.runnable, in_progress) {
        body = body.push(caption(reason.clone()));
    }
    body.into()
}
