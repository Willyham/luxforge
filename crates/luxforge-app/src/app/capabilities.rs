//! The desktop's module capability driver. A gesture on a capability block, on a task control or
//! on the consent notice becomes one [`Operation`], and one owner round trip runs it off the update
//! loop through the same methods the JSON API dispatches: the `module.*` methods, `task.<id>` and
//! `job.*`. Every round trip ends with one `module.status` read of that module alone, which is the
//! narrowest completion path that keeps the block honest. A settings write answers with the fresh
//! settings itself, so it needs no second read; one that meets a newer revision reads them again
//! and says so on the block's one status line.
//!
//! Jobs are followed by a reader that exists only while a job the desktop tracks is queued or
//! running, and that sends the desktop a message only when a job's record changes or the job ends
//! (see [`Editor::capability_reader_subscription`]). A `consent-required` answer opens the consent
//! notice; Allow grants exactly the scope the core named, with this client's permission authority,
//! and retries the refused operation once. Every request the desktop records is redacted first.
use crate::{
    app::{
        Editor,
        evidence::{CapabilityAction, CapabilityStep, Reference},
        job_reads::{self, Pass, Reader, Watch},
        message::{Message, action::ActionMessage, capability::CapabilityMessage},
        tasks::{self, mutation, owner_task, request},
    },
    state::{
        capabilities::{
            Consent, ModuleStatus, OpenConsent, Operation, TaskPhase, TaskRun, declares,
            task_profile,
        },
        fields, tools,
    },
};
use iced::{Subscription, Task, futures::stream::BoxStream};
use luxforge_core::{
    AssetId, ClientId, ModuleDescriptor, OwnerHandle, ParameterKind,
    capabilities::{
        grants::{self, GrantList},
        host::{self, TASK_PREFIX},
        resources,
        settings::{self, SettingsRead},
    },
    jobs::{JOB_CANCEL, JOB_READ, JobRecord},
    redact_params,
};
use serde_json::{Map, Value, json};

#[cfg(test)]
use crate::app::job_reads::Verdict;
pub(crate) use tasks::CallError;

/// How many times a task retries after its source or artifacts were prepared.
const PREPARATION_RETRIES: usize = 4;

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
    /// A settings write met a newer revision; the settings were read again.
    Conflict(String),
    Failed(CallError),
}

/// One finished round trip, back on the update loop.
#[derive(Clone, Debug)]
pub(crate) struct Answer {
    pub(crate) module_id: String,
    /// The operation the outcome answers. After Allow or Don't allow it is the refused operation,
    /// so the answer is routed exactly as a first try would be.
    pub(crate) op: Operation,
    /// The notice Allow or Don't allow answered, when this round trip was one.
    pub(crate) consent: Option<(bool, Box<Consent>)>,
    pub(crate) outcome: Outcome,
    pub(crate) settings: Option<SettingsRead>,
    pub(crate) status: Result<ModuleStatus, String>,
    /// The job the operation started, read once after it was queued.
    pub(crate) job: Option<JobRecord>,
    /// Every request sent, redacted, for the event log.
    pub(crate) sent: Vec<Value>,
}

/// One request through the desktop's one call helper, recorded redacted before it is sent.
fn call(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
    sent: &mut Vec<Value>,
) -> Result<Value, CallError> {
    sent.push(json!({"method": method, "params": redact_params(method, &params)}));
    tasks::call_detailed(owner, client, method, params)
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// What a failure means for the block: consent, or a plain failure whose message is its status
/// line (a `not-ready` answer names what is missing in its message).
fn classify(error: CallError, retry: &Operation) -> Outcome {
    if error.code == "consent-required"
        && let Some(Ok(consent)) = error
            .data
            .as_ref()
            .and_then(|data| data.get("consent"))
            .cloned()
            .map(parse::<Consent>)
    {
        return Outcome::Consent {
            consent: Box::new(consent),
            retry: Box::new(retry.clone()),
        };
    }
    Outcome::Failed(error)
}

/// Withdraw every live grant the module holds, one `module.permission.revoke` each, and say how
/// many were withdrawn.
fn revoke_all(
    owner: &OwnerHandle,
    client: ClientId,
    module_id: &str,
    sent: &mut Vec<Value>,
) -> Result<Value, CallError> {
    let listed = call(
        owner,
        client,
        grants::LIST,
        json!({"module_id": module_id}),
        sent,
    )?;
    let list: GrantList = parse(listed).map_err(|message| CallError {
        code: "internal".into(),
        message,
        data: None,
        job_id: None,
    })?;
    let mut revoked = 0;
    for grant in list.grants.iter().filter(|grant| grant.is_live()) {
        call(
            owner,
            client,
            grants::REVOKE,
            json!({"grant_id": grant.grant_id, "mutation": request()}),
            sent,
        )?;
        revoked += 1;
    }
    Ok(json!({"revoked": revoked}))
}

/// Run one operation's own requests and say what they came to. A settings write puts the fresh
/// settings it answers with into `settings`.
fn perform(
    owner: &OwnerHandle,
    client: ClientId,
    module_id: &str,
    op: &Operation,
    sent: &mut Vec<Value>,
    settings: &mut Option<SettingsRead>,
) -> Outcome {
    let mut read = |sent: &mut Vec<Value>| {
        if let Ok(read) = call(
            owner,
            client,
            settings::READ,
            json!({"module_id": module_id}),
            sent,
        ) {
            *settings = parse(read).ok();
        }
    };
    let result = match op {
        Operation::Load => {
            read(sent);
            Ok(Value::Null)
        }
        Operation::Refresh => Ok(Value::Null),
        Operation::Set {
            field,
            value,
            revision,
        } => {
            let params = json!({
                "module_id": module_id,
                "values": {field.as_str(): value},
                "mutation": mutation(*revision),
            });
            match call(owner, client, settings::SET, params, sent) {
                Ok(answer) => {
                    *settings = parse(answer["settings"].clone()).ok();
                    Ok(answer)
                }
                // Somebody else changed the settings: read them again and say so.
                Err(error) if error.code == "conflict" => {
                    read(sent);
                    return Outcome::Conflict(error.message);
                }
                Err(error) => Err(error),
            }
        }
        Operation::Install { resource } => call(
            owner,
            client,
            resources::INSTALL,
            json!({"module_id": module_id, "resource_id": resource, "mutation": request()}),
            sent,
        ),
        Operation::Remove { resource } => call(
            owner,
            client,
            resources::REMOVE,
            json!({"module_id": module_id, "resource_id": resource, "mutation": request()}),
            sent,
        ),
        Operation::Cancel { job } => call(owner, client, JOB_CANCEL, json!({"job_id": job}), sent),
        Operation::RevokeAll => revoke_all(owner, client, module_id, sent),
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
            // One press is one request: asking again after a preparation keeps its request_id.
            params.insert("mutation".into(), json!(request()));
            let method = format!("{TASK_PREFIX}{task}");
            let mut attempt = 0;
            loop {
                match call(owner, client, &method, Value::Object(params.clone()), sent) {
                    // The source or an artifact is not prepared yet: wait for that job through the
                    // same wait a preview uses, and ask again.
                    Err(error)
                        if error.code == "preparation-required"
                            && attempt < PREPARATION_RETRIES =>
                    {
                        attempt += 1;
                        let job = error
                            .job_id
                            .clone()
                            .unwrap_or_else(|| error.message.clone());
                        sent.push(json!({"method": JOB_READ, "params": {"job_id": job}}));
                        if let Err(error) = tasks::wait_source_job(owner, client, &job) {
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
                return match call(owner, client, grants::DENY, answer, sent) {
                    Ok(answer) => Outcome::Done(answer),
                    Err(error) => Outcome::Failed(error),
                };
            }
            if let Err(error) = call(owner, client, grants::GRANT, answer, sent) {
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
    // An answer to the notice is routed as the refused operation it answers: after Allow the
    // outcome is the retry's, and after Don't allow that operation's run ends.
    let (routed, consent) = match &op {
        Operation::Consent {
            allow,
            consent,
            retry,
        } => (
            retry.as_deref().unwrap_or(&op).clone(),
            Some((*allow, consent.clone())),
        ),
        _ => (op.clone(), None),
    };
    let job = match &outcome {
        Outcome::Done(answer) => answer["job_id"].as_str().and_then(|job| {
            call(owner, client, JOB_READ, json!({"job_id": job}), &mut sent)
                .ok()
                .and_then(|record| parse::<JobRecord>(record).ok())
        }),
        _ => None,
    };
    let status = call(
        owner,
        client,
        host::STATUS,
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
        status,
        job,
        sent,
    }
}

/// `job.read` for one tracked job.
#[cfg(test)]
pub(crate) fn read_job(
    owner: &OwnerHandle,
    client: ClientId,
    job: &str,
) -> Result<JobRecord, String> {
    tasks::call_detailed(owner, client, JOB_READ, json!({"job_id": job}))
        .map_err(|error| error.to_string())
        .and_then(parse::<JobRecord>)
}

/// What a reader of capability jobs sends: each read that is news, by module and job.
pub(crate) type Polled = Vec<(String, String, Result<JobRecord, String>)>;

/// A stable reader for one capability job; changing the tracked set keeps unrelated readers.
pub(crate) fn capability_reads(reader: &Reader<(String, String)>) -> BoxStream<'static, Message> {
    let (owner, client) = (reader.owner.clone(), reader.client);
    let (module, job) = reader.identity.clone();
    let visible = reader.presentation_visible;
    let state = std::sync::Arc::new(std::sync::Mutex::new((None, Watch::default())));
    job_reads::reads(move || {
        let (owner, module, job) = (owner.clone(), module.clone(), job.clone());
        let (previous, mut watch) = std::mem::take(&mut *state.lock().expect("capability reader"));
        let state = state.clone();
        async move {
            let result = job_reads::wait(&owner, client, &job, previous).await;
            let (next, result) = match result {
                Ok((change, value)) => (Some(change), Ok(value)),
                Err(error) => (previous, Err(error)),
            };
            let verdict = watch.observe_filtered(
                &result,
                |record| !matches!(record["status"].as_str(), Some("queued" | "running")),
                |one, two| {
                    if visible {
                        one == two
                    } else {
                        job_reads::same_without_progress(one, two)
                    }
                },
            );
            *state.lock().expect("capability reader") = (next, watch);
            Pass::of(verdict, || {
                Message::Capability(CapabilityMessage::Polled(vec![(
                    module,
                    job,
                    result.and_then(|value| {
                        serde_json::from_value(value).map_err(|error| error.to_string())
                    }),
                )]))
            })
        }
    })
}

/// One pass of the reader of capability jobs: read each job it still follows and send, in one
/// message, only the reads that are news. A live record is news when it is the job's first or
/// differs from the one sent before it (progress included); a job that ended, or whose read
/// failed, is sent once and read no more, and the reader ends with its last job. The message is
/// the one the desktop applies entry by entry, so it needs only the entries that changed.
#[cfg(test)]
pub(crate) fn capability_pass(
    jobs: Vec<(String, String)>,
    mut read: impl FnMut(&str) -> Result<JobRecord, String>,
) -> impl FnMut() -> Pass<Message> {
    let mut watches: Vec<_> = jobs
        .into_iter()
        .map(|(module, job)| (module, job, Watch::default()))
        .collect();
    move || {
        let mut news: Polled = Vec::new();
        for (module, job, watch) in watches.iter_mut().filter(|(_, _, watch)| !watch.finished()) {
            let result = read(job);
            if watch.observe(&result, |record| record.status.is_finished()) != Verdict::Skip {
                news.push((module.clone(), job.clone(), result));
            }
        }
        if news.is_empty() {
            return Pass::Quiet;
        }
        let message = Message::Capability(CapabilityMessage::Polled(news));
        if watches.iter().all(|(_, _, watch)| watch.finished()) {
            Pass::Last(message)
        } else {
            Pass::Send(message)
        }
    }
}

impl Editor {
    /// Start one owner round trip for a module. Nothing runs on the update loop but this.
    pub(crate) fn capability_op(&mut self, module_id: &str, op: Operation) -> Task<Message> {
        self.capabilities.module_mut(module_id).pending += 1;
        #[cfg(test)]
        self.capability_started
            .push((module_id.to_owned(), op.clone()));
        let owner = self.owner.clone();
        let client = self.client;
        let module = module_id.to_owned();
        owner_task(
            move || run(&owner, client, module, op),
            |answer| Message::Capability(CapabilityMessage::Answered(Box::new(answer))),
        )
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
                let state = self.capabilities.module(module);
                state.settings.is_none()
                    && state.status.is_none()
                    && state.message.is_none()
                    && state.pending == 0
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

    /// Each job's subscription identity survives changes to the other tracked jobs.
    pub(crate) fn capability_reader_subscription(&self) -> Option<Subscription<Message>> {
        self.capabilities.live().then(|| {
            Subscription::batch(self.capabilities.live_jobs().into_iter().map(|identity| {
                Subscription::run_with(
                    Reader {
                        identity,
                        owner: self.owner.clone(),
                        client: self.client,
                        presentation_visible: self.visibility.sampling_allowed(),
                    },
                    capability_reads,
                )
            }))
        })
    }

    /// A task run's result belongs to the asset it ran for: another asset clears every run.
    pub(crate) fn capabilities_asset_changed(&mut self, asset: &AssetId) {
        for state in self.capabilities.modules.values_mut() {
            state
                .tasks
                .retain(|_, run| run.asset.as_ref().is_none_or(|ran| ran == asset));
        }
    }

    fn capability_module(&self, module_id: &str) -> Option<&ModuleDescriptor> {
        tools::module_of(&self.modules, module_id).filter(|module| declares(module))
    }

    /// A module-level setting written from the form, or the reason it cannot be sent, on the
    /// block's status line.
    fn write_setting(&mut self, module_id: &str, field: String, value: Value) -> Task<Message> {
        let state = self.capabilities.module(module_id);
        let revision = match (state.pending, state.revision()) {
            (0, Some(revision)) => Ok(revision),
            (0, None) => Err("The module's settings have not been read yet".to_owned()),
            _ => Err("Waiting for the last change".to_owned()),
        };
        match revision {
            Ok(revision) => self.capability_op(
                module_id,
                Operation::Set {
                    field,
                    value,
                    revision,
                },
            ),
            Err(reason) => self.capability_refused(module_id, reason),
        }
    }

    /// A gesture that sends nothing: its reason goes on the block's status line and the status bar.
    fn capability_refused(&mut self, module_id: &str, reason: String) -> Task<Message> {
        self.capabilities.module_mut(module_id).message = Some(reason.clone());
        self.status.text = reason;
        Task::none()
    }

    pub(crate) fn capability_update(&mut self, message: CapabilityMessage) -> Task<Message> {
        match message {
            CapabilityMessage::FieldText {
                module_id,
                field,
                text,
            } => {
                self.capabilities
                    .module_mut(&module_id)
                    .edits
                    .insert(field, text);
            }
            CapabilityMessage::FieldCommit { module_id, field } => {
                let Some(text) = self
                    .capabilities
                    .module(&module_id)
                    .edits
                    .get(&field)
                    .cloned()
                else {
                    return Task::none();
                };
                // Typed text is read back exactly as a module control's text is, against the same
                // parameter declaration; an emptied endpoint returns the field to having no value,
                // and any other endpoint text is sent for the transport policy to classify.
                let declared = self
                    .capability_module(&module_id)
                    .and_then(|module| module.settings.as_ref())
                    .and_then(|settings| settings.field(&field));
                let value = match declared {
                    Some(declared) if declared.is_endpoint() => match text.trim() {
                        "" => Ok(Value::Null),
                        trimmed => Ok(Value::from(trimmed)),
                    },
                    Some(declared)
                        if matches!(
                            declared.kind(),
                            ParameterKind::Number { .. }
                                | ParameterKind::Integer { .. }
                                | ParameterKind::String { .. }
                        ) =>
                    {
                        fields::parse_field(&declared.parameter, &text)
                    }
                    Some(_) => Err(format!("{field} is not a typed field")),
                    None => Err(format!("{module_id} declares no setting {field}")),
                };
                return match value {
                    Ok(value) => self.write_setting(&module_id, field, value),
                    Err(reason) => self.capability_refused(&module_id, reason),
                };
            }
            CapabilityMessage::FieldValue {
                module_id,
                field,
                value,
            } => return self.write_setting(&module_id, field, value),
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
            CapabilityMessage::RevokeAll(module_id) => {
                return self.capability_op(&module_id, Operation::RevokeAll);
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
                        self.capabilities
                            .module_mut(&module_id)
                            .tasks
                            .insert(task, run);
                        self.capability_op(&module_id, op)
                    }
                    Err(reason) => {
                        self.status.text = reason;
                        Task::none()
                    }
                };
            }
            CapabilityMessage::Apply { module_id, task } => {
                return match self.apply_preset(&module_id, &task) {
                    Ok((action, preset)) => {
                        self.dispatch(Message::Action(ActionMessage::Run { action, preset }))
                    }
                    Err(reason) => {
                        self.status.text = reason;
                        Task::none()
                    }
                };
            }
            CapabilityMessage::CopyTaskRequest { module_id, task } => {
                self.view_state.menu = None;
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
                        params.insert("mutation".into(), json!(request()));
                        let method = format!("{TASK_PREFIX}{task}");
                        let params = redact_params(&method, &Value::Object(params));
                        self.status.text = format!("Copied the {method} request");
                        iced::clipboard::write(
                            serde_json::to_string_pretty(
                                &json!({"method": method, "params": params}),
                            )
                            .unwrap_or_default(),
                        )
                    }
                    Ok(_) => Task::none(),
                    Err(reason) => {
                        self.status.text = reason;
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
            CapabilityMessage::Answered(answer) => self.capability_answered(*answer),
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
        let asset = match (declared.asset, &self.document.state) {
            (true, Some(state)) => Some(state.asset.id.clone()),
            (true, None) => return Err("No photograph is open".into()),
            (false, _) => None,
        };
        let profile = task_profile(self.capabilities.module(module_id), declared.profile);
        if declared.profile && profile.is_none() {
            return Err("Create a profile with module.profile.create first".into());
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
        let asset = self.document.state.as_ref().map(|state| &state.asset.id);
        let artifact = self
            .capabilities
            .module(module_id)
            .tasks
            .get(task)
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

    fn capability_answered(&mut self, answer: Answer) {
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
            || json!({
                "module_id": module_id,
                "operation": op.name(),
                "consent": consent.as_ref().map(|(allow, consent)| json!({"allow": allow, "capability": consent.capability})),
                "outcome": match &outcome {
                    Outcome::Done(_) => json!("done"),
                    Outcome::Consent { consent, .. } => json!({"consent-required": consent.capability}),
                    Outcome::Conflict(message) => json!({"conflict": message}),
                    Outcome::Failed(error) => json!({"failed": {"code": error.code, "message": error.message}}),
                },
                "job": job.as_ref().map(|job| json!({"job_id": job.job_id, "kind": job.kind, "status": job.status})),
                "requests": sent,
            }),
        );
        let state = self.capabilities.module_mut(&module_id);
        state.pending = state.pending.saturating_sub(1);
        if let Some(settings) = settings {
            state.settings = Some(settings);
        }
        // The jobs this status reports ended. A run waiting on one takes its result here: once a
        // read has tracked the job as ended the job reader stops, so a read made for any other
        // reason — another client's change, or the owner's wake when this desktop's own task
        // ended — is the last the desktop hears of it.
        let mut ended = Vec::new();
        match status {
            Ok(status) => {
                for record in &status.jobs {
                    state.track(record.clone(), false);
                    if record.status.is_finished() {
                        ended.push(record.clone());
                    }
                }
                state.status = Some(status);
            }
            Err(error) => state.message = Some(error),
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
        let run = match &op {
            Operation::RunTask { task, .. } => state.tasks.get_mut(task),
            _ => None,
        };
        match outcome {
            Outcome::Done(answer) => {
                state.message = None;
                if let Operation::Set { field, .. } = &op {
                    state.edits.remove(field);
                }
                if let Some(record) = &job {
                    if let Some(run) = run {
                        run.phase = TaskPhase::Job(record.job_id.as_str().to_owned());
                    }
                    state.track(record.clone(), true);
                }
                if op == Operation::RevokeAll {
                    self.status.text = format!(
                        "Revoked {} permission(s) of {module_id}",
                        answer["revoked"].as_u64().unwrap_or(0)
                    );
                }
            }
            Outcome::Consent { consent, retry } => {
                if let Some(run) = run {
                    run.phase = TaskPhase::Consent;
                }
                self.capabilities.consent = Some(OpenConsent {
                    consent: *consent,
                    retry: Some(*retry),
                });
            }
            Outcome::Conflict(message) => {
                // The settings were read again: the form shows the other client's values, and the
                // one status line says why the typed value was not written.
                if let Operation::Set { field, .. } = &op {
                    state.edits.remove(field);
                }
                let line = format!("Changed elsewhere, so the settings were read again: {message}");
                state.message = Some(line.clone());
                self.status.text = line;
            }
            Outcome::Failed(error) => {
                if let Some(run) = run {
                    run.phase = TaskPhase::Failed {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    };
                }
                state.message = Some(error.to_string());
                self.status.text = error.to_string();
            }
        }
        // A job that finished before it was first read is settled here, with the status that was
        // read after it; nothing needs to poll for it.
        if let Some(record) = job.filter(|record| record.status.is_finished()) {
            self.job_finished(&module_id, &record);
        }
        for record in &ended {
            self.job_finished(&module_id, record);
        }
    }

    /// A tracked job reached a terminal status: a task's run takes its result or its error.
    fn job_finished(&mut self, module_id: &str, record: &JobRecord) {
        let state = self.capabilities.module_mut(module_id);
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

    fn capability_polled(&mut self, polled: Polled) -> Task<Message> {
        let mut refresh = Vec::new();
        for (module, job, result) in polled {
            match result {
                Ok(record) => {
                    let finished = record.status.is_finished();
                    self.capabilities
                        .module_mut(&module)
                        .track(record.clone(), false);
                    if finished {
                        self.job_finished(&module, &record);
                    }
                    if finished && !refresh.contains(&module) {
                        refresh.push(module);
                    }
                }
                Err(error) => {
                    // A job the owner no longer knows cannot be followed: stop polling it.
                    let state = self.capabilities.module_mut(&module);
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

    // ---- evidence -------------------------------------------------------------------------------

    /// The envelope an `api` step's settings write carries: the settings revision the desktop
    /// holds for the module it names, as the block's own write would, so a stale one is a conflict.
    pub(crate) fn settings_envelope(
        &self,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<Value, String> {
        let module = params
            .get("module_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{method} names no module whose settings revision is held"))?;
        self.capabilities
            .module(module)
            .revision()
            .map(|revision| json!(mutation(revision)))
            .ok_or_else(|| format!("{module}'s settings have not been read yet"))
    }

    /// A profile an `api` step names by its label or its position in the module's settings read,
    /// as the identity the host assigned it.
    pub(crate) fn resolve_profile(
        &self,
        module: Option<&str>,
        reference: &Reference,
    ) -> Result<String, String> {
        let module = module.ok_or("a profile named by label or position needs a module_id")?;
        let profiles = self
            .capabilities
            .module(module)
            .settings
            .as_ref()
            .map(|settings| settings.profiles.as_slice())
            .ok_or_else(|| format!("{module}'s settings have not been read yet"))?;
        let found: Vec<&str> = match reference {
            Reference::Id(id) => return Ok(id.clone()),
            Reference::Index(index) => profiles
                .get(*index)
                .map(|profile| profile.id.as_str())
                .into_iter()
                .collect(),
            Reference::Name { name } => profiles
                .iter()
                .filter(|profile| &profile.label == name)
                .map(|profile| profile.id.as_str())
                .collect(),
        };
        match found.as_slice() {
            [id] => Ok((*id).to_owned()),
            [] => Err(format!("{module} has no such profile")),
            many => Err(format!(
                "{} profiles of {module} carry that label",
                many.len()
            )),
        }
    }

    /// An `api` step that calls a capability method on a module reads that module again once the
    /// method answers, as another client's change would be read, so its frame shows the block
    /// the method left and the next step resolves against it.
    pub(crate) fn capability_read_after(&mut self, method: &str, params: &Map<String, Value>) {
        let module = params.get("module_id").and_then(Value::as_str);
        if let (true, Some(module), Some(evidence)) =
            (tasks::capability_event(method), module, &mut self.evidence)
        {
            // A job the method starts is captured once it reports progress, not once it ends.
            evidence.capability_wait = Some((module.to_owned(), false));
        }
    }

    /// The `api` step's capability method answered: read its module, and capture once that read
    /// has answered. `None` when the step called no capability method.
    pub(crate) fn capability_host_answered(&mut self) -> Option<Task<Message>> {
        let (module, wait) = self.evidence.as_ref()?.capability_wait.clone()?;
        let task = self.capability_op(&module, Operation::Load);
        self.await_capability(&module, wait);
        Some(task)
    }

    /// One scripted gesture on the task control or the consent notice, through exactly the
    /// messages they send. Its frame is captured once the round trips it started have answered
    /// and, unless it said `"wait": false`, its module's jobs have finished.
    pub(crate) fn capability_step(&mut self, step: CapabilityStep) -> Task<Message> {
        let module = step.module;
        if self.capability_module(&module).is_none() {
            return self.fail_step(format!("{module} declares no capabilities"));
        }
        let message = match self.step_message(&module, step.action) {
            Ok(Some(message)) => message,
            Ok(None) => {
                self.await_capability(&module, true);
                self.settle_capability();
                return Task::none();
            }
            Err(reason) => return self.fail_step(reason),
        };
        // Apply is an edit: its frame is the committed render, as an `api` step's is.
        if let CapabilityMessage::Apply { .. } = message {
            self.begin_request();
            let task = self.capability_update(message);
            if !self.busy {
                return self.fail_step(format!("the result was not applied: {}", self.status.text));
            }
            return task;
        }
        self.await_capability(&module, step.wait);
        let task = self.capability_update(message);
        // Nothing reached the owner: the step's frame is the refusal, with its reason.
        if self.capabilities.module(&module).pending == 0 {
            if let Some(evidence) = &mut self.evidence {
                evidence.capability_wait = None;
            }
            return self.fail_step(format!("the step sent nothing: {}", self.status.text));
        }
        task
    }

    /// The message one scripted gesture sends, `None` for a step that sends nothing, or why it
    /// cannot be sent.
    fn step_message(
        &mut self,
        module: &str,
        action: CapabilityAction,
    ) -> Result<Option<CapabilityMessage>, String> {
        let module_id = module.to_owned();
        Ok(Some(match action {
            CapabilityAction::Settle => return Ok(None),
            CapabilityAction::Task(task) => CapabilityMessage::RunTask { module_id, task },
            CapabilityAction::Consent(allow) => {
                if self
                    .capabilities
                    .consent
                    .as_ref()
                    .is_none_or(|open| open.consent.module_id != module)
                {
                    return Err(format!("no consent notice is open for {module}"));
                }
                CapabilityMessage::Consent(allow)
            }
            CapabilityAction::Apply => {
                let task = self
                    .capability_module(module)
                    .into_iter()
                    .flat_map(|declared| declared.tasks.iter().map(|task| task.id.clone()))
                    .find(|task| self.apply_preset(module, task).is_ok())
                    .ok_or_else(|| format!("{module} has no task result to apply"))?;
                CapabilityMessage::Apply { module_id, task }
            }
        }))
    }
}

/// After the screen is derived: a capability section is read for the first time once it is on
/// screen. Its first read is what the section then shows, so the screen is derived again to show
/// it loading.
pub(super) fn after_derive(editor: &mut Editor) -> Task<Message> {
    let Some(loads) = editor.request_capability_loads() else {
        return Task::none();
    };
    editor.rederive();
    loads
}

/// Capability jobs are read while one the desktop follows is queued or running, and never
/// otherwise ([`Editor::capability_reader_subscription`]).
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    editor
        .capability_reader_subscription()
        .unwrap_or_else(Subscription::none)
}
