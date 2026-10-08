//! Settings capture runs off the update loop; every paste follows the shared command path.
use super::{
    Editor,
    message::{Message, copy_settings::CopySettingsMessage as C},
    tasks::{call, owner_task, request},
};
use crate::state::{
    copy_settings::{Chooser, Clipboard, Confirmation, CopyModel, Group, Source, Targets},
    presets::{PresetForm, PresettableGroup, capture_fields, copyable_groups},
    select::{SelectionModel, Shown},
    select_catalog::BatchKind,
};
use iced::Task;
use luxforge_core::{ClientId, EditorState, ModuleDescriptor, OwnerHandle, catalog_types::RowItem};
use serde_json::{Map, Value, json};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

fn message(value: C) -> Message {
    Message::CopySettings(value)
}

fn read_source(
    owner: &OwnerHandle,
    client: ClientId,
    mut source: Source,
) -> Result<(Source, luxforge_core::SourceTag), String> {
    let (value, _) = call(
        owner,
        client,
        "asset.state",
        json!({"asset_id": source.asset}),
    )?;
    let state: EditorState = serde_json::from_value(value).map_err(|error| error.to_string())?;
    source.entry = Some(source.entry.unwrap_or(state.current_entry.id));
    if source.name.is_empty() {
        source.name = state
            .asset
            .locator
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    Ok((source, state.asset.source.tag()))
}

pub(crate) fn capture_now(
    owner: &OwnerHandle,
    client: ClientId,
    source: Source,
    groups: &[PresettableGroup],
    form: &PresetForm,
) -> Result<Arc<Clipboard>, String> {
    let fields = capture_fields(groups, form);
    if fields.is_empty() {
        return Err("Choose at least one group of settings".into());
    }
    let (source, kind) = read_source(owner, client, source)?;
    let params = json!({"asset_id": source.asset, "entry_id": source.entry, "fields": fields});
    let (captured, _) = call(owner, client, "preset.capture", params.clone())?;
    let settings = captured["settings"]
        .as_object()
        .ok_or("Capture returned no settings")?
        .clone();
    Ok(Arc::new(Clipboard {
        capture: json!({"method":"preset.capture", "params":params}),
        source,
        kind,
        settings,
        groups: groups
            .iter()
            .filter(|group| form.is_checked(group))
            .cloned()
            .collect(),
        copied_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }))
}

fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => left == right,
    }
}

fn custom(
    settings: &Map<String, Value>,
    fields: &Map<String, Value>,
    modules: &[ModuleDescriptor],
) -> bool {
    settings.iter().any(|(action, values)| {
        let declared = modules.iter().find_map(|module| module.action(action));
        values.as_object().is_some_and(|values| {
            values.iter().any(|(name, value)| {
                let included = fields.get(action).is_none_or(|names| {
                    names == &Value::Bool(true)
                        || names
                            .as_array()
                            .is_some_and(|names| names.iter().any(|field| field == name))
                });
                included
                    && declared
                        .and_then(|action| action.parameter(name))
                        .is_none_or(|parameter| {
                            parameter
                                .default
                                .as_ref()
                                .is_none_or(|default| !same_value(default, value))
                        })
            })
        })
    })
}

pub(crate) fn inspect_now(
    owner: &OwnerHandle,
    client: ClientId,
    source: Source,
    groups: Vec<PresettableGroup>,
    mut form: PresetForm,
    modules: &[ModuleDescriptor],
) -> Result<Chooser, String> {
    let (resolved, kind) = read_source(owner, client, source.clone())?;
    let all = PresetForm {
        checked: groups
            .iter()
            .map(|group| (group.label.clone(), true))
            .collect(),
        ..PresetForm::default()
    };
    let capture = |fields: Map<String, Value>| -> Result<Map<String, Value>, String> {
        let (value, _) = call(
            owner,
            client,
            "preset.capture",
            json!({"asset_id": resolved.asset, "entry_id": resolved.entry, "fields": fields}),
        )?;
        value["settings"]
            .as_object()
            .cloned()
            .ok_or("Capture returned no settings".into())
    };
    let captured = capture(capture_fields(&groups, &all));
    let mut rows = Vec::new();
    for group in groups {
        let fields = capture_fields(std::slice::from_ref(&group), &all);
        // A successful whole capture supplies ordinary fields. Kind-resolved controls and a
        // refused whole capture get a narrow read, so the chooser can name each affected group.
        let local = match &captured {
            Ok(settings)
                if fields.iter().all(|(action, names)| {
                    names.as_array().is_some_and(|names| {
                        names.iter().all(|name| {
                            settings
                                .get(action)
                                .and_then(|values| values.get(name.as_str().unwrap_or_default()))
                                .is_some()
                        })
                    })
                }) =>
            {
                let selected: Map<String, Value> = fields
                    .iter()
                    .filter_map(|(action, names)| {
                        settings
                            .get(action)
                            .and_then(Value::as_object)
                            .map(|values| {
                                (
                                    action.clone(),
                                    Value::Object(
                                        values
                                            .iter()
                                            .filter(|(name, _)| {
                                                names.as_array().is_some_and(|names| {
                                                    names.iter().any(|field| field == name.as_str())
                                                })
                                            })
                                            .map(|(name, value)| (name.clone(), value.clone()))
                                            .collect(),
                                    ),
                                )
                            })
                    })
                    .collect();
                Ok(selected)
            }
            _ => capture(fields.clone()),
        };
        let (custom, reason) = match local {
            Ok(settings) => (custom(&settings, &fields, modules), None),
            Err(reason) => {
                form.checked.insert(group.label.clone(), false);
                (false, Some(reason))
            }
        };
        rows.push(Group {
            group,
            custom,
            reason,
        });
    }
    Ok(Chooser {
        source,
        inspected_entry: resolved.entry.unwrap(),
        kind,
        groups: rows,
        form,
    })
}

impl Editor {
    pub(crate) fn copy_source(&self) -> Result<Source, String> {
        if self.view_state.copy_settings.pending {
            return Err("Waiting for the settings capture".into());
        }
        if self.select.state.shown == Shown::Select {
            if !self.select.state.over_catalog() {
                return Err("Only developed photographs have settings".into());
            }
            let selection = SelectionModel::of(&self.session.browse, self.select.state.revision());
            let row = selection
                .active
                .and_then(|position| self.select.state.rows.row(position))
                .ok_or("Select a photograph first")?;
            return match &row.item {
                RowItem::Photo { asset_id } => Ok(Source {
                    asset: asset_id.clone(),
                    entry: None,
                    name: row.file_name.clone(),
                }),
                _ => Err("Only developed photographs have settings".into()),
            };
        }
        let state = self
            .document
            .state
            .as_ref()
            .ok_or("No photograph is open")?;
        if self.busy {
            return Err(crate::state::IN_FLIGHT.into());
        }
        let entry = self
            .displayed_entry()
            .ok_or("No history entry is displayed")?;
        Ok(Source {
            asset: state.asset.id.clone(),
            entry: (entry != state.current_entry.id).then_some(entry),
            name: state
                .asset
                .locator
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        })
    }

    pub(crate) fn paste_targets(&self) -> Targets {
        if self.select.state.shown == Shown::Select {
            let selection = SelectionModel::of(&self.session.browse, self.select.state.revision());
            Targets::Selection {
                revision: self.session.browse.revision,
                ranges: selection.ranges,
                count: selection.count,
            }
        } else {
            Targets::Assets(
                self.develop
                    .state
                    .set
                    .as_ref()
                    .map(|set| set.selected_assets())
                    .unwrap_or_else(|| {
                        self.document
                            .state
                            .as_ref()
                            .map(|state| vec![state.asset.id.clone()])
                            .unwrap_or_default()
                    }),
            )
        }
    }

    pub(crate) fn paste_refusal(&self, previous: bool) -> Option<String> {
        if self.view_state.copy_settings.pending {
            return Some("Waiting for the settings capture".into());
        }
        if previous
            && (self.select.state.shown == Shown::Select
                || self.view_state.copy_settings.previous.is_none())
        {
            return Some("No previous photograph in this window".into());
        }
        if !previous && self.view_state.copy_settings.clipboard.is_none() {
            return Some("Nothing copied yet · Copy settings ⇧⌘C".into());
        }
        if let Some(batch) = self.select.state.catalog.running() {
            return Some(format!("Waiting for the batch: {}", batch.running()));
        }
        if self.select.state.shown == Shown::Select {
            return self.batch_refusal();
        }
        self.action_refusal("apply-settings")
    }

    pub(crate) fn copy_model(&self) -> CopyModel {
        CopyModel {
            menu: self
                .view_state
                .copy_settings
                .cell_menu
                .and_then(|(index, at)| {
                    let set = self.develop.state.set.as_ref()?;
                    let photo = set.photo(index)?;
                    let source = Source {
                        asset: photo.asset_id.clone(),
                        entry: None,
                        name: photo.name.clone(),
                    };
                    let targets = Targets::Assets(if set.selected(index) {
                        set.selected_assets()
                    } else {
                        vec![photo.asset_id.clone()]
                    });
                    let at = (
                        at.0.min(
                            (self.view_state.window.0 - luxforge_ui::theme::MENU_WIDTH).max(0.0),
                        ),
                        at.1.min(self.view_state.window.1),
                    );
                    Some((at, source, targets))
                }),
            report: self
                .select
                .state
                .catalog
                .report
                .then(|| {
                    crate::state::select_catalog::sheet(
                        &self.select.state,
                        &SelectionModel::of(&self.session.browse, self.select.state.revision()),
                    )
                })
                .flatten(),
            batch_report: self
                .select
                .state
                .catalog
                .batch
                .as_ref()
                .is_some_and(|batch| {
                    matches!(
                        batch.end,
                        Some(crate::state::select_catalog::BatchEnd::Done(_))
                    )
                }),
            state: self.view_state.copy_settings.clone(),
            copy_refusal: self.copy_source().err(),
            paste_refusal: self.paste_refusal(false),
            previous_refusal: self.paste_refusal(true),
            targets: if self.select.state.shown == Shown::Select {
                self.session.browse.selection.count as usize
            } else {
                self.develop
                    .state
                    .set
                    .as_ref()
                    .map_or(usize::from(self.document.state.is_some()), |set| {
                        set.selected.len() + usize::from(!set.selected.contains(&set.active))
                    })
            },
        }
    }

    pub(crate) fn copy_settings_update(&mut self, event: C) -> Task<Message> {
        match event {
            C::BatchStarted {
                kind,
                params,
                count,
                names,
                result,
            } => {
                self.view_state.copy_settings.pending = false;
                return self.adopt_batch_start(kind, params, count, names, result);
            }
            C::Copy { choose, source } => {
                self.view_state.copy_settings.cell_menu = None;
                self.select.state.catalog.context_at = None;
                self.select.state.catalog.menu = None;
                let source = match source.map(Ok).unwrap_or_else(|| self.copy_source()) {
                    Ok(source) => source,
                    Err(reason) => {
                        self.status.text = reason;
                        return Task::none();
                    }
                };
                return self.capture_settings(source, choose, false, None);
            }
            C::Chosen => {
                let Some(chooser) = self.view_state.copy_settings.chooser.clone() else {
                    return Task::none();
                };
                return self.capture_settings(chooser.source, false, false, Some(chooser.form));
            }
            C::Previous => {
                if let Some(reason) = self.paste_refusal(true) {
                    self.status.text = reason;
                    return Task::none();
                }
                if let Some(mut source) = self.view_state.copy_settings.previous.clone() {
                    source.entry = None;
                    self.view_state.copy_settings.previous_target = self
                        .document
                        .state
                        .as_ref()
                        .map(|state| state.asset.id.clone());
                    return self.capture_settings(source, false, true, None);
                }
            }
            C::Check { label, checked } => {
                if let Some(chooser) = &mut self.view_state.copy_settings.chooser
                    && chooser
                        .groups
                        .iter()
                        .any(|group| group.group.label == label && group.reason.is_none())
                {
                    chooser.form.checked.insert(label, checked);
                }
            }
            C::CheckMany {
                module,
                edited,
                checked,
            } => {
                if let Some(chooser) = &mut self.view_state.copy_settings.chooser {
                    for group in &chooser.groups {
                        if module.as_ref().is_none_or(|module| {
                            group.group.label.split(" · ").next() == Some(module)
                        }) {
                            chooser.form.checked.insert(
                                group.group.label.clone(),
                                group.reason.is_none() && checked && (!edited || group.custom),
                            );
                        }
                    }
                }
            }
            C::CopyReport => {
                if let Some(crate::state::select_catalog::BatchEnd::Done(report)) = self
                    .select
                    .state
                    .catalog
                    .batch
                    .as_ref()
                    .and_then(|batch| batch.end.as_ref())
                {
                    return iced::clipboard::write(
                        serde_json::to_string_pretty(report).unwrap_or_default(),
                    );
                }
            }
            C::Cancel => {
                self.select.state.catalog.report = false;
                let state = &mut self.view_state.copy_settings;
                state.serial += 1;
                state.pending = false;
                state.cell_menu = None;
                state.chooser = None;
                state.confirm = None;
            }
            C::Inspected { serial, result } => {
                if serial != self.view_state.copy_settings.serial {
                    return Task::none();
                }
                self.view_state.copy_settings.pending = false;
                match result {
                    Ok(chooser) => self.view_state.copy_settings.chooser = Some(chooser),
                    Err(reason) => self.status.text = reason,
                }
            }
            C::Captured {
                serial,
                result,
                previous,
            } => {
                if serial != self.view_state.copy_settings.serial {
                    return Task::none();
                }
                self.view_state.copy_settings.pending = false;
                match result {
                    Ok(clipboard) => {
                        if previous {
                            let target = self.view_state.copy_settings.previous_target.take();
                            if self.document.state.as_ref().map(|state| &state.asset.id)
                                != target.as_ref()
                            {
                                self.status.text =
                                    "Photograph changed before Paste from previous finished".into();
                                return Task::none();
                            }
                            return self.paste_single(&clipboard);
                        }
                        if let Some(chooser) = self.view_state.copy_settings.chooser.take() {
                            self.view_state.copy_settings.remembered = chooser.form.checked;
                        }
                        self.view_state.copy_settings.request = Some(clipboard.capture.clone());
                        self.status.text = format!(
                            "Copied {} group{} from {} · Paste ⌘V",
                            clipboard.groups.len(),
                            if clipboard.groups.len() == 1 { "" } else { "s" },
                            clipboard.source.name
                        );
                        self.view_state.copy_settings.clipboard = Some(clipboard);
                    }
                    Err(reason) => self.status.text = reason,
                }
            }
            C::Paste => {
                let targets = self.paste_targets();
                return self.copy_settings_update(C::PasteTo(targets));
            }
            C::PasteTo(targets) => {
                self.view_state.copy_settings.cell_menu = None;
                self.select.state.catalog.menu = None;
                self.select.state.catalog.context_at = None;
                if let Some(reason) = self.paste_refusal(false) {
                    self.status.text = reason;
                    return Task::none();
                }
                let Some(clipboard) = self.view_state.copy_settings.clipboard.clone() else {
                    return Task::none();
                };
                if targets.count() > 1 {
                    let skip = self.white_balance_notice(&clipboard);
                    self.view_state.copy_settings.confirm = Some(Confirmation {
                        clipboard,
                        targets,
                        skip,
                    });
                } else {
                    return self.paste_to(clipboard, targets);
                }
            }
            C::Confirm => {
                let Some(confirm) = self.view_state.copy_settings.confirm.take() else {
                    return Task::none();
                };
                if confirm.targets != self.paste_targets() {
                    self.status.text = "The selection changed; confirm the new selection".into();
                    return self.copy_settings_update(C::Paste);
                }
                if let Some(reason) = self.paste_refusal(false) {
                    self.status.text = reason;
                    return Task::none();
                }
                return self.paste_to(confirm.clipboard, confirm.targets);
            }
        }
        Task::none()
    }

    fn white_balance_notice(&self, clipboard: &Clipboard) -> String {
        if !clipboard.settings.values().any(|values| {
            values.get("temperature").is_some()
                || values.get("tint").is_some()
                || values.get("white-balance").is_some()
        }) {
            return String::new();
        }
        if self.select.state.shown == Shown::Select {
            let selection = SelectionModel::of(&self.session.browse, self.select.state.revision());
            if let Some(rows) =
                crate::state::select_catalog::selected_rows(&self.select.state, &selection)
            {
                let count = rows.iter().filter(|row| row.kind != clipboard.kind).count();
                return if count == 0 {
                    String::new()
                } else {
                    format!(
                        "White balance will be skipped on {count} photographs of a different source kind; they keep their own."
                    )
                };
            }
        }
        if self.select.state.shown != Shown::Select
            && let Some(set) = &self.develop.state.set
        {
            let kinds: Option<Vec<_>> = set
                .photos
                .iter()
                .enumerate()
                .filter(|(index, _)| set.selected(*index))
                .map(|(_, photo)| photo.kind)
                .collect();
            if let Some(kinds) = kinds {
                let count = kinds.iter().filter(|kind| **kind != clipboard.kind).count();
                return if count == 0 {
                    String::new()
                } else {
                    format!(
                        "White balance will be skipped on {count} photographs of a different source kind; they keep their own."
                    )
                };
            }
        }
        "White balance is skipped on photographs of a different source kind; each keeps its own. The report identifies any skips.".into()
    }

    pub(crate) fn copy_settings_summary(&self) -> Value {
        let state = &self.view_state.copy_settings;
        json!({
            "pending": state.pending,
            "clipboard": state.clipboard.as_ref().map(|clip| json!({"asset": clip.source.asset, "entry": clip.source.entry, "name": clip.source.name, "kind": clip.kind, "groups": clip.labels(), "settings": clip.settings, "copied_at": clip.copied_at})),
            "chooser": state.chooser.as_ref().map(|chooser| json!({"name": chooser.source.name, "entry": chooser.inspected_entry, "kind": chooser.kind, "groups": chooser.groups.iter().map(|row| json!({"label":row.group.label,"custom":row.custom,"reason":row.reason,"checked":chooser.form.is_checked(&row.group)})).collect::<Vec<_>>() })),
            "confirmation": state.confirm.as_ref().map(|confirm| json!({"count": confirm.targets.count(),"source":confirm.clipboard.source.name,"skip":confirm.skip})),
            "previous": state.previous.as_ref().map(|source| &source.asset),
            "request": state.request,
            "paste_refusal": self.workspace.copy_settings.paste_refusal,
        })
    }

    fn capture_settings(
        &mut self,
        source: Source,
        choose: bool,
        previous: bool,
        form: Option<PresetForm>,
    ) -> Task<Message> {
        if self.view_state.copy_settings.pending {
            return Task::none();
        }
        let groups = copyable_groups(&self.modules, self.developer);
        let form = form.unwrap_or_else(|| self.view_state.copy_settings.form());
        self.view_state.copy_settings.serial += 1;
        self.view_state.copy_settings.pending = true;
        let serial = self.view_state.copy_settings.serial;
        let (owner, client) = (self.owner.clone(), self.client);
        if choose {
            let modules = self.modules.clone();
            owner_task(
                move || inspect_now(&owner, client, source, groups, form, &modules),
                move |result| message(C::Inspected { serial, result }),
            )
        } else {
            self.view_state.copy_settings.request = Some(
                json!({"method": "preset.capture", "params": {"asset_id": source.asset, "entry_id": source.entry, "fields": capture_fields(&groups, &form)}}),
            );
            owner_task(
                move || capture_now(&owner, client, source, &groups, &form),
                move |result| {
                    message(C::Captured {
                        serial,
                        result,
                        previous,
                    })
                },
            )
        }
    }

    fn paste_single(&mut self, clipboard: &Clipboard) -> Task<Message> {
        if let Some(reason) = self.action_refusal("apply-settings") {
            self.status.text = reason;
            return Task::none();
        }
        match self.request("apply-settings", &clipboard.parameters()) {
            Ok((method, params)) => {
                self.view_state.copy_settings.request =
                    Some(json!({"method": method, "params": params}));
                if let Some(request_id) = params["mutation"]["request_id"].as_str() {
                    self.view_state.copy_settings.paste_request =
                        Some((request_id.into(), Arc::new(clipboard.clone())));
                }
                self.command(method, params)
            }
            Err(reason) => {
                self.status.text = reason;
                Task::none()
            }
        }
    }

    fn paste_to(&mut self, clipboard: Arc<Clipboard>, targets: Targets) -> Task<Message> {
        if let Targets::Assets(ids) = &targets
            && ids.len() == 1
            && self
                .document
                .state
                .as_ref()
                .is_some_and(|state| state.asset.id == ids[0])
            && self.select.state.shown == Shown::Develop
        {
            return self.paste_single(&clipboard);
        }
        let params = json!({"targets": targets.params(), "settings": clipboard.settings, "origin": clipboard.origin(), "mutation": request()});
        self.start_settings_batch(
            BatchKind::Paste {
                source: clipboard.source.name.clone(),
            },
            params,
            targets,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::import_and_adopt;
    use luxforge_testbase::paths;
    /// The copied groups that capture `action`'s `field`, from the registry's own groups.
    fn group_with(action: &str, field: &str) -> PresettableGroup {
        let modules = luxforge_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        copyable_groups(&modules, false)
            .into_iter()
            .find(|group| {
                group
                    .fields
                    .iter()
                    .any(|(named, fields)| named == action && fields.iter().any(|f| f == field))
            })
            .unwrap_or_else(|| panic!("a group capturing {action}.{field}"))
    }

    /// A paste's status reads its answer: the outcome and the skipped settings say how many of the
    /// copied groups applied and which were skipped, and a no-op says nothing changed.
    #[test]
    fn copy_settings_status_correlates_the_mutation_and_reads_applied_and_skipped_groups() {
        use crate::app::{message::sync::SyncMessage, tasks, testing};
        let catalog = paths::temp_catalog("copy-settings-status");
        let (mut editor, asset, _) = testing::real_photo_at(&catalog, &paths::jpeg());
        let clipboard = Clipboard {
            source: Source {
                asset: asset.clone(),
                entry: None,
                name: "source.NEF".into(),
            },
            kind: luxforge_core::SourceTag::Raw,
            settings: json!({"set-basic":{"exposure":0.6}, "set-raw":{"white-balance":"as-shot"}})
                .as_object()
                .unwrap()
                .clone(),
            groups: vec![
                group_with("set-basic", "exposure"),
                group_with("set-basic", "temperature"),
            ],
            copied_at: 0,
            capture: Value::Null,
        };
        let target = paths::jpeg()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for unchanged in [false, true] {
            let _ = editor.paste_single(&clipboard);
            let sent = editor.view_state.copy_settings.request.clone().unwrap();
            assert_eq!(sent["method"], "edit.apply-settings");
            assert_eq!(
                sent["params"]["origin"],
                json!({"kind": "paste", "source": "source.NEF", "source_asset": asset})
            );
            let refresh = tasks::command_now(
                &editor.owner,
                editor.client,
                asset.clone(),
                "edit.apply-settings",
                sent["params"].clone(),
                None,
            )
            .unwrap();
            assert_eq!(
                refresh.mutation_request.as_deref(),
                sent["params"]["mutation"]["request_id"].as_str()
            );
            assert_ne!(
                refresh.mutation_request, refresh.request,
                "transport and mutation identities are distinct"
            );
            let reason = refresh.skipped[0].reason.clone();
            let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
            let Some(crate::state::status::Happened::Pasted(sentence)) = &editor.status.happened
            else {
                panic!(
                    "paste status was not retained: {:?}",
                    editor.status.happened
                );
            };
            let expected = if unchanged {
                format!(
                    "Nothing changed: {target} already has the settings that apply \u{b7} White balance skipped: {reason}"
                )
            } else {
                format!(
                    "Pasted 1 of 2 groups from source.NEF \u{b7} White balance skipped: {reason} \u{b7} Undo \u{2318}Z"
                )
            };
            assert_eq!(sentence, &expected);
            assert_eq!(editor.document.state.as_ref().unwrap().revision, 1);
            assert!(editor.view_state.copy_settings.paste_request.is_none());
        }
        testing::finish(editor, catalog);
    }

    /// Pasting from a photograph whose file name is as long as a platform allows works, and the
    /// history label shortens the name in the middle.
    #[test]
    fn copy_settings_pastes_from_a_source_with_the_longest_file_name() {
        use crate::app::{tasks, testing};
        let catalog = paths::temp_catalog("copy-settings-long-name");
        let (mut editor, asset, _) = testing::real_photo_at(&catalog, &paths::jpeg());
        let name = format!("{}.NEF", "a".repeat(251));
        assert_eq!(name.len(), 255, "the longest file name a platform allows");
        let clipboard = Clipboard {
            source: Source {
                asset: asset.clone(),
                entry: None,
                name: name.clone(),
            },
            kind: luxforge_core::SourceTag::Raw,
            settings: json!({"set-basic":{"exposure":0.6}})
                .as_object()
                .unwrap()
                .clone(),
            groups: vec![group_with("set-basic", "exposure")],
            copied_at: 0,
            capture: Value::Null,
        };
        let _ = editor.paste_single(&clipboard);
        let sent = editor.view_state.copy_settings.request.clone().unwrap();
        assert_eq!(sent["params"]["origin"]["source"], json!(name));
        tasks::command_now(
            &editor.owner,
            editor.client,
            asset.clone(),
            "edit.apply-settings",
            sent["params"].clone(),
            None,
        )
        .expect("a 255-byte file name pastes");
        let (state, _) = call(
            &editor.owner,
            editor.client,
            "asset.state",
            json!({"asset_id": asset}),
        )
        .unwrap();
        let label = state["current_entry"]["label"].as_str().unwrap();
        assert!(
            label.starts_with("Paste settings from aaaa") && label.ends_with("aaa.NEF"),
            "{label}"
        );
        assert!(label.contains('\u{2026}'), "{label}");
        testing::finish(editor, catalog);
    }

    #[test]
    fn copy_settings_capture_is_a_snapshot_and_captions_read_the_named_entry() {
        let path = paths::temp_catalog("copy-settings-capture");
        let (owner, join) = OwnerHandle::start(&path).unwrap();
        let client = owner.register();
        let asset = import_and_adopt(&owner, client, &paths::jpeg());
        let (state, _) = call(&owner, client, "asset.state", json!({"asset_id": asset})).unwrap();
        let original = serde_json::from_value(state["current_entry"]["id"].clone()).unwrap();
        call(&owner, client, "edit.set-basic", json!({"asset_id": asset, "exposure": 1, "mutation": {"expected_revision": 0, "request_id": "expose", "actor": "copy-test"}})).unwrap();
        let modules = luxforge_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let groups = copyable_groups(&modules, false);
        let source = Source {
            asset: asset.clone(),
            entry: None,
            name: "source.jpg".into(),
        };
        let copied = capture_now(
            &owner,
            client,
            source.clone(),
            &groups,
            &PresetForm::default(),
        )
        .unwrap();
        assert_eq!(copied.settings["set-basic"]["exposure"].as_f64(), Some(1.0));
        let (independent, _) = call(&owner, client, "preset.capture", json!({"asset_id": asset, "entry_id": copied.source.entry, "fields": capture_fields(&groups, &PresetForm::default())})).unwrap();
        assert_eq!(independent["settings"], json!(copied.settings));
        let chooser = inspect_now(
            &owner,
            client,
            source.clone(),
            groups.clone(),
            PresetForm::default(),
            &modules,
        )
        .unwrap();
        assert!(
            chooser
                .groups
                .iter()
                .find(|row| row.group.label == "Basic · Tone")
                .unwrap()
                .custom
        );
        let historic = Source {
            entry: Some(original),
            ..source
        };
        let historic =
            capture_now(&owner, client, historic, &groups, &PresetForm::default()).unwrap();
        assert_eq!(
            historic.settings["set-basic"]["exposure"].as_f64(),
            Some(0.0)
        );
        call(&owner, client, "edit.set-basic", json!({"asset_id": asset, "exposure": -1, "mutation": {"expected_revision": 1, "request_id": "change", "actor": "copy-test"}})).unwrap();
        assert_eq!(copied.settings["set-basic"]["exposure"].as_f64(), Some(1.0));
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
