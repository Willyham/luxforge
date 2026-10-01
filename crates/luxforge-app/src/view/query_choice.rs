//! A searchable, paged module list. Eligibility and reasons come from the host query; the widget
//! carries keys and typed input messages and makes no matching or admission decisions.
use crate::{
    app::message::{Message, control::ControlMessage},
    state::query_choice::QueryChoiceModel,
};
use iced::{
    Element, Length,
    widget::{button, checkbox, column, row, text_input},
};
use luxforge_ui::{caption, error_caption, label, theme};

fn control(message: ControlMessage) -> Message {
    Message::Control(message)
}

pub(crate) fn query_choice_view(model: &QueryChoiceModel, enabled: bool) -> Element<'_, Message> {
    let enabled = enabled && model.enabled;
    let action = model.control.action.clone();
    let mut input = text_input("Search…", &model.ui.text).size(theme::SIZE_CAPTION);
    if enabled {
        input = input.on_input(move |text| {
            control(ControlMessage::QueryChoiceSearch {
                action: action.clone(),
                text,
            })
        });
    }
    let mut content = column![caption(model.control.label.clone()), input]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    if let Some(summary) = model
        .ui
        .status
        .as_ref()
        .and_then(|status| status.get("summary"))
        .and_then(serde_json::Value::as_str)
    {
        content = content.push(caption(summary.to_owned()));
    }
    for shared in &model.shared {
        let name = shared.parameter.name.clone();
        let action = model.control.action.clone();
        let mut label_text = name.replace('-', " ");
        if let Some(first) = label_text.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        let label_text = crate::state::fields::labelled(&label_text, &shared.parameter);
        let input: Element<'_, Message> = if shared.is_boolean() {
            let mut input = checkbox(shared.text == "true").label(label_text);
            if enabled {
                input = input.on_toggle(move |checked| {
                    control(ControlMessage::QueryChoiceShared {
                        action: action.clone(),
                        parameter: name.clone(),
                        text: checked.to_string(),
                    })
                });
            }
            input.into()
        } else {
            let mut input = text_input(&label_text, &shared.text).size(theme::SIZE_CAPTION);
            if enabled {
                input = input.on_input(move |text| {
                    control(ControlMessage::QueryChoiceShared {
                        action: action.clone(),
                        parameter: name.clone(),
                        text,
                    })
                });
            }
            input.into()
        };
        content = content.push(input);
    }
    if model.ui.loading {
        content = content.push(caption("Updating choices…"));
    }
    if let Some(error) = &model.ui.error {
        let mut retry = button(label("Retry")).style(theme::button_plain);
        if enabled && model.ui.can_retry() {
            retry = retry.on_press(control(ControlMessage::QueryChoiceRetry {
                action: model.control.action.clone(),
            }));
        }
        content =
            content.push(row![error_caption(error.clone()), retry].spacing(theme::ROW_SPACING));
    }
    for choice in &model.ui.rows {
        let mut select = button(label(choice.title.clone()))
            .style(theme::button_plain)
            .width(Length::Fill);
        if enabled && choice.eligible && !model.ui.loading {
            select = select.on_press(control(ControlMessage::QueryChoiceSelect {
                action: model.control.action.clone(),
                key: choice.key.clone(),
            }));
        }
        let mut line = column![select].spacing(2);
        if let Some(subtitle) = &choice.subtitle {
            line = line.push(caption(subtitle.clone()));
        }
        if !choice.eligible && !choice.reasons.is_empty() {
            line = line.push(error_caption(choice.reasons.join(" · ")));
        }
        content = content.push(line);
    }
    let mut previous = button(label("Previous")).style(theme::button_plain);
    let mut next = button(label("Next")).style(theme::button_plain);
    if enabled && !model.ui.loading && model.ui.page > 0 {
        previous = previous.on_press(control(ControlMessage::QueryChoicePage {
            action: model.control.action.clone(),
            page: model.ui.page - 1,
        }));
    }
    if enabled && !model.ui.loading && model.ui.page + 1 < model.ui.pages {
        next = next.on_press(control(ControlMessage::QueryChoicePage {
            action: model.control.action.clone(),
            page: model.ui.page + 1,
        }));
    }
    content
        .push(
            row![
                previous,
                caption(format!("{} / {}", model.ui.page + 1, model.ui.pages)),
                next
            ]
            .spacing(theme::ROW_SPACING),
        )
        .into()
}
