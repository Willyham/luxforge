//! A module's query-choice control, drawn from its answer: the current or suggested choice as a
//! card, the answer's notice, the search with its paged rows only when the answer offers one (under
//! a card only while Change is open), and a report link. Eligibility, notices and links come from
//! the host query; the widget carries keys and typed input messages and decides nothing.
use crate::{
    app::message::{Message, control::ControlMessage},
    state::query_choice::{NoticeLevel, QueryChoiceCard, QueryChoiceModel},
};
use iced::{
    Length,
    widget::{button, checkbox, column, row, text_input},
};
use luxforge_ui::Element;
use luxforge_ui::{
    ButtonSize, ButtonTone, Tone, caption, error_caption, inline_notice, label, text_button, theme,
};

fn control(message: ControlMessage) -> Message {
    Message::Control(message)
}

/// A card: its title, subtitle, note and any reasons it cannot be chosen, then its buttons.
fn card<'a>(card: &QueryChoiceCard, buttons: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut content = column![label(card.title.clone())].spacing(2);
    if let Some(subtitle) = &card.subtitle {
        content = content.push(caption(subtitle.clone()));
    }
    if let Some(note) = &card.note {
        content = content.push(inline_notice(Tone::Neutral, note.clone()));
    }
    if !card.eligible && !card.reasons.is_empty() {
        content = content.push(error_caption(card.reasons.join(" · ")));
    }
    let mut actions = row![].spacing(theme::BUTTON_ROW_SPACING);
    for button in buttons {
        actions = actions.push(button);
    }
    column![content, actions]
        .spacing(theme::ROW_SPACING * 3.0)
        .width(Length::Fill)
        .into()
}

pub(crate) fn query_choice_view(model: &QueryChoiceModel, enabled: bool) -> Element<'_, Message> {
    let enabled = enabled && model.enabled;
    let ui = &model.ui;
    let action = model.control.action.clone();
    let mut content = column![]
        .spacing(theme::ROW_SPACING * 3.0)
        .width(Length::Fill);

    let change = |open: bool| {
        text_button(
            if open { "Close" } else { "Change" },
            ButtonTone::Control,
            ButtonSize::Compact,
            (enabled && ui.search).then(|| {
                control(ControlMessage::QueryChoiceChange {
                    action: action.clone(),
                    open: !open,
                })
            }),
        )
    };
    if let Some(current) = &ui.current {
        let buttons = if ui.search {
            vec![change(ui.changing)]
        } else {
            Vec::new()
        };
        content = content.push(card(current, buttons));
    } else if let Some(suggestion) = &ui.suggestion {
        let apply = text_button(
            suggestion.label.as_deref().unwrap_or("Apply"),
            ButtonTone::Primary,
            ButtonSize::Compact,
            (enabled && suggestion.eligible && !ui.loading).then(|| {
                control(ControlMessage::QueryChoiceApply {
                    action: action.clone(),
                })
            }),
        );
        let mut buttons = vec![apply];
        if ui.search {
            buttons.push(change(ui.changing));
        }
        content = content.push(card(suggestion, buttons));
    }
    if let Some(notice) = &ui.notice {
        let tone = match notice.level {
            NoticeLevel::Info => Tone::Neutral,
            NoticeLevel::Warning => Tone::Warning,
        };
        content = content.push(inline_notice(tone, notice.text.clone()));
    }

    if ui.shows_inputs() {
        for shared in &model.shared {
            let name = shared.parameter.name.clone();
            let action = action.clone();
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
    }

    if ui.shows_search() {
        let search_action = action.clone();
        let mut input = text_input(
            &format!("Search {}…", model.control.label.to_lowercase()),
            &ui.text,
        )
        .size(theme::SIZE_CAPTION);
        if enabled {
            input = input.on_input(move |text| {
                control(ControlMessage::QueryChoiceSearch {
                    action: search_action.clone(),
                    text,
                })
            });
        }
        content = content.push(input);
        if ui.loading {
            content = content.push(caption("Updating choices…"));
        }
        for choice in &ui.rows {
            let mut select = button(label(choice.title.clone()))
                .style(theme::button_plain)
                .width(Length::Fill);
            if enabled && choice.eligible && !ui.loading {
                select = select.on_press(control(ControlMessage::QueryChoiceSelect {
                    action: action.clone(),
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
        if ui.pages > 1 {
            let mut previous = button(label("Previous")).style(theme::button_plain);
            let mut next = button(label("Next")).style(theme::button_plain);
            if enabled && !ui.loading && ui.page > 0 {
                previous = previous.on_press(control(ControlMessage::QueryChoicePage {
                    action: action.clone(),
                    page: ui.page - 1,
                }));
            }
            if enabled && !ui.loading && ui.page + 1 < ui.pages {
                next = next.on_press(control(ControlMessage::QueryChoicePage {
                    action: action.clone(),
                    page: ui.page + 1,
                }));
            }
            content = content.push(
                row![
                    previous,
                    caption(format!("{} / {}", ui.page + 1, ui.pages)),
                    next
                ]
                .spacing(theme::ROW_SPACING),
            );
        }
    }
    // A failed answer keeps its Retry whether or not the search is shown, so a status-only query
    // that failed can be asked again.
    if let Some(error) = &ui.error {
        let mut retry = button(label("Retry")).style(theme::button_plain);
        if enabled && ui.can_retry() {
            retry = retry.on_press(control(ControlMessage::QueryChoiceRetry {
                action: action.clone(),
            }));
        }
        content =
            content.push(row![error_caption(error.clone()), retry].spacing(theme::ROW_SPACING));
    }
    if let Some(report) = &ui.report {
        content = content.push(text_button(
            &report.label,
            ButtonTone::Control,
            ButtonSize::Compact,
            enabled.then(|| {
                control(ControlMessage::QueryChoiceReport {
                    action: action.clone(),
                })
            }),
        ));
    }
    if let Some(summary) = ui
        .status
        .as_ref()
        .and_then(|status| status.get("summary"))
        .and_then(serde_json::Value::as_str)
    {
        content = content.push(caption(summary.to_owned()));
    }
    content.into()
}
