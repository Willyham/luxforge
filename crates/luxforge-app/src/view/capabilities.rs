//! A module's capability block and task control, drawn from [`CapabilityModel`] and
//! [`TaskControl`]. The view knows no module and no setting: it lays out what the model says, with
//! the widget library, and publishes the one [`CapabilityMessage`] each control stands for.
use crate::{
    app::message::{CapabilityMessage, MenuTarget, Message, ViewMessage},
    state::capabilities::{
        ActivationRow, CapabilityModel, CapabilityView, FieldKindModel, FieldModel,
        PermissionsModel, ResourceAction, ResourceRowModel, SettingsModel, TaskControl,
        TaskControlState,
    },
};
use iced::{
    Alignment, Element, Length,
    widget::{Column, Space, button, column, container, mouse_area, row},
};
use luxforge_ui::{
    NumberFieldModel, SegmentedModel, ToggleModel, ValueEdit, caption, error_caption, inline_menu,
    label, number_field, section_label, segmented, theme, toggle, value_input,
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

/// The block above a capability module's controls: its status, or its settings.
pub(crate) fn block(model: &CapabilityModel) -> Element<'_, Message> {
    if model.loading {
        return caption("Reading the module's settings and status…");
    }
    let body = match (&model.settings, model.view) {
        (Some(settings), CapabilityView::Settings) => settings_view(model, settings),
        _ => status_view(model),
    };
    container(body)
        .padding(theme::SPACING)
        .style(theme::bar_surface)
        .width(Length::Fill)
        .into()
}

fn status_view(model: &CapabilityModel) -> Element<'_, Message> {
    let module_id = &model.module_id;
    let mut body = Column::new().spacing(theme::SPACING / 2.0);
    body = body.push(
        row![
            section_label("Status"),
            fill(),
            text_button(
                "Settings",
                false,
                Some(capability(CapabilityMessage::Show {
                    module_id: module_id.clone(),
                    view: CapabilityView::Settings,
                })),
            ),
        ]
        .align_y(Alignment::Center),
    );
    if let Some(activation) = &model.activation {
        body = body.push(activation_view(module_id, activation, model.enabled));
    }
    for resource in &model.resources {
        body = body.push(resource_view(module_id, resource, model.enabled));
    }
    body = body.push(permissions_view(
        module_id,
        &model.permissions,
        model.enabled,
    ));
    if !model.requirements.is_empty() {
        body = body.push(error_caption(format!(
            "Needs {}",
            model.requirements.join(", ")
        )));
    }
    if let Some(message) = &model.message {
        body = body.push(error_caption(message.clone()));
    }
    body.into()
}

fn activation_view<'a>(
    module_id: &str,
    activation: &ActivationRow,
    enabled: bool,
) -> Element<'a, Message> {
    let (text, on) = if activation.activate {
        ("Activate", true)
    } else {
        ("Deactivate", false)
    };
    row![
        label(activation.text.clone()),
        fill(),
        text_button(
            text,
            false,
            enabled.then(|| {
                capability(CapabilityMessage::Activate {
                    module_id: module_id.to_owned(),
                    on,
                })
            }),
        ),
    ]
    .align_y(Alignment::Center)
    .into()
}

fn resource_view<'a>(
    module_id: &str,
    resource: &ResourceRowModel,
    enabled: bool,
) -> Element<'a, Message> {
    let mut actions = row![].spacing(theme::SPACING / 2.0);
    for action in &resource.actions {
        let (text, message) = match action {
            ResourceAction::Download => (
                "Download",
                CapabilityMessage::Install {
                    module_id: module_id.to_owned(),
                    resource: resource.id.clone(),
                },
            ),
            ResourceAction::Remove => (
                "Remove",
                CapabilityMessage::Remove {
                    module_id: module_id.to_owned(),
                    resource: resource.id.clone(),
                },
            ),
            ResourceAction::Cancel => match &resource.job {
                Some(job) => (
                    "Cancel",
                    CapabilityMessage::Cancel {
                        module_id: module_id.to_owned(),
                        job: job.clone(),
                    },
                ),
                None => continue,
            },
        };
        // Cancel stops work already running, so it stays pressable while other requests are out.
        let press = (enabled || *action == ResourceAction::Cancel).then(|| capability(message));
        actions = actions.push(text_button(text, false, press));
    }
    column![
        row![
            label(resource.title.clone()),
            fill(),
            caption(resource.detail.clone())
        ]
        .align_y(Alignment::Center),
        row![caption(resource.state.clone()), fill(), actions].align_y(Alignment::Center),
    ]
    .spacing(2.0)
    .into()
}

fn permissions_view<'a>(
    module_id: &str,
    permissions: &PermissionsModel,
    enabled: bool,
) -> Element<'a, Message> {
    let line = button(label(format!(
        "{} {}",
        if permissions.open { "▾" } else { "▸" },
        permissions.summary
    )))
    .padding([2.0, 0.0])
    .style(theme::button_plain)
    .on_press(capability(CapabilityMessage::TogglePermissions(
        module_id.to_owned(),
    )));
    let mut body = Column::new().spacing(2.0).push(line);
    if permissions.open {
        if permissions.rows.is_empty() {
            body = body.push(caption("Nothing is allowed or declined"));
        }
        for permission in &permissions.rows {
            let trailing: Element<'a, Message> = match &permission.revoke {
                Some(grant) => text_button(
                    "Revoke",
                    false,
                    enabled.then(|| {
                        capability(CapabilityMessage::Revoke {
                            module_id: module_id.to_owned(),
                            grant: grant.clone(),
                        })
                    }),
                ),
                None => caption(permission.state.clone()),
            };
            body = body.push(
                row![
                    container(caption(permission.text.clone())).width(Length::Fill),
                    trailing
                ]
                .spacing(theme::SPACING / 2.0)
                .align_y(Alignment::Center),
            );
        }
    }
    body.into()
}

fn settings_view<'a>(
    model: &'a CapabilityModel,
    settings: &'a SettingsModel,
) -> Element<'a, Message> {
    let module_id = &model.module_id;
    let mut body = Column::new().spacing(theme::SPACING / 2.0);
    body = body.push(
        row![
            section_label("Settings"),
            fill(),
            text_button(
                "Done",
                false,
                Some(capability(CapabilityMessage::Show {
                    module_id: module_id.clone(),
                    view: CapabilityView::Status,
                })),
            ),
        ]
        .align_y(Alignment::Center),
    );
    if let Some(state) = &settings.state {
        body = body.push(error_caption(state.clone()));
    }
    for field in &settings.fields {
        body = body.push(field_view(module_id, field, model.enabled));
    }
    if let Some(profiles_label) = &settings.profiles_label {
        body = body.push(section_label(profiles_label.clone()));
        for profile in &settings.profiles {
            let mut block = Column::new().spacing(theme::SPACING / 2.0).push(
                row![
                    label(profile.title.clone()),
                    fill(),
                    text_button(
                        "Remove",
                        false,
                        model.enabled.then(|| {
                            capability(CapabilityMessage::ProfileRemove {
                                module_id: module_id.clone(),
                                profile: profile.id.clone(),
                            })
                        }),
                    ),
                ]
                .align_y(Alignment::Center),
            );
            for field in &profile.fields {
                block = block.push(field_view(module_id, field, model.enabled));
            }
            body =
                body.push(container(block).padding(iced::Padding::default().left(theme::SPACING)));
        }
        if let Some(add) = &settings.add_profile {
            if add.adapters.len() > 1 {
                let adapters = add.adapters.clone();
                let module = module_id.clone();
                body = body.push(segmented(
                    &SegmentedModel {
                        options: add
                            .adapters
                            .iter()
                            .map(|(_, title)| title.clone())
                            .collect(),
                        selected: add.selected,
                        enabled: model.enabled,
                    },
                    move |index| {
                        capability(CapabilityMessage::ProfileAdapter {
                            module_id: module.clone(),
                            adapter: adapters[index].0.clone(),
                        })
                    },
                ));
            }
            let module = module_id.clone();
            body = body.push(
                row![
                    value_input(
                        "Profile name",
                        &add.label,
                        false,
                        model.enabled,
                        move |label| capability(CapabilityMessage::ProfileLabel {
                            module_id: module.clone(),
                            label,
                        }),
                        capability(CapabilityMessage::ProfileCreate(module_id.clone())),
                    )
                    .width(Length::Fill),
                    text_button(
                        "Add profile",
                        false,
                        (model.enabled && add.can_add).then(|| capability(
                            CapabilityMessage::ProfileCreate(module_id.clone())
                        )),
                    ),
                ]
                .spacing(theme::SPACING / 2.0)
                .align_y(Alignment::Center),
            );
        }
    }
    if let Some(message) = &model.message {
        body = body.push(error_caption(message.clone()));
    }
    body.into()
}

/// One setting, drawn by its kind. A typed field commits on Enter, a toggle or a choice at once, a
/// file through the native dialog, and a secret only through its masked Replace input.
fn field_view<'a>(module_id: &str, field: &'a FieldModel, enabled: bool) -> Element<'a, Message> {
    let module = module_id.to_owned();
    let profile = field.profile.clone();
    let name = field.field.clone();
    let text = {
        let (module, profile, name) = (module.clone(), profile.clone(), name.clone());
        move |text| {
            capability(CapabilityMessage::FieldText {
                module_id: module.clone(),
                profile: profile.clone(),
                field: name.clone(),
                text,
            })
        }
    };
    let commit = capability(CapabilityMessage::FieldCommit {
        module_id: module.clone(),
        profile: profile.clone(),
        field: name.clone(),
    });
    let value = {
        let (module, profile, name) = (module.clone(), profile.clone(), name.clone());
        move |value: Value| {
            capability(CapabilityMessage::FieldValue {
                module_id: module.clone(),
                profile: profile.clone(),
                field: name.clone(),
                value,
            })
        }
    };
    let mut body = Column::new().spacing(2.0);
    match &field.kind {
        FieldKindModel::Number {
            display,
            typing,
            range,
        } => {
            body = body.push(number_field(
                &NumberFieldModel {
                    id: Some(field.id.clone()),
                    label: field.label.clone(),
                    display: display.clone(),
                    edit: match typing {
                        Some(typed) => ValueEdit::Editing {
                            text: typed.clone(),
                            invalid: field.error.as_ref().map(|_| format!("From {range}")),
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
            ));
        }
        FieldKindModel::Toggle { on } => {
            body = body.push(toggle(
                &ToggleModel {
                    label: field.label.clone(),
                    on: *on,
                    enabled,
                },
                move |on| value(Value::Bool(on)),
            ));
        }
        FieldKindModel::Choice { options, selected } => {
            let options = options.clone();
            body = body.push(label(field.label.clone()));
            body = body.push(segmented(
                &SegmentedModel {
                    options: options.clone(),
                    selected: selected.unwrap_or(usize::MAX),
                    enabled,
                },
                move |index| value(Value::from(options[index].clone())),
            ));
        }
        FieldKindModel::Text {
            display,
            typing,
            class,
        } => {
            let mut heading = row![label(field.label.clone()), fill()].align_y(Alignment::Center);
            if let Some(class) = class {
                heading = heading.push(caption(class.clone()));
            }
            body = body.push(heading);
            body = body.push(
                value_input(
                    "Not set",
                    typing.as_deref().unwrap_or(display),
                    field.error.is_some(),
                    enabled,
                    text,
                    commit,
                )
                .id(iced::widget::Id::from(field.id.clone()))
                .width(Length::Fill),
            );
        }
        FieldKindModel::Secret { state, replacing } => {
            body = body.push(
                row![
                    label(field.label.clone()),
                    fill(),
                    caption(state.clone()),
                    text_button(
                        "Replace",
                        false,
                        (enabled && replacing.is_none()).then(|| {
                            capability(CapabilityMessage::SecretEdit {
                                module_id: module.clone(),
                                profile: profile.clone(),
                                field: name.clone(),
                            })
                        }),
                    ),
                    text_button(
                        "Clear",
                        false,
                        (enabled && state == "Set").then(|| {
                            capability(CapabilityMessage::SecretClear {
                                module_id: module.clone(),
                                profile: profile.clone(),
                                field: name.clone(),
                            })
                        }),
                    ),
                ]
                .spacing(theme::SPACING / 2.0)
                .align_y(Alignment::Center),
            );
            if let Some(typed) = replacing {
                let module_for_text = module.clone();
                body = body.push(
                    row![
                        // Masked: the text is drawn as dots, and it leaves the desktop only in
                        // the one request Save sends.
                        value_input(
                            "New value",
                            typed.expose(),
                            false,
                            enabled,
                            move |text| capability(CapabilityMessage::SecretText {
                                module_id: module_for_text.clone(),
                                text: crate::state::capabilities::SecretText::new(text),
                            }),
                            capability(CapabilityMessage::SecretCommit(module.clone())),
                        )
                        .secure(true)
                        .id(iced::widget::Id::from(field.id.clone()))
                        .width(Length::Fill),
                        text_button(
                            "Save",
                            true,
                            enabled.then(|| {
                                capability(CapabilityMessage::SecretCommit(module.clone()))
                            }),
                        ),
                        text_button(
                            "Cancel",
                            false,
                            Some(capability(CapabilityMessage::SecretCancel(module.clone()))),
                        ),
                    ]
                    .spacing(theme::SPACING / 2.0)
                    .align_y(Alignment::Center),
                );
            }
        }
    }
    if field.conflict {
        body = body.push(error_caption("Changed elsewhere"));
    }
    if let Some(error) = &field.error {
        body = body.push(error_caption(error.clone()));
    }
    body.into()
}

/// A task control: the button, the profile choice when several are ready, and the newest run's
/// state with Cancel while it runs and Apply once it has succeeded. A right-click offers the
/// `task.<id>` request the button sends.
pub(crate) fn task_view<'a>(
    task: &'a TaskControl,
    enabled: bool,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let module_id = task.module_id.clone();
    let mut body = Column::new().spacing(theme::SPACING / 2.0);
    if !task.profiles.is_empty() {
        let profiles = task.profiles.clone();
        let (module, id) = (module_id.clone(), task.task.clone());
        body = body.push(segmented(
            &SegmentedModel {
                options: task
                    .profiles
                    .iter()
                    .map(|(_, label)| label.clone())
                    .collect(),
                selected: task
                    .profile
                    .as_ref()
                    .and_then(|chosen| profiles.iter().position(|(id, _)| id == chosen))
                    .unwrap_or(usize::MAX),
                enabled,
            },
            move |index| {
                capability(CapabilityMessage::TaskProfile {
                    module_id: module.clone(),
                    task: id.clone(),
                    profile: profiles[index].0.clone(),
                })
            },
        ));
    }
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
        TaskControlState::NotReady => Some(error_caption("Not ready: see the status above")),
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
