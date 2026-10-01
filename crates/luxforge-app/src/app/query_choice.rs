//! Descriptor-backed search and paging. One request runs and only the newest waits; each answer
//! is correlated with the asset, displayed entry, text, page and shared inputs that requested it.
use super::{
    Editor,
    message::{Message, action::ActionMessage, control::ControlMessage},
    tasks,
};
use crate::state::{
    control_tree::walk,
    fields,
    query_choice::{QueryChoiceIdentity, query_parameters},
    tools::{self, ControlModel, SectionModel},
};
use iced::Task;
use luxforge_core::{Control, QueryChoiceControl};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
pub(crate) struct QueryChoiceRequest {
    identity: QueryChoiceIdentity,
    control: QueryChoiceControl,
}

fn declaration(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
) -> Option<QueryChoiceControl> {
    modules.iter().find_map(|module| {
        walk(&module.controls).find_map(|control| match control {
            Control::QueryChoice(control) if control.action == action => Some(control.clone()),
            _ => None,
        })
    })
}

fn visible_choice(section: &SectionModel) -> Option<String> {
    if !section.shows_controls() || !section.enabled {
        return None;
    }
    let mut controls = walk(&section.controls);
    let tab = section.visible_tab();
    while let Some(control) = controls.next() {
        match control {
            ControlModel::QueryChoice(choice) => return Some(choice.control.action.clone()),
            ControlModel::Group(group) => {
                let shown = match tab {
                    Some(tab) if controls.depth() == 1 => std::ptr::eq(group, tab),
                    _ => group.expanded,
                };
                if !shown {
                    controls.skip_children();
                }
            }
            _ => {}
        }
    }
    None
}

impl Editor {
    fn query_choice_shared(
        &self,
        control: &QueryChoiceControl,
    ) -> Result<Map<String, Value>, String> {
        let declared = tools::declared_action(&self.modules, &control.action)
            .ok_or("Choice action is unavailable")?;
        let mut shared = Map::new();
        for name in &control.shared {
            let parameter = declared
                .parameter(name)
                .ok_or("Choice input is unavailable")?;
            let text = self
                .controls
                .fields
                .get(&control.action, name)
                .unwrap_or("");
            if text.trim().is_empty() && !parameter.required && parameter.default.is_none() {
                continue;
            }
            shared.insert(name.clone(), fields::parse_field(parameter, text)?);
        }
        Ok(shared)
    }

    pub(super) fn query_choice_update(&mut self, message: ControlMessage) -> Task<Message> {
        match message {
            ControlMessage::QueryChoiceSearch { action, text } => {
                let ui = self
                    .controls
                    .ui
                    .query_choices
                    .entry(action.clone())
                    .or_default();
                ui.text = text;
                ui.page = 0;
                self.request_query_choice(&action)
            }
            ControlMessage::QueryChoicePage { action, page } => {
                if self
                    .controls
                    .ui
                    .query_choices
                    .entry(action.clone())
                    .or_default()
                    .change_page(page)
                {
                    self.request_query_choice(&action)
                } else {
                    Task::none()
                }
            }
            ControlMessage::QueryChoiceRetry { action } => {
                if self
                    .controls
                    .ui
                    .query_choices
                    .get(&action)
                    .is_some_and(|ui| ui.can_retry())
                {
                    self.request_query_choice(&action)
                } else {
                    Task::none()
                }
            }
            ControlMessage::QueryChoiceShared {
                action,
                parameter,
                text,
            } => {
                self.controls.fields.set(&action, &parameter, text);
                self.controls
                    .ui
                    .query_choices
                    .entry(action.clone())
                    .or_default()
                    .page = 0;
                self.request_query_choice(&action)
            }
            ControlMessage::QueryChoiceSelect { action, key } => {
                let Some(control) = declaration(&self.modules, &action) else {
                    return Task::none();
                };
                let selected = self.query_choice_shared(&control).and_then(|shared| {
                    let ui = self
                        .controls
                        .ui
                        .query_choices
                        .get(&action)
                        .ok_or("Choices are not ready")?;
                    let request = ui.request.as_ref().ok_or("Choices are not ready")?;
                    if self.document.state.as_ref().map(|state| &state.asset.id)
                        != Some(&request.asset)
                        || self.displayed_entry().as_ref() != Some(&request.entry)
                        || request.shared != shared
                    {
                        return Err("Choices are no longer current".into());
                    }
                    ui.selection(&control, &key, shared)
                });
                match selected {
                    Ok(preset) => self.action_update(ActionMessage::Run { action, preset }),
                    Err(reason) => {
                        self.status.text = reason;
                        Task::none()
                    }
                }
            }
            ControlMessage::QueryChoiceAnswered { identity, result } => {
                self.controls.query_choice_slot.answered();
                let current = self.document.state.as_ref().map(|state| &state.asset.id)
                    == Some(&identity.asset)
                    && self.displayed_entry().as_ref() == Some(&identity.entry);
                if current {
                    let ui = self
                        .controls
                        .ui
                        .query_choices
                        .entry(identity.action.clone())
                        .or_default();
                    if ui.accept(&identity, result) {
                        let failure = ui.error.clone();
                        self.outcome(super::outcome::Outcome::QueryChoiceAnswered {
                            action: &identity.action,
                            failure: failure.as_deref(),
                        });
                    }
                }
                self.start_query_choice()
            }
            _ => Task::none(),
        }
    }

    fn request_query_choice(&mut self, action: &str) -> Task<Message> {
        let Some(control) = declaration(&self.modules, action) else {
            return Task::none();
        };
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        let shared = match self.query_choice_shared(&control) {
            Ok(shared) => shared,
            Err(error) => {
                let ui = self
                    .controls
                    .ui
                    .query_choices
                    .entry(action.to_owned())
                    .or_default();
                ui.error = Some(error);
                ui.loading = false;
                ui.rows.clear();
                ui.request = None;
                return Task::none();
            }
        };
        let ui = self
            .controls
            .ui
            .query_choices
            .entry(action.to_owned())
            .or_default();
        let identity = ui.begin(asset, entry, action.to_owned(), shared);
        if let Some(displaced) = self
            .controls
            .query_choice_slot
            .offer(QueryChoiceRequest { identity, control })
        {
            let ui = self
                .controls
                .ui
                .query_choices
                .get_mut(&displaced.identity.action);
            if let Some(ui) = ui
                && ui.request.as_ref() == Some(&displaced.identity)
            {
                ui.request = None;
                ui.loading = false;
            }
        }
        self.start_query_choice()
    }

    fn start_query_choice(&mut self) -> Task<Message> {
        let Some(request) = self.controls.query_choice_slot.start() else {
            return Task::none();
        };
        let owner = self.owner.clone();
        let client = self.client;
        let identity = request.identity;
        let sent = identity.clone();
        let control = request.control;
        tasks::owner_task(
            move || {
                let mut params = query_parameters(&control, &sent);
                params.insert("asset_id".into(), json!(sent.asset));
                params.insert("entry_id".into(), json!(sent.entry));
                tasks::call(
                    &owner,
                    client,
                    &format!("query.{}", control.query),
                    Value::Object(params),
                )
                .map(|(answer, _)| answer)
            },
            move |result| {
                Message::Control(ControlMessage::QueryChoiceAnswered {
                    identity: identity.clone(),
                    result,
                })
            },
        )
    }

    pub(crate) fn request_visible_query_choices(&mut self) -> Task<Message> {
        if !self.controls.query_choice_slot.idle() {
            return Task::none();
        }
        let (Some(asset), Some(entry)) = (
            self.document.state.as_ref().map(|state| &state.asset.id),
            self.displayed_entry(),
        ) else {
            return Task::none();
        };
        let wanted = self
            .workspace
            .tools
            .all()
            .filter_map(visible_choice)
            .find(|action| {
                let Some(control) = declaration(&self.modules, action) else {
                    return false;
                };
                let Ok(shared) = self.query_choice_shared(&control) else {
                    return false;
                };
                self.controls
                    .ui
                    .query_choices
                    .get(action)
                    .and_then(|ui| ui.request.as_ref().map(|request| (ui, request)))
                    .is_none_or(|(ui, request)| {
                        &request.asset != asset
                            || request.entry != entry
                            || request.text != ui.text
                            || request.page != ui.page
                            || request.shared != shared
                    })
            });
        match wanted {
            Some(action) => self.request_query_choice(&action),
            None => Task::none(),
        }
    }
}
