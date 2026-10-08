//! Copy chooser and paste confirmation shared by Develop and Select.
use crate::{
    app::message::{Message, copy_settings::CopySettingsMessage as C},
    state::copy_settings::{Chooser, CopyModel},
};
use iced::{
    Alignment, Length,
    widget::{Space, checkbox, column, container, mouse_area, row, scrollable, stack, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, Icon, IconButtonModel, Token, popover, text_button, theme,
    title_bar_icon_button,
};
fn msg(value: C) -> Message {
    Message::CopySettings(value)
}
fn button(label: &str, event: C) -> Element<'static, Message> {
    text_button(
        label,
        ButtonTone::Quiet,
        ButtonSize::Compact,
        Some(msg(event)),
    )
}

pub(crate) fn buttons(model: &CopyModel) -> Element<'_, Message> {
    let copy = title_bar_icon_button(
        &IconButtonModel {
            icon: Icon::Copy,
            tooltip: model
                .copy_refusal
                .clone()
                .unwrap_or_else(|| "Copy settings… (⇧⌘C)".into()),
            enabled: model.copy_refusal.is_none(),
            selected: model.state.chooser.is_some(),
        },
        model.copy_refusal.is_none().then(|| {
            msg(C::Copy {
                choose: true,
                source: None,
            })
        }),
    );
    let copy = popover(
        copy,
        model.state.chooser.as_ref().map(chooser),
        msg(C::Cancel),
    );
    let tooltip = model.paste_refusal.clone().unwrap_or_else(|| {
        model
            .state
            .clipboard
            .as_ref()
            .map(|clipboard| {
                format!(
                    "Paste settings from {} · {} groups · copied {} seconds ago\n{}",
                    clipboard.source.name,
                    clipboard.groups.len(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(clipboard.copied_at),
                    clipboard.labels().join(", ")
                )
            })
            .unwrap_or_default()
    });
    let paste = title_bar_icon_button(
        &IconButtonModel {
            icon: Icon::Paste,
            tooltip,
            enabled: model.paste_refusal.is_none(),
            selected: model.state.clipboard.is_some(),
        },
        model.paste_refusal.is_none().then(|| msg(C::Paste)),
    );
    let paste: Element<'_, Message> = if model.state.clipboard.is_some() {
        stack![
            paste,
            container(text("•").size(10).style(theme::ink(Token::Accent)))
                .align_right(Length::Fill)
                .align_top(Length::Fill)
                .padding(2)
        ]
        .width(theme::ICON_BUTTON_SIZE)
        .height(theme::ICON_BUTTON_SIZE)
        .into()
    } else {
        paste
    };
    row![copy, paste]
        .spacing(theme::TITLE_ACTION_SPACING)
        .align_y(Alignment::Center)
        .into()
}

fn chooser(model: &Chooser) -> Element<'_, Message> {
    let count = model
        .groups
        .iter()
        .filter(|group| model.form.is_checked(&group.group))
        .count();
    let header = column![
        text(format!("Copy settings from {}", model.source.name)).size(theme::SIZE_TITLE),
        text(format!(
            "{} · entry {}",
            format!("{:?}", model.kind).to_uppercase(),
            model.inspected_entry
        ))
        .size(theme::SIZE_CAPTION),
        row![
            button(
                "All",
                C::CheckMany {
                    module: None,
                    edited: false,
                    checked: true
                }
            ),
            button(
                "Edited",
                C::CheckMany {
                    module: None,
                    edited: true,
                    checked: true
                }
            ),
            button(
                "None",
                C::CheckMany {
                    module: None,
                    edited: false,
                    checked: false
                }
            ),
            text(format!("{count} of {} groups", model.groups.len())).size(theme::SIZE_CAPTION),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(10);
    let mut rows = column![].spacing(8).width(Length::Fill);
    let mut module = "";
    for group in &model.groups {
        let (heading, label) = group
            .group
            .label
            .split_once(" · ")
            .unwrap_or((&group.group.label, &group.group.label));
        if module != heading {
            module = heading;
            let members: Vec<_> = model
                .groups
                .iter()
                .filter(|row| row.group.label.split(" · ").next() == Some(heading))
                .collect();
            let selected = members
                .iter()
                .filter(|row| model.form.is_checked(&row.group))
                .count();
            let caption = if selected > 0 && selected < members.len() {
                format!("− {heading}")
            } else {
                heading.to_owned()
            };
            rows = rows.push(button(
                &caption,
                C::CheckMany {
                    module: Some(heading.into()),
                    edited: false,
                    checked: selected != members.len(),
                },
            ));
        }
        let mut check = checkbox(model.form.is_checked(&group.group))
            .label(label.to_owned())
            .text_size(theme::SIZE_CONTROL)
            .size(14)
            .width(Length::Shrink);
        if group.reason.is_none() {
            let label = group.group.label.clone();
            check = check.on_toggle(move |checked| {
                msg(C::Check {
                    label: label.clone(),
                    checked,
                })
            });
        }
        let caption = group
            .reason
            .clone()
            .unwrap_or_else(|| if group.custom { "Custom" } else { "Original" }.into());
        rows = rows.push(
            row![
                check,
                Space::new().width(Length::Fill),
                text(caption)
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(if group.custom {
                        Token::Accent
                    } else {
                        Token::TextSecondary
                    }))
            ]
            .spacing(8),
        );
    }
    let body = column![
        header,
        scrollable(container(rows).padding(iced::Padding {
            right: 18.0,
            ..iced::Padding::default()
        }))
        .height(Length::Fixed(360.0)),
        text("Crop, orientation, lens, perspective, RAW look and masks stay with each photograph.")
            .size(theme::SIZE_CAPTION),
        text("Checked Original groups reset those groups on the target.").size(theme::SIZE_CAPTION),
        row![
            button("Cancel", C::Cancel),
            text_button(
                "Copy",
                ButtonTone::Primary,
                ButtonSize::Compact,
                (count > 0).then(|| msg(C::Chosen))
            )
        ]
        .spacing(8),
    ]
    .spacing(12)
    .width(Length::Fill);
    container(body)
        .padding(16)
        .width(Length::Fixed(460.0))
        .style(theme::panel_surface)
        .into()
}

pub(crate) fn confirmation(model: &CopyModel) -> Option<Element<'_, Message>> {
    let confirm = model.state.confirm.as_ref()?;
    let count = confirm.targets.count();
    let body = column![
        text(format!("Paste settings to {count} photographs?")).size(theme::SIZE_TITLE),
        text(format!("Settings from {}", confirm.clipboard.source.name)),
        text(confirm.clipboard.labels().join(" · ")).size(theme::SIZE_CAPTION),
        text("Each photograph gets its own history entry. Undo in each photograph’s history; there is no undo for the whole paste."),
        text(confirm.skip.clone()).size(theme::SIZE_CAPTION),
        row![button("Cancel", C::Cancel), text_button(&format!("Paste to {count}"), ButtonTone::Primary, ButtonSize::Compact, Some(msg(C::Confirm)))].spacing(8),
    ].spacing(14);
    Some(
        stack![
            mouse_area(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
            )
            .on_press(msg(C::Cancel)),
            container(
                container(body)
                    .padding(24)
                    .width(520)
                    .style(theme::panel_surface)
            )
            .center(Length::Fill),
        ]
        .into(),
    )
}

pub(crate) fn card(model: &CopyModel) -> Element<'_, Message> {
    let Some(clipboard) = &model.state.clipboard else {
        return Space::new().into();
    };
    container(
        column![
            text(format!("Settings from {}", clipboard.source.name)).size(theme::SIZE_CAPTION),
            text(format!(
                "{} groups · {}",
                clipboard.groups.len(),
                clipboard
                    .source
                    .entry
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            ))
            .size(theme::SIZE_CAPTION),
            text(format!(
                "Copied {} seconds ago",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(clipboard.copied_at)
            ))
            .size(theme::SIZE_CAPTION),
            text(clipboard.labels().join(" · ")).size(theme::SIZE_CAPTION),
            text_button(
                &format!("Paste settings to {}", model.targets),
                ButtonTone::Primary,
                ButtonSize::Compact,
                model.paste_refusal.is_none().then(|| msg(C::Paste))
            ),
        ]
        .spacing(8),
    )
    .padding(12)
    .into()
}

pub(crate) fn cell_menu(model: &CopyModel) -> Option<Element<'_, Message>> {
    let (at, source, targets) = model.menu.as_ref()?;
    use crate::app::message::develop::DevelopMessage;
    let items = vec![
        (
            format!("Paste settings to {} photographs", targets.count()),
            msg(C::PasteTo(targets.clone())),
        ),
        (
            format!("Copy settings from {}", source.name),
            msg(C::Copy {
                choose: false,
                source: Some(source.clone()),
            }),
        ),
        (
            "Copy settings…".into(),
            msg(C::Copy {
                choose: true,
                source: Some(source.clone()),
            }),
        ),
        (
            "Select all".into(),
            Message::Develop(DevelopMessage::SelectAll),
        ),
        (
            "Select only the open photograph".into(),
            Message::Develop(DevelopMessage::SelectOnly),
        ),
    ];
    let entries = items
        .into_iter()
        .map(|(label, message)| {
            let reason = match &message {
                Message::CopySettings(C::PasteTo(_)) => model.paste_refusal.clone(),
                Message::CopySettings(C::Copy { .. }) if model.state.pending => {
                    Some("Waiting for the settings capture".into())
                }
                _ => None,
            };
            luxforge_ui::MenuEntry::Item(luxforge_ui::MenuItem {
                icon: None,
                label,
                trailing: None,
                on_press: reason.is_none().then_some(message),
                reason,
            })
        })
        .collect();
    Some(
        stack![
            mouse_area(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
            )
            .on_press(msg(C::Cancel))
            .on_right_press(msg(C::Cancel)),
            container(luxforge_ui::menu_list(entries)).padding(iced::Padding {
                top: (at.1 - 190.0).max(0.0),
                left: at.0.max(0.0),
                ..iced::Padding::default()
            }),
        ]
        .into(),
    )
}
