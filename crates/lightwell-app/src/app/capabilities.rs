//! The desktop's module capability driver. A gesture on a capability section, on a task control or
//! on the consent notice becomes one [`Operation`], and one owner round trip runs it off the update
//! loop through the same methods the JSON API dispatches: the `module.*` methods, `task.<id>` and
//! `module.permission.*`. Every round trip ends with one `module.status` read of that module alone,
//! which is the narrowest completion path that keeps the section honest; settings writes answer
//! with the fresh settings themselves, so they need no second read.
//!
//! Jobs are followed by a poll that exists only while a job the desktop tracks is queued or running
//! (see [`Editor::capability_poll_subscription`]). A `consent-required` answer opens the consent
//! notice; Allow grants exactly the scope the core named, with this client's permission authority,
//! and retries the refused operation once. Every request the desktop records is redacted first.
use crate::{
    app::{
        Editor,
        evidence::{CapabilityAction, CapabilityStep, Settle},
        message::{CapabilityMessage, Message},
        tasks::{REQUEST_NUMBER, mutation, request},
    },
    state::{
        capabilities::{
            CapabilityView, Consent, FieldKey, ModuleCapabilities, ModuleStatus, OpenConsent,
            Operation, SecretText, TaskPhase, TaskRun, declares, task_profile,
        },
        tools,
    },
};
use iced::{Subscription, Task};
use lightwell_core::{
    ApiRequest, AssetId, ClientId, ModuleDescriptor, OwnerHandle,
    capabilities::{
        descriptor::SettingKind,
        host::Requirement,
        jobs::{JobRecord, JobStatus},
        settings::SettingsRead,
    },
    redact_params,
};
use serde_json::{Map, Value, json};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// How often the tracked live jobs are read. The worker posts its completions into the owner's
/// channel, but no client is pushed anything: a client that wants a job's progress reads it. So
/// the desktop reads its live jobs every 100 ms, and only while one is queued or running — with no
/// live job there is no timer at all, which keeps an idle desktop asleep (performance rule 8). A
/// read is one owner lookup per job, and a progress bar that moves ten times a second is as smooth
/// as a job's own reports, which a worker makes between chunks of real work.
pub(crate) const JOB_POLL: Duration = Duration::from_millis(100);

/// How many times a task retries after its source or artifacts were prepared.
const PREPARATION_RETRIES: usize = 4;

/// An owner failure with its structured data kept, which a `consent-required` or `not-ready`
/// answer needs and a plain message would lose.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
    pub(crate) job_id: Option<String>,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// What one round trip came to.
#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    Done(Value),
    /// The core asked for consent before any work started: open the notice, and retry this
    /// operation once if the person allows it.
    Consent {
        consent: Box<Consent>,
        retry: Box<Operation>,
    },
    NotReady {
        requirements: Vec<Requirement>,
        message: String,
    },
    /// A settings write met a newer revision; the settings were read again.
    Conflict(String),
    Failed(CallError),
}

/// One finished round trip, back on the update loop.
#[derive(Clone, Debug)]
pub(crate) struct Answer {
    pub(crate) module_id: String,
    /// The operation the outcome answers. After Allow it is the retried operation, so the answer
    /// is routed exactly as a first try would be.
    pub(crate) op: Operation,
    /// The notice Allow or Don't allow answered, when this round trip was one.
    pub(crate) consent: Option<(bool, Box<Consent>)>,
    pub(crate) outcome: Outcome,
    pub(crate) settings: Option<SettingsRead>,
    pub(crate) status: Option<Result<ModuleStatus, String>>,
    /// The job the operation started, read once after it was queued.
    pub(crate) job: Option<JobRecord>,
    /// Every request sent, redacted, for the event log.
    pub(crate) sent: Vec<Value>,
}

/// One request through the owner's method table, recorded redacted before it is sent.
fn call(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
    sent: &mut Vec<Value>,
) -> Result<Value, CallError> {
    sent.push(json!({"method": method, "params": redact_params(method, &params)}));
    let request = ApiRequest {
        id: format!("ui-{}", REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)),
        method: method.into(),
        params,
        token: None,
    };
    let response = owner.call(client, request).map_err(|error| CallError {
        code: error.kind.code().into(),
        message: error.detail,
        data: None,
        job_id: None,
    })?;
    match response.error {
        Some(error) => Err(CallError {
            code: error.code,
            message: error.message,
            data: error.data,
            job_id: error.job_id,
        }),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// The envelope of every settings write: the revision it was made against and a fresh request id.
fn envelope(module_id: &str, profile: Option<&String>, revision: u64) -> Map<String, Value> {
    let mut params = Map::new();
    params.insert("module_id".into(), json!(module_id));
    if let Some(profile) = profile {
        params.insert("profile_id".into(), json!(profile));
    }
    params.insert("mutation".into(), json!(mutation(revision)));
    params
}

/// What a failure means for the section: consent, missing requirements, or a plain failure.
fn classify(error: CallError, retry: &Operation) -> Outcome {
    match error.code.as_str() {
        "consent-required" => {
            match error
                .data
                .as_ref()
                .and_then(|data| data.get("consent"))
                .cloned()
                .map(parse::<Consent>)
            {
                Some(Ok(consent)) => Outcome::Consent {
                    consent: Box::new(consent),
                    retry: Box::new(retry.clone()),
                },
                _ => Outcome::Failed(error),
            }
        }
        "not-ready" => Outcome::NotReady {
            requirements: error
                .data
                .as_ref()
                .and_then(|data| data.get("requirements"))
                .cloned()
                .and_then(|requirements| parse(requirements).ok())
                .unwrap_or_default(),
            message: error.message,
        },
        _ => Outcome::Failed(error),
    }
}

/// Wait for a source or artifact preparation job to leave the queue, as a preview does.
fn wait_preparation(
    owner: &OwnerHandle,
    client: ClientId,
    job_id: &str,
    sent: &mut Vec<Value>,
) -> Result<(), CallError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = call(owner, client, "job.status", json!({"job_id": job_id}), sent)?;
        match status["status"].as_str() {
            Some("queued" | "running") if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Some("failed") => {
                return Err(CallError {
                    code: status["error"]["code"]
                        .as_str()
                        .unwrap_or("internal")
                        .into(),
                    message: status["error"]["message"]
                        .as_str()
                        .unwrap_or("preparation failed")
                        .into(),
                    data: None,
                    job_id: None,
                });
            }
            _ => return Ok(()),
        }
    }
}

/// Run one operation's own requests and say what they came to. Settings writes put the fresh
/// settings they answer with into `settings`.
fn perform(
    owner: &OwnerHandle,
    client: ClientId,
    module_id: &str,
    op: &Operation,
    sent: &mut Vec<Value>,
    settings: &mut Option<SettingsRead>,
) -> Outcome {
    let mut write = |method: &str, params: Map<String, Value>, sent: &mut Vec<Value>| {
        call(owner, client, method, Value::Object(params), sent).map(|answer| {
            *settings = parse(answer["settings"].clone()).ok();
            answer
        })
    };
    let result = match op {
        Operation::Load => call(
            owner,
            client,
            "module.settings.read",
            json!({"module_id": module_id}),
            sent,
        )
        .map(|read| {
            *settings = parse(read).ok();
            Value::Null
        }),
        Operation::Refresh => Ok(Value::Null),
        Operation::Set {
            profile,
            field,
            value,
            revision,
        } => {
            let mut params = envelope(module_id, profile.as_ref(), *revision);
            params.insert("values".into(), json!({field.as_str(): value}));
            write("module.settings.set", params, sent)
        }
        Operation::SetSecret {
            profile,
            field,
            value,
            revision,
        } => {
            let mut params = envelope(module_id, profile.as_ref(), *revision);
            params.insert("setting".into(), json!(field));
            params.insert("value".into(), json!(value.expose()));
            write("module.settings.set-secret", params, sent)
        }
        Operation::ClearSecret {
            profile,
            field,
            revision,
        } => {
            let mut params = envelope(module_id, profile.as_ref(), *revision);
            params.insert("setting".into(), json!(field));
            write("module.settings.clear-secret", params, sent)
        }
        Operation::CreateProfile {
            adapter,
            label,
            revision,
        } => {
            let mut params = envelope(module_id, None, *revision);
            params.insert("adapter".into(), json!(adapter));
            params.insert("label".into(), json!(label));
            write("module.profile.create", params, sent)
        }
        Operation::RemoveProfile { profile, revision } => {
            let mut params = envelope(module_id, None, *revision);
            params.insert("profile_id".into(), json!(profile));
            write("module.profile.remove", params, sent)
        }
        Operation::Install { resource } => call(
            owner,
            client,
            "module.resource.install",
            json!({"module_id": module_id, "resource_id": resource, "mutation": request()}),
            sent,
        ),
        Operation::Remove { resource } => call(
            owner,
            client,
            "module.resource.remove",
            json!({"module_id": module_id, "resource_id": resource, "mutation": request()}),
            sent,
        ),
        Operation::Activate => call(
            owner,
            client,
            "module.activate",
            json!({"module_id": module_id, "mutation": request()}),
            sent,
        ),
        Operation::Deactivate => call(
            owner,
            client,
            "module.deactivate",
            json!({"module_id": module_id, "mutation": request()}),
            sent,
        ),
        Operation::Cancel { job } => call(
            owner,
            client,
            "module.job.cancel",
            json!({"job_id": job, "mutation": request()}),
            sent,
        ),
        Operation::Revoke { grant } => call(
            owner,
            client,
            "module.permission.revoke",
            json!({"grant_id": grant, "mutation": request()}),
            sent,
        ),
        Operation::RunTask {
            task,
            asset,
            profile,
        } => {
            let mut params = Map::new();
            if let Some(asset) = asset {
                params.insert("asset_id".into(), json!(asset));
            }
            if let Some(profile) = profile {
                params.insert("profile_id".into(), json!(profile));
            }
            let method = format!("task.{task}");
            let mut attempt = 0;
            loop {
                match call(owner, client, &method, Value::Object(params.clone()), sent) {
                    // The source or an artifact is not prepared yet: wait for that job and ask
                    // again, exactly as a preview does.
                    Err(error)
                        if error.code == "preparation-required"
                            && attempt < PREPARATION_RETRIES =>
                    {
                        attempt += 1;
                        let job = error
                            .job_id
                            .clone()
                            .unwrap_or_else(|| error.message.clone());
                        if let Err(error) = wait_preparation(owner, client, &job, sent) {
                            break Err(error);
                        }
                    }
                    other => break other,
                }
            }
        }
        Operation::Consent {
            allow,
            consent,
            retry,
        } => {
            // Allow and Don't allow are one press each, so each is a new request.
            let answer = json!({
                "module_id": consent.module_id,
                "capability": consent.capability,
                "scope": consent.scope,
                "mutation": request(),
            });
            if !*allow {
                return match call(owner, client, "module.permission.deny", answer, sent) {
                    Ok(answer) => Outcome::Done(answer),
                    Err(error) => Outcome::Failed(error),
                };
            }
            if let Err(error) = call(owner, client, "module.permission.grant", answer, sent) {
                return Outcome::Failed(error);
            }
            return match retry {
                // Exactly once: a retry that asks for another consent opens a new notice.
                Some(retry) => perform(owner, client, module_id, retry, sent, settings),
                None => Outcome::Done(Value::Null),
            };
        }
    };
    match result {
        Ok(answer) => Outcome::Done(answer),
        Err(error) if error.code == "conflict" && op.field().is_some() => {
            // Somebody else changed the settings: read them again and say so at the field.
            if let Ok(read) = call(
                owner,
                client,
                "module.settings.read",
                json!({"module_id": module_id}),
                sent,
            ) {
                *settings = parse(read).ok();
            }
            Outcome::Conflict(error.message)
        }
        Err(error) => classify(error, op),
    }
}

/// One whole round trip: the operation, one read of a job it started, and the module's status.
pub(crate) fn run(
    owner: &OwnerHandle,
    client: ClientId,
    module_id: String,
    op: Operation,
) -> Answer {
    let mut sent = Vec::new();
    let mut settings = None;
    let outcome = perform(owner, client, &module_id, &op, &mut sent, &mut settings);
    // After Allow the answer is the retried operation's, and is routed as that operation.
    let (routed, consent) = match &op {
        Operation::Consent {
            allow,
            consent,
            retry,
        } => (
            match (allow, retry) {
                (true, Some(retry)) => (**retry).clone(),
                _ => op.clone(),
            },
            Some((*allow, consent.clone())),
        ),
        _ => (op.clone(), None),
    };
    let job = match &outcome {
        Outcome::Done(answer) => answer["job_id"].as_str().and_then(|job| {
            call(
                owner,
                client,
                "module.job.read",
                json!({"job_id": job}),
                &mut sent,
            )
            .ok()
            .and_then(|record| parse::<JobRecord>(record).ok())
        }),
        _ => None,
    };
    let status = call(
        owner,
        client,
        "module.status",
        json!({"module_id": module_id}),
        &mut sent,
    )
    .map_err(|error| error.to_string())
    .and_then(parse::<ModuleStatus>);
    Answer {
        module_id,
        op: routed,
        consent,
        outcome,
        settings,
        status: Some(status),
        job,
        sent,
    }
}

/// Read each live job once.
pub(crate) fn poll(
    owner: &OwnerHandle,
    client: ClientId,
    jobs: Vec<(String, String)>,
) -> Vec<(String, String, Result<JobRecord, String>)> {
    let mut sent = Vec::new();
    jobs.into_iter()
        .map(|(module, job)| {
            let record = call(
                owner,
                client,
                "module.job.read",
                json!({"job_id": job}),
                &mut sent,
            )
            .map_err(|error| error.to_string())
            .and_then(parse::<JobRecord>);
            (module, job, record)
        })
        .collect()
}

impl Editor {
    /// Start one owner round trip for a module. Nothing runs on the update loop but this.
    pub(crate) fn capability_op(&mut self, module_id: &str, op: Operation) -> Task<Message> {
        self.capabilities.touch(module_id).pending += 1;
        #[cfg(test)]
        self.capability_started
            .push((module_id.to_owned(), op.clone()));
        let owner = self.owner.clone();
        let client = self.client;
        let module = module_id.to_owned();
        Task::perform(async move { run(&owner, client, module, op) }, |answer| {
            Message::Capability(CapabilityMessage::Answered(Box::new(answer)))
        })
    }

    /// Read settings and status for every expanded capability section that has never been read,
    /// the first time it is shown, or `None` when there is none. Discovery reads nothing; a
    /// section that is never opened costs nothing.
    pub(crate) fn request_capability_loads(&mut self) -> Option<Task<Message>> {
        let wanted: Vec<String> = self
            .workspace
            .tools
            .all()
            .filter(|section| section.expanded && section.capability.is_some())
            .map(|section| section.module_id.clone())
            .filter(|module| {
                self.capabilities.modules.get(module).is_none_or(|state| {
                    state.settings.is_none()
                        && state.status.is_none()
                        && state.load_error.is_none()
                        && state.pending == 0
                })
            })
            .collect();
        (!wanted.is_empty()).then(|| {
            Task::batch(
                wanted
                    .into_iter()
                    .map(|module| self.capability_op(&module, Operation::Load))
                    .collect::<Vec<_>>(),
            )
        })
    }

    /// Read settings and status again for every loaded capability module, after another client
    /// changed something through a capability method.
    pub(crate) fn reload_capabilities(&mut self) -> Task<Message> {
        let loaded: Vec<String> = self
            .capabilities
            .modules
            .iter()
            .filter(|(_, state)| state.settings.is_some() || state.status.is_some())
            .map(|(module, _)| module.clone())
            .collect();
        Task::batch(
            loaded
                .into_iter()
                .map(|module| self.capability_op(&module, Operation::Load))
                .collect::<Vec<_>>(),
        )
    }

    /// The job poll's timer, which exists only while a tracked job is queued or running.
    pub(crate) fn capability_poll_subscription(&self) -> Option<Subscription<Message>> {
        self.capabilities.live().then(|| {
            iced::time::every(JOB_POLL).map(|_| Message::Capability(CapabilityMessage::Poll))
        })
    }

    /// A task run's result belongs to the asset it ran for: another asset clears every run.
    pub(crate) fn capabilities_asset_changed(&mut self, asset: &AssetId) {
        let stale: Vec<String> = self
            .capabilities
            .modules
            .iter()
            .filter(|(_, state)| {
                state
                    .tasks
                    .values()
                    .any(|run| run.asset.as_ref().is_some_and(|ran| ran != asset))
            })
            .map(|(module, _)| module.clone())
            .collect();
        for module in stale {
            self.capabilities
                .touch(&module)
                .tasks
                .retain(|_, run| run.asset.as_ref().is_none_or(|ran| ran == asset));
        }
    }

    fn capability_module(&self, module_id: &str) -> Option<&ModuleDescriptor> {
        tools::module_of(&self.modules, module_id).filter(|module| declares(module))
    }

    /// The settings revision a write is made against, or the reason no write can be made yet.
    fn settings_revision(&self, module_id: &str) -> Result<u64, String> {
        let state = self.capabilities.modules.get(module_id);
        if state.is_some_and(|state| state.pending > 0) {
            return Err("Waiting for the last change".into());
        }
        state
            .and_then(ModuleCapabilities::revision)
            .ok_or_else(|| "The module's settings have not been read yet".into())
    }

    /// A settings field's declared kind, module-level or of a profile block.
    fn setting_kind(
        &self,
        module_id: &str,
        profile: Option<&String>,
        field: &str,
    ) -> Option<SettingKind> {
        let settings = self.capability_module(module_id)?.settings.as_ref()?;
        match profile {
            None => settings.field(field),
            Some(_) => settings.profiles.as_ref()?.field(field),
        }
        .map(|declared| declared.kind.clone())
    }

    /// A settings write, or the reason it cannot be sent, shown under the field.
    fn write_setting(
        &mut self,
        module_id: &str,
        key: FieldKey,
        build: impl FnOnce(u64) -> Operation,
    ) -> Task<Message> {
        match self.settings_revision(module_id) {
            Ok(revision) => self.capability_op(module_id, build(revision)),
            Err(reason) => {
                self.capabilities
                    .touch(module_id)
                    .errors
                    .insert(key, reason.clone());
                self.status = reason;
                Task::none()
            }
        }
    }

    pub(crate) fn capability_update(&mut self, message: CapabilityMessage) -> Task<Message> {
        let task = self.capability_message(message);
        self.capability_settle();
        task
    }

    fn capability_message(&mut self, message: CapabilityMessage) -> Task<Message> {
        match message {
            CapabilityMessage::Show { module_id, view } => {
                let state = self.capabilities.touch(&module_id);
                state.view = view;
                if view == CapabilityView::Status {
                    state.secret = None;
                }
            }
            CapabilityMessage::TogglePermissions(module_id) => {
                let state = self.capabilities.touch(&module_id);
                state.permissions_open = !state.permissions_open;
            }
            CapabilityMessage::FieldText {
                module_id,
                profile,
                field,
                text,
            } => {
                self.capabilities
                    .touch(&module_id)
                    .edits
                    .insert((profile, field), text);
            }
            CapabilityMessage::FieldCommit {
                module_id,
                profile,
                field,
            } => {
                let key = (profile.clone(), field.clone());
                let Some(text) = self
                    .capabilities
                    .modules
                    .get(&module_id)
                    .and_then(|state| state.edits.get(&key))
                    .cloned()
                else {
                    return Task::none();
                };
                let value = match self.setting_kind(&module_id, profile.as_ref(), &field) {
                    Some(SettingKind::Number { min, max, .. }) => text
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|value| value.is_finite())
                        .map(Value::from)
                        .ok_or_else(|| format!("Enter a number from {min} to {max}")),
                    Some(SettingKind::Integer { min, max }) => text
                        .trim()
                        .parse::<i64>()
                        .map(Value::from)
                        .map_err(|_| format!("Enter a whole number from {min} to {max}")),
                    // An emptied endpoint returns the field to having no value.
                    Some(SettingKind::Endpoint { .. }) if text.trim().is_empty() => Ok(Value::Null),
                    Some(SettingKind::Endpoint { .. }) => Ok(Value::from(text.trim())),
                    Some(SettingKind::Text { .. }) => Ok(Value::from(text)),
                    Some(_) => Err(format!("{field} is not a typed field")),
                    None => Err(format!("{module_id} declares no setting {field}")),
                };
                match value {
                    Ok(value) => {
                        return self.write_setting(&module_id, key, |revision| Operation::Set {
                            profile,
                            field,
                            value,
                            revision,
                        });
                    }
                    Err(reason) => {
                        self.capabilities
                            .touch(&module_id)
                            .errors
                            .insert(key, reason.clone());
                        self.status = reason;
                    }
                }
            }
            CapabilityMessage::FieldValue {
                module_id,
                profile,
                field,
                value,
            } => {
                let key = (profile.clone(), field.clone());
                return self.write_setting(&module_id, key, |revision| Operation::Set {
                    profile,
                    field,
                    value,
                    revision,
                });
            }
            CapabilityMessage::SecretEdit {
                module_id,
                profile,
                field,
            } => {
                self.capabilities.touch(&module_id).secret =
                    Some(((profile, field), SecretText::default()));
            }
            CapabilityMessage::SecretText { module_id, text } => {
                if let Some((_, typed)) = &mut self.capabilities.touch(&module_id).secret {
                    *typed = text;
                }
            }
            CapabilityMessage::SecretCancel(module_id) => {
                self.capabilities.touch(&module_id).secret = None;
            }
            CapabilityMessage::SecretCommit(module_id) => {
                let Some(((profile, field), value)) = self
                    .capabilities
                    .modules
                    .get(&module_id)
                    .and_then(|state| state.secret.clone())
                else {
                    return Task::none();
                };
                let key = (profile.clone(), field.clone());
                return self.write_setting(&module_id, key, |revision| Operation::SetSecret {
                    profile,
                    field,
                    value,
                    revision,
                });
            }
            CapabilityMessage::SecretClear {
                module_id,
                profile,
                field,
            } => {
                let key = (profile.clone(), field.clone());
                return self.write_setting(&module_id, key, |revision| Operation::ClearSecret {
                    profile,
                    field,
                    revision,
                });
            }
            CapabilityMessage::ProfileAdapter { module_id, adapter } => {
                self.capabilities.touch(&module_id).profile_adapter = Some(adapter);
            }
            CapabilityMessage::ProfileLabel { module_id, label } => {
                self.capabilities.touch(&module_id).profile_label = label;
            }
            CapabilityMessage::ProfileCreate(module_id) => {
                let Some(adapters) = self
                    .capability_module(&module_id)
                    .and_then(|module| module.settings.as_ref())
                    .and_then(|settings| settings.profiles.as_ref())
                    .map(|profiles| profiles.adapters.clone())
                else {
                    self.status = format!("{module_id} declares no profiles");
                    return Task::none();
                };
                let state = self.capabilities.modules.get(&module_id);
                let label = state
                    .map(|state| state.profile_label.trim().to_owned())
                    .unwrap_or_default();
                let adapter = state
                    .and_then(|state| state.profile_adapter.clone())
                    .or_else(|| adapters.first().map(|adapter| adapter.id.clone()));
                let (Some(adapter), false) = (adapter, label.is_empty()) else {
                    self.status = "Name the profile first".into();
                    return Task::none();
                };
                let revision = match self.settings_revision(&module_id) {
                    Ok(revision) => revision,
                    Err(reason) => {
                        self.status = reason;
                        return Task::none();
                    }
                };
                return self.capability_op(
                    &module_id,
                    Operation::CreateProfile {
                        adapter,
                        label,
                        revision,
                    },
                );
            }
            CapabilityMessage::ProfileRemove { module_id, profile } => {
                let revision = match self.settings_revision(&module_id) {
                    Ok(revision) => revision,
                    Err(reason) => {
                        self.status = reason;
                        return Task::none();
                    }
                };
                return self
                    .capability_op(&module_id, Operation::RemoveProfile { profile, revision });
            }
            CapabilityMessage::Activate { module_id, on } => {
                let op = if on {
                    Operation::Activate
                } else {
                    Operation::Deactivate
                };
                return self.capability_op(&module_id, op);
            }
            CapabilityMessage::Install {
                module_id,
                resource,
            } => return self.capability_op(&module_id, Operation::Install { resource }),
            CapabilityMessage::Remove {
                module_id,
                resource,
            } => return self.capability_op(&module_id, Operation::Remove { resource }),
            CapabilityMessage::Cancel { module_id, job } => {
                return self.capability_op(&module_id, Operation::Cancel { job });
            }
            CapabilityMessage::Revoke { module_id, grant } => {
                return self.capability_op(&module_id, Operation::Revoke { grant });
            }
            CapabilityMessage::TaskProfile {
                module_id,
                task,
                profile,
            } => {
                self.capabilities
                    .touch(&module_id)
                    .task_profiles
                    .insert(task, profile);
            }
            CapabilityMessage::RunTask { module_id, task } => {
                return match self.task_operation(&module_id, &task) {
                    Ok(op) => {
                        let Operation::RunTask { asset, profile, .. } = &op else {
                            return Task::none();
                        };
                        let run = TaskRun {
                            asset: asset.clone(),
                            profile: profile.clone(),
                            phase: TaskPhase::Requesting,
                        };
                        self.capabilities.touch(&module_id).tasks.insert(task, run);
                        self.capability_op(&module_id, op)
                    }
                    Err(reason) => {
                        self.status = reason;
                        Task::none()
                    }
                };
            }
            CapabilityMessage::Apply { module_id, task } => {
                return match self.apply_preset(&module_id, &task) {
                    Ok((action, preset)) => self.dispatch(Message::RunAction { action, preset }),
                    Err(reason) => {
                        self.status = reason;
                        Task::none()
                    }
                };
            }
            CapabilityMessage::CopyTaskRequest { module_id, task } => {
                self.menu = None;
                return match self.task_operation(&module_id, &task) {
                    Ok(Operation::RunTask {
                        task,
                        asset,
                        profile,
                    }) => {
                        let mut params = Map::new();
                        if let Some(asset) = asset {
                            params.insert("asset_id".into(), json!(asset));
                        }
                        if let Some(profile) = profile {
                            params.insert("profile_id".into(), json!(profile));
                        }
                        let method = format!("task.{task}");
                        let params = redact_params(&method, &Value::Object(params));
                        self.status = format!("Copied the {method} request");
                        iced::clipboard::write(
                            serde_json::to_string_pretty(
                                &json!({"method": method, "params": params}),
                            )
                            .unwrap_or_default(),
                        )
                    }
                    Ok(_) => Task::none(),
                    Err(reason) => {
                        self.status = reason;
                        Task::none()
                    }
                };
            }
            CapabilityMessage::Consent(allow) => {
                let Some(OpenConsent { consent, retry }) = self.capabilities.consent.take() else {
                    return Task::none();
                };
                let module_id = consent.module_id.clone();
                return self.capability_op(
                    &module_id,
                    Operation::Consent {
                        allow,
                        consent: Box::new(consent),
                        retry: retry.map(Box::new),
                    },
                );
            }
            CapabilityMessage::Answered(answer) => return self.capability_answered(*answer),
            CapabilityMessage::Poll => {
                if self.capabilities.polling {
                    return Task::none();
                }
                let jobs = self.capabilities.live_jobs();
                if jobs.is_empty() {
                    return Task::none();
                }
                self.capabilities.polling = true;
                let owner = self.owner.clone();
                let client = self.client;
                return Task::perform(async move { poll(&owner, client, jobs) }, |polled| {
                    Message::Capability(CapabilityMessage::Polled(polled))
                });
            }
            CapabilityMessage::Polled(polled) => return self.capability_polled(polled),
        }
        Task::none()
    }

    /// The `task.<id>` operation a press on the task control sends right now: the open asset when
    /// the task takes one, and the profile it would use.
    fn task_operation(&self, module_id: &str, task: &str) -> Result<Operation, String> {
        let module = self
            .capability_module(module_id)
            .ok_or_else(|| format!("{module_id} declares no capabilities"))?;
        let declared = module
            .task(task)
            .ok_or_else(|| format!("{} declares no task {task}", module.title))?;
        let asset = match (declared.asset, &self.state) {
            (true, Some(state)) => Some(state.asset.id.clone()),
            (true, None) => return Err("No photograph is open".into()),
            (false, _) => None,
        };
        let empty = ModuleCapabilities::default();
        let state = self.capabilities.modules.get(module_id).unwrap_or(&empty);
        let profile = task_profile(state, task, declared.profile);
        if declared.profile && profile.is_none() {
            return Err("Add a profile in Settings first".into());
        }
        Ok(Operation::RunTask {
            task: task.to_owned(),
            asset,
            profile,
        })
    }

    /// The action and preset Apply commits: the task's declared apply action with its artifact.
    fn apply_preset(
        &self,
        module_id: &str,
        task: &str,
    ) -> Result<(String, Map<String, Value>), String> {
        let apply = self
            .capability_module(module_id)
            .and_then(|module| module.task(task))
            .and_then(|task| task.apply.clone())
            .ok_or_else(|| format!("{task} declares nothing to apply"))?;
        let asset = self.state.as_ref().map(|state| &state.asset.id);
        let artifact = self
            .capabilities
            .modules
            .get(module_id)
            .and_then(|state| state.tasks.get(task))
            .filter(|run| run.asset.is_none() || run.asset.as_ref() == asset)
            .and_then(|run| match &run.phase {
                TaskPhase::Succeeded { artifacts, .. } => artifacts.first().cloned(),
                _ => None,
            })
            .ok_or_else(|| "The task has no result to apply".to_owned())?;
        Ok((
            apply.action,
            [(apply.parameter, Value::from(artifact))]
                .into_iter()
                .collect(),
        ))
    }

    fn capability_answered(&mut self, answer: Answer) -> Task<Message> {
        let Answer {
            module_id,
            op,
            consent,
            outcome,
            settings,
            status,
            job,
            sent,
        } = answer;
        self.event(
            "capability_answer",
            json!({
                "module_id": module_id,
                "operation": op.name(),
                "consent": consent.as_ref().map(|(allow, consent)| json!({"allow": allow, "capability": consent.capability})),
                "outcome": match &outcome {
                    Outcome::Done(_) => json!("done"),
                    Outcome::Consent { consent, .. } => json!({"consent-required": consent.capability}),
                    Outcome::NotReady { requirements, .. } => json!({"not-ready": requirements}),
                    Outcome::Conflict(message) => json!({"conflict": message}),
                    Outcome::Failed(error) => json!({"failed": {"code": error.code, "message": error.message}}),
                },
                "job": job.as_ref().map(|job| json!({"job_id": job.job_id, "kind": job.kind, "status": job.status})),
                "requests": sent,
            }),
        );
        let state = self.capabilities.touch(&module_id);
        state.pending = state.pending.saturating_sub(1);
        if let Some(settings) = settings {
            state.settings = Some(settings);
        }
        match status {
            Some(Ok(status)) => {
                for record in &status.jobs {
                    state.track(record.clone(), false);
                }
                state.status = Some(status);
                if matches!(op, Operation::Load) {
                    state.load_error = None;
                }
            }
            Some(Err(error)) if matches!(op, Operation::Load) => state.load_error = Some(error),
            Some(Err(error)) => state.message = Some(error),
            None => {}
        }
        // A declined consent ends the run it was asked for.
        if let Some((false, declined)) = &consent
            && let Operation::RunTask { task, .. } = &op
            && let Some(run) = state.tasks.get_mut(task)
        {
            run.phase = TaskPhase::Failed {
                code: "consent-required".into(),
                message: format!("{} was not allowed", declined.capability),
            };
        }
        let field = op.field();
        match outcome {
            Outcome::Done(_) => {
                state.message = None;
                if matches!(
                    op,
                    Operation::Activate | Operation::Install { .. } | Operation::RunTask { .. }
                ) {
                    state.requirements.clear();
                }
                if let Some(key) = &field {
                    state.errors.remove(key);
                    state.conflicts.remove(key);
                    state.edits.remove(key);
                    if state.secret.as_ref().is_some_and(|(open, _)| open == key) {
                        state.secret = None;
                    }
                }
                if matches!(op, Operation::CreateProfile { .. }) {
                    state.profile_label.clear();
                }
                if let Some(record) = &job {
                    state.track(record.clone(), true);
                    if let Operation::RunTask { task, .. } = &op
                        && let Some(run) = state.tasks.get_mut(task)
                    {
                        run.phase = TaskPhase::Job(record.job_id.as_str().to_owned());
                    }
                }
            }
            Outcome::Consent { consent, retry } => {
                if let Operation::RunTask { task, .. } = &op
                    && let Some(run) = state.tasks.get_mut(task)
                {
                    run.phase = TaskPhase::Consent;
                }
                // The notice belongs to one module; opening it re-derives that module's section.
                let previous = self.capabilities.consent.replace(OpenConsent {
                    consent: *consent,
                    retry: Some(*retry),
                });
                if let Some(previous) = previous {
                    self.capabilities.touch(&previous.consent.module_id);
                }
            }
            Outcome::NotReady {
                requirements,
                message,
            } => {
                state.requirements = requirements;
                state.message = Some(format!("Not ready: {message}"));
                if let Operation::RunTask { task, .. } = &op
                    && let Some(run) = state.tasks.get_mut(task)
                {
                    run.phase = TaskPhase::NotReady;
                }
            }
            Outcome::Conflict(message) => {
                if let Some(key) = field {
                    state.errors.remove(&key);
                    state.edits.remove(&key);
                    state.conflicts.insert(key);
                }
                self.status = format!("Changed elsewhere: {message}");
            }
            Outcome::Failed(error) => {
                match field {
                    Some(key) => {
                        state.errors.insert(key, error.message.clone());
                    }
                    None => state.message = Some(error.to_string()),
                }
                if let Operation::RunTask { task, .. } = &op
                    && let Some(run) = state.tasks.get_mut(task)
                {
                    run.phase = TaskPhase::Failed {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    };
                }
                self.status = error.to_string();
            }
        }
        // A job that finished before it was first read is settled here, with the status that was
        // read after it; nothing needs to poll for it.
        if let Some(record) = job.filter(|record| record.status.is_finished()) {
            self.job_finished(&module_id, &record);
        }
        Task::none()
    }

    /// A tracked job reached a terminal status: a task's run takes its result or its error.
    fn job_finished(&mut self, module_id: &str, record: &JobRecord) {
        let state = self.capabilities.touch(module_id);
        let id = record.job_id.as_str();
        for run in state.tasks.values_mut() {
            if run.phase != TaskPhase::Job(id.to_owned()) {
                continue;
            }
            run.phase = match (&record.result, &record.error) {
                (Some(result), _) => TaskPhase::Succeeded {
                    job: id.to_owned(),
                    artifacts: result["artifacts"]
                        .as_array()
                        .map(|artifacts| {
                            artifacts
                                .iter()
                                .filter_map(|artifact| artifact.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                    result: result["result"].clone(),
                },
                (None, Some(error)) => TaskPhase::Failed {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                (None, None) => TaskPhase::Failed {
                    code: "internal".into(),
                    message: format!("the job ended {:?}", record.status),
                },
            };
        }
    }

    fn capability_polled(
        &mut self,
        polled: Vec<(String, String, Result<JobRecord, String>)>,
    ) -> Task<Message> {
        self.capabilities.polling = false;
        let mut refresh = Vec::new();
        for (module, job, result) in polled {
            match result {
                Ok(record) => {
                    let finished = record.status.is_finished();
                    self.capabilities
                        .touch(&module)
                        .track(record.clone(), false);
                    if finished {
                        self.job_finished(&module, &record);
                        if !refresh.contains(&module) {
                            refresh.push(module);
                        }
                    }
                }
                Err(error) => {
                    // A job the owner no longer knows cannot be followed: stop polling it.
                    let state = self.capabilities.touch(&module);
                    state.jobs.retain(|record| record.job_id.as_str() != job);
                    state.message = Some(format!("Job {job} could not be read: {error}"));
                    if !refresh.contains(&module) {
                        refresh.push(module);
                    }
                }
            }
        }
        let tasks: Vec<Task<Message>> = refresh
            .into_iter()
            .map(|module| self.capability_op(&module, Operation::Refresh))
            .collect();
        Task::batch(tasks)
    }

    /// Capture the running capability step's frame once what it waits for has happened: its round
    /// trips have answered and its module's jobs have finished or, for `"wait": false`, are
    /// running and have reported how far they have come, so the frame shows real progress.
    pub(crate) fn capability_settle(&mut self) {
        let Some((module, wait)) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.capability_wait.clone())
        else {
            return;
        };
        let settled = self.capabilities.modules.get(&module).is_none_or(|state| {
            state.pending == 0
                && state.jobs.iter().all(|job| {
                    job.status.is_finished()
                        || (!wait
                            && job.status == JobStatus::Running
                            && job.progress.fraction.is_some())
                })
        });
        if settled {
            if let Some(evidence) = &mut self.evidence {
                evidence.capability_wait = None;
            }
            self.settle_step(Settle::Capability);
        }
    }

    fn arm_capability(&mut self, module: &str, wait: bool) {
        self.await_step(Settle::Capability);
        if let Some(evidence) = &mut self.evidence {
            evidence.capability_wait = Some((module.to_owned(), wait));
        }
    }

    /// One scripted capability gesture, through exactly the messages the section, the task control
    /// and the consent notice send. Its frame is captured once the round trips it started have
    /// answered and, unless it said `"wait": false`, its module's jobs have finished.
    pub(crate) fn capability_step(&mut self, step: CapabilityStep) -> Task<Message> {
        let module = step.module;
        if self.capability_module(&module).is_none() {
            return self.fail_step(format!("{module} declares no capabilities"));
        }
        // A settings gesture is made on the settings sub-view and every other one on the status
        // view, so each frame shows the control the step used.
        let view = match &step.action {
            CapabilityAction::Section(view) => *view,
            CapabilityAction::Set { .. }
            | CapabilityAction::Secret { .. }
            | CapabilityAction::CreateProfile { .. }
            | CapabilityAction::RemoveProfile(_) => CapabilityView::Settings,
            _ => CapabilityView::Status,
        };
        let _ = self.capability_update(CapabilityMessage::Show {
            module_id: module.clone(),
            view,
        });
        // `settle` waits for the module's jobs; a sub-view waits only for a read in flight.
        let settle = matches!(step.action, CapabilityAction::Settle);
        let messages = match self.step_messages(&module, step.action) {
            Ok(Some(messages)) => messages,
            Ok(None) => {
                self.arm_capability(&module, settle);
                self.capability_settle();
                return Task::none();
            }
            Err(reason) => return self.fail_step(reason),
        };
        // Apply is an edit: its frame is the committed render, as an `api` step's is.
        if let [CapabilityMessage::Apply { .. }] = messages.as_slice() {
            self.begin_request();
            let task = self.capability_update(messages.into_iter().next().expect("one message"));
            if !self.busy {
                return self.fail_step(format!("the result was not applied: {}", self.status));
            }
            return task;
        }
        self.arm_capability(&module, step.wait);
        // The messages are one gesture — typing then Enter, say — so the step settles on what the
        // whole gesture started, never between its messages.
        let tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.capability_message(message))
            .collect();
        // Nothing reached the owner: the step's frame is the refusal, with its reason.
        let sent = self
            .capabilities
            .modules
            .get(&module)
            .is_some_and(|state| state.pending > 0);
        if !sent {
            if let Some(evidence) = &mut self.evidence {
                evidence.capability_wait = None;
            }
            return self.fail_step(format!("the step sent nothing: {}", self.status));
        }
        Task::batch(tasks)
    }

    /// The messages one scripted gesture sends, `None` for a step that sends nothing, or why it
    /// cannot be sent.
    fn step_messages(
        &mut self,
        module: &str,
        action: CapabilityAction,
    ) -> Result<Option<Vec<CapabilityMessage>>, String> {
        let module_id = module.to_owned();
        let profile = |editor: &Self, index: Option<usize>| -> Result<Option<String>, String> {
            match index {
                None => Ok(None),
                Some(index) => editor
                    .capabilities
                    .modules
                    .get(module)
                    .and_then(|state| state.profile_at(index))
                    .map(Some)
                    .ok_or_else(|| format!("{module} has no profile {index}")),
            }
        };
        let messages = match action {
            CapabilityAction::Section(_) | CapabilityAction::Settle => return Ok(None),
            CapabilityAction::Set {
                field,
                value,
                profile: index,
            } => {
                let profile = profile(self, index)?;
                match self.setting_kind(module, profile.as_ref(), &field) {
                    Some(
                        SettingKind::Number { .. }
                        | SettingKind::Integer { .. }
                        | SettingKind::Text { .. }
                        | SettingKind::Endpoint { .. },
                    ) => vec![
                        CapabilityMessage::FieldText {
                            module_id: module_id.clone(),
                            profile: profile.clone(),
                            field: field.clone(),
                            text: match &value {
                                Value::String(text) => text.clone(),
                                other => other.to_string(),
                            },
                        },
                        CapabilityMessage::FieldCommit {
                            module_id,
                            profile,
                            field,
                        },
                    ],
                    Some(SettingKind::Boolean | SettingKind::Enum { .. }) => {
                        vec![CapabilityMessage::FieldValue {
                            module_id,
                            profile,
                            field,
                            value,
                        }]
                    }
                    Some(_) => return Err(format!("{field} is set with a file or secret step")),
                    None => return Err(format!("{module} declares no setting {field}")),
                }
            }
            CapabilityAction::Secret {
                field,
                value,
                profile: index,
            } => {
                let profile = profile(self, index)?;
                vec![
                    CapabilityMessage::SecretEdit {
                        module_id: module_id.clone(),
                        profile,
                        field,
                    },
                    CapabilityMessage::SecretText {
                        module_id: module_id.clone(),
                        text: value,
                    },
                    CapabilityMessage::SecretCommit(module_id),
                ]
            }
            CapabilityAction::CreateProfile { adapter, label } => vec![
                CapabilityMessage::ProfileAdapter {
                    module_id: module_id.clone(),
                    adapter,
                },
                CapabilityMessage::ProfileLabel {
                    module_id: module_id.clone(),
                    label,
                },
                CapabilityMessage::ProfileCreate(module_id),
            ],
            CapabilityAction::RemoveProfile(index) => {
                let profile = profile(self, Some(index))?.expect("an index names a profile");
                vec![CapabilityMessage::ProfileRemove { module_id, profile }]
            }
            CapabilityAction::Install(resource) => {
                vec![CapabilityMessage::Install {
                    module_id,
                    resource,
                }]
            }
            CapabilityAction::Remove(resource) => {
                vec![CapabilityMessage::Remove {
                    module_id,
                    resource,
                }]
            }
            CapabilityAction::Activate(on) => vec![CapabilityMessage::Activate { module_id, on }],
            CapabilityAction::Task(task) => vec![CapabilityMessage::RunTask { module_id, task }],
            CapabilityAction::Consent(allow) => {
                if self
                    .capabilities
                    .consent
                    .as_ref()
                    .is_none_or(|open| open.consent.module_id != module)
                {
                    return Err(format!("no consent notice is open for {module}"));
                }
                vec![CapabilityMessage::Consent(allow)]
            }
            CapabilityAction::Apply => {
                let task = self
                    .capability_module(module)
                    .into_iter()
                    .flat_map(|declared| declared.tasks.iter().map(|task| task.id.clone()))
                    .find(|task| self.apply_preset(module, task).is_ok())
                    .ok_or_else(|| format!("{module} has no task result to apply"))?;
                vec![CapabilityMessage::Apply { module_id, task }]
            }
            CapabilityAction::Cancel => {
                let job = self
                    .capabilities
                    .modules
                    .get(module)
                    .and_then(ModuleCapabilities::newest_live)
                    .map(|job| job.job_id.as_str().to_owned())
                    .ok_or_else(|| format!("{module} has no live job to cancel"))?;
                vec![CapabilityMessage::Cancel { module_id, job }]
            }
            CapabilityAction::Revoke(index) => {
                // The permissions list is opened first, exactly as a person would to reach Revoke.
                if !self
                    .capabilities
                    .modules
                    .get(module)
                    .is_some_and(|state| state.permissions_open)
                {
                    let _ = self
                        .capability_update(CapabilityMessage::TogglePermissions(module_id.clone()));
                }
                let grant = self
                    .capabilities
                    .modules
                    .get(module)
                    .and_then(|state| state.grants().get(index).map(|grant| (*grant).clone()))
                    .ok_or_else(|| format!("{module} has no grant {index}"))?;
                if !grant.is_live() {
                    return Err(format!("grant {index} of {module} is already revoked"));
                }
                vec![CapabilityMessage::Revoke {
                    module_id,
                    grant: grant.grant_id,
                }]
            }
        };
        Ok(Some(messages))
    }
}
