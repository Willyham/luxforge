//! What a module receives on the capability worker: the host's view of its settings, the secrets
//! it declares, the resources its job was given, the requests it may send, the artifacts it may
//! publish, and the job's progress and cancellation. It is the only way module code reaches
//! anything the host owns, and it enforces what the job runs under: a module never names a key or
//! a URL of its own. See `docs/design/module-capabilities.md#outcome-and-boundary`.
//!
//! The host builds one per job on the owner, where settings are read and grants are checked, and
//! moves it to the worker. Each capability a job is granted adds one field and one method here, set
//! by the host with a `with_*` builder before the job is queued: a task's profile, the endpoint,
//! adapter and disclosed data of each granted `remote-image-request`, and the artifact writer a
//! task publishes through. Every capability call checks the job's cancel flag first. Module code
//! only ever receives what the host checked.
use super::{
    data::DisclosedData,
    descriptor::{AdapterAuth, AdapterDescriptor},
    jobs::JobControl,
    resources::SharedTransport,
    secrets::{SecretKey, SecretStore, SecretValue},
    transport::{Endpoint, Method, RedirectPolicy, SendOptions, TransportRequest},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error,
    artifacts::{
        ArtifactId, ArtifactMeta, ArtifactRecord, ArtifactWriter, MAX_ARTIFACT_BYTES,
        PreparedArtifact,
    },
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};
use zeroize::Zeroize;

/// The most artifacts one task may publish. Each is held ready in memory until the owner records
/// it, so the count and [`MAX_TASK_ARTIFACT_BYTES`] bound what a task keeps alive.
pub const MAX_TASK_ARTIFACTS: usize = 16;
/// The most bytes the artifacts of one task may hold together: one artifact's limit.
pub const MAX_TASK_ARTIFACT_BYTES: u64 = MAX_ARTIFACT_BYTES;

/// One provider profile as a task sees it: its identity, the adapter it names and its non-secret
/// values. An endpoint is not among them: the host sends to it, the module never does.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileView {
    pub id: String,
    pub adapter: String,
    pub values: Map<String, Value>,
}

/// What a granted `remote-image-request` capability sends: the endpoint, the adapter whose limits,
/// timeout and authentication apply, the profile's credential field when the adapter is bearer, and
/// the disclosed data the host builds the body from.
pub(crate) struct GrantedSend {
    pub transport: Arc<SharedTransport>,
    pub endpoint: Endpoint,
    pub adapter: AdapterDescriptor,
    pub credential: Option<String>,
    pub data: DisclosedData,
}

/// What a task's worker hands back to the owner beside its result: the artifacts it published,
/// which the owner records only when the task succeeds.
#[derive(Debug, Default)]
pub(crate) struct TaskOutcome {
    published: Mutex<Vec<(ArtifactRecord, Arc<PreparedArtifact>)>>,
}

impl TaskOutcome {
    /// The identities published so far, in publishing order.
    pub(crate) fn published_ids(&self) -> Vec<ArtifactId> {
        lock(&self.published)
            .iter()
            .map(|(record, _)| record.id.clone())
            .collect()
    }

    /// Everything published, for the owner to record.
    pub(crate) fn take_published(&self) -> Vec<(ArtifactRecord, Arc<PreparedArtifact>)> {
        std::mem::take(&mut *lock(&self.published))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A module's capability context for one job. `Send`, so it moves to the worker with the job.
pub struct ModuleContext {
    module_id: String,
    /// The effective, valid, non-secret module-level setting values, without file paths and
    /// endpoints: those are reached through the capabilities that name them.
    values: Map<String, Value>,
    /// The module-level secret fields it declares.
    secret_fields: Vec<String>,
    profile: Option<ProfileView>,
    /// The secret fields of the profile, when the job has one.
    profile_secret_fields: Vec<String>,
    secrets: Arc<dyn SecretStore>,
    /// The installed resources the job was given, by resource identity.
    resources: BTreeMap<String, PathBuf>,
    /// The requests the job may send, by capability.
    sends: BTreeMap<String, GrantedSend>,
    /// Where a task publishes artifacts; `None` for a job that publishes none.
    writer: Option<ArtifactWriter>,
    outcome: Arc<TaskOutcome>,
    control: Arc<JobControl>,
}

/// Names only: a path, an endpoint, a body or a secret never reaches a log through a context.
impl std::fmt::Debug for ModuleContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleContext")
            .field("module_id", &self.module_id)
            .field("values", &self.values)
            .field("profile", &self.profile.as_ref().map(|profile| &profile.id))
            .field("resources", &self.resources.keys().collect::<Vec<_>>())
            .field("sends", &self.sends.keys().collect::<Vec<_>>())
            .field("publishes", &self.writer.is_some())
            .finish_non_exhaustive()
    }
}

impl ModuleContext {
    pub(crate) fn new(
        module_id: &str,
        secrets: Arc<dyn SecretStore>,
        control: Arc<JobControl>,
    ) -> Self {
        Self {
            module_id: module_id.to_owned(),
            values: Map::new(),
            secret_fields: Vec::new(),
            profile: None,
            profile_secret_fields: Vec::new(),
            secrets,
            resources: BTreeMap::new(),
            sends: BTreeMap::new(),
            writer: None,
            outcome: Arc::default(),
            control,
        }
    }

    /// The module-level values and the secret fields the module declares.
    pub(crate) fn with_settings(
        mut self,
        values: Map<String, Value>,
        secret_fields: Vec<String>,
    ) -> Self {
        self.values = values;
        self.secret_fields = secret_fields;
        self
    }

    /// The profile a task names, with the secret fields of its profile block.
    pub(crate) fn with_profile(mut self, profile: ProfileView, secret_fields: Vec<String>) -> Self {
        self.profile = Some(profile);
        self.profile_secret_fields = secret_fields;
        self
    }

    /// One installed resource the job may read, at the path the host installed it to.
    pub(crate) fn with_resource(mut self, resource_id: &str, path: PathBuf) -> Self {
        self.resources.insert(resource_id.to_owned(), path);
        self
    }

    /// The request a live `remote-image-request` grant of `capability_id` allows.
    pub(crate) fn with_send(mut self, capability_id: &str, send: GrantedSend) -> Self {
        self.sends.insert(capability_id.to_owned(), send);
        self
    }

    /// Publish through `writer` and report what was published through `outcome`.
    pub(crate) fn with_artifacts(
        mut self,
        writer: ArtifactWriter,
        outcome: Arc<TaskOutcome>,
    ) -> Self {
        self.writer = Some(writer);
        self.outcome = outcome;
        self
    }

    pub fn module_id(&self) -> &str {
        &self.module_id
    }

    /// Every valid, non-secret module-level value, defaults included. An endpoint is not a value a
    /// module reads: `send` uses it.
    pub fn values(&self) -> &Map<String, Value> {
        &self.values
    }

    /// One module-level value, when it is set or has a default and is valid.
    pub fn value(&self, setting_id: &str) -> Option<&Value> {
        self.values.get(setting_id)
    }

    pub fn profile(&self) -> Option<&ProfileView> {
        self.profile.as_ref()
    }

    /// Read one secret the module or the job's profile declares. Any other name is a `validation`
    /// error without asking the store; a secret that is not set is `not-ready`. The value is read
    /// here, on the worker, and nowhere else.
    pub fn secret(&self, setting_id: &str) -> Result<SecretValue, Error> {
        let key = if self.secret_fields.iter().any(|field| field == setting_id) {
            SecretKey::new(&self.module_id, None, setting_id)
        } else if let Some(profile) = self.profile.as_ref().filter(|_| {
            self.profile_secret_fields
                .iter()
                .any(|field| field == setting_id)
        }) {
            SecretKey::new(&self.module_id, Some(&profile.id), setting_id)
        } else {
            return Err(Error::validation(format!(
                "module {} declares no secret setting {setting_id} for this job",
                self.module_id
            )));
        };
        self.secrets
            .read(&key)?
            .ok_or_else(|| Error::not_ready(format!("secret setting {setting_id} is not set")))
    }

    /// Where an installed resource the job was given lives. Its bytes were checked against the
    /// pinned hash when it was installed.
    pub fn resource_path(&self, resource_id: &str) -> Result<&Path, Error> {
        self.resources
            .get(resource_id)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                Error::not_ready(format!(
                    "resource {resource_id} of module {} is not available to this job",
                    self.module_id
                ))
            })
    }

    fn not_granted(&self, capability_id: &str) -> Error {
        Error::validation(format!(
            "module {} was not granted capability {capability_id} for this job",
            self.module_id
        ))
    }

    /// Send the data a granted `remote-image-request` capability discloses to the profile's
    /// endpoint, and return the response body. The host builds the body from the data the consent
    /// notice disclosed, as the entry was when the task was requested — for `sample-grid-8` it
    /// samples that entry here, on the worker, the first time — posts it through its transport with
    /// the adapter's size limits and timeout, follows no redirect, and adds `Authorization: Bearer`
    /// with the profile's credential when the adapter authenticates; the credential is read here and
    /// never logged. A status outside 2xx is a `read-error` naming the adapter and the status. The
    /// module never supplies a URL or a body.
    pub fn send(&self, capability_id: &str) -> Result<Vec<u8>, Error> {
        self.checkpoint()?;
        let granted = self
            .sends
            .get(capability_id)
            .ok_or_else(|| self.not_granted(capability_id))?;
        let adapter = &granted.adapter;
        let body = granted.data.body(&|| self.checkpoint())?;
        let mut headers = vec![(
            "Content-Type".to_owned(),
            granted.data.class().content_type().to_owned(),
        )];
        if adapter.auth == AdapterAuth::Bearer {
            let field = granted.credential.as_deref().ok_or_else(|| {
                Error::internal(format!("adapter {} has no credential field", adapter.id))
            })?;
            let secret = self.secret(field)?;
            headers.push((
                "Authorization".to_owned(),
                format!("Bearer {}", secret.expose()),
            ));
        }
        let request = TransportRequest {
            method: Method::Post,
            endpoint: granted.endpoint.clone(),
            headers,
            body,
        };
        let timeout = Duration::from_millis(adapter.timeout_ms);
        let control = &self.control;
        let mut response = Vec::new();
        let sent = granted.transport.get().and_then(|transport| {
            transport.send(
                &request,
                SendOptions {
                    max_request_bytes: adapter.max_request_bytes,
                    max_response_bytes: adapter.max_response_bytes,
                    connect_timeout: timeout,
                    read_timeout: timeout,
                    total_timeout: timeout,
                    redirects: RedirectPolicy::default(),
                    cancel: &|| control.is_cancelled(),
                    progress: &mut |_, _| {},
                },
                &mut response,
            )
        });
        // The credential was copied into the header; it is cleared as soon as the request is done.
        let TransportRequest { mut headers, .. } = request;
        for (_, value) in &mut headers {
            value.zeroize();
        }
        let sent = sent?;
        if !(200..300).contains(&sent.status) {
            return Err(Error::file_access(format!(
                "{} answered {}",
                adapter.id, sent.status
            )));
        }
        Ok(response)
    }

    /// The origin a granted `remote-image-request` capability sends to, as its grant names it.
    pub fn origin(&self, capability_id: &str) -> Result<String, Error> {
        self.sends
            .get(capability_id)
            .map(|granted| granted.endpoint.origin())
            .ok_or_else(|| self.not_granted(capability_id))
    }

    /// Publish `bytes` as one immutable artifact of this catalog and return its identity, which the
    /// task returns for a client to apply. The bytes are written and synced now; the catalog records
    /// the artifact when the task succeeds, before its job reads succeeded, and never when it fails
    /// or is cancelled. Publishing the same bytes twice yields the same artifact. A task publishes
    /// at most [`MAX_TASK_ARTIFACTS`] artifacts of [`MAX_TASK_ARTIFACT_BYTES`] together.
    pub fn publish_artifact(&self, bytes: &[u8], meta: ArtifactMeta) -> Result<ArtifactId, Error> {
        self.checkpoint()?;
        let writer = self.writer.as_ref().ok_or_else(|| {
            Error::validation(format!(
                "this job of module {} publishes no artifacts",
                self.module_id
            ))
        })?;
        {
            let published = lock(&self.outcome.published);
            let held: u64 = published.iter().map(|(record, _)| record.bytes).sum();
            if published.len() >= MAX_TASK_ARTIFACTS
                || held.saturating_add(bytes.len() as u64) > MAX_TASK_ARTIFACT_BYTES
            {
                return Err(Error::resource_limit(format!(
                    "a task publishes at most {MAX_TASK_ARTIFACTS} artifacts of {MAX_TASK_ARTIFACT_BYTES} bytes together"
                )));
            }
        }
        let (record, prepared) = writer.write(bytes, meta, &self.module_id)?;
        let id = record.id.clone();
        let mut published = lock(&self.outcome.published);
        if !published.iter().any(|(held, _)| held.id == id) {
            published.push((record, prepared));
        }
        Ok(id)
    }

    /// Report progress: a fraction of 0 to 1 when the extent is known, and a short message.
    pub fn progress(&self, fraction: Option<f64>, message: &str) {
        self.control.set_progress(fraction, message);
    }

    /// `Err(cancelled)` once the job has been cancelled; the module returns it and releases what
    /// it held. Call it between units of work.
    pub fn checkpoint(&self) -> Result<(), Error> {
        self.control.checkpoint()
    }

    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EditorService,
        capabilities::{
            data::DisclosedData,
            descriptor::{AdapterCost, DataClass},
            secrets::MemorySecretStore,
            testing::temp,
            transport::{EndpointClass, TlsTrust, TransportConfig, parse_endpoint},
        },
    };
    use lightwell_testkit::ProofEndpoint;
    use std::fs;

    fn assert_send<T: Send>() {}

    #[test]
    fn a_context_reads_only_declared_secrets_and_given_resources() {
        assert_send::<ModuleContext>();
        let store = Arc::new(MemorySecretStore::new());
        store
            .set(
                &SecretKey::new("test.module", None, "token"),
                &SecretValue::new("module-secret".into()),
            )
            .unwrap();
        store
            .set(
                &SecretKey::new("test.module", Some("profile-1"), "api-key"),
                &SecretValue::new("profile-secret".into()),
            )
            .unwrap();
        let control = JobControl::new();
        let mut values = Map::new();
        values.insert("strength".into(), Value::from(0.5));
        let context = ModuleContext::new("test.module", store.clone(), control.clone())
            .with_settings(values, vec!["token".into(), "unset".into()])
            .with_resource("palette", PathBuf::from("/installed/palette"));
        assert_eq!(
            context.secret("api-key").unwrap_err().kind,
            ErrorKind::Validation,
            "a profile secret needs the job's profile"
        );
        let context = context.with_profile(
            ProfileView {
                id: "profile-1".into(),
                adapter: "echo".into(),
                values: Map::new(),
            },
            vec!["api-key".into()],
        );
        assert_eq!(context.module_id(), "test.module");
        assert_eq!(context.value("strength"), Some(&Value::from(0.5)));
        assert_eq!(context.secret("token").unwrap().expose(), "module-secret");
        assert_eq!(
            context.secret("api-key").unwrap().expose(),
            "profile-secret"
        );
        let reads = store.calls().read;
        let error = context.secret("other").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            store.calls().read,
            reads,
            "an undeclared name never asks the store"
        );
        assert_eq!(
            context.secret("unset").unwrap_err().kind,
            ErrorKind::NotReady
        );
        assert_eq!(
            context.resource_path("palette").unwrap(),
            Path::new("/installed/palette")
        );
        assert_eq!(
            context.resource_path("model").unwrap_err().kind,
            ErrorKind::NotReady
        );
        let debug = format!("{context:?}");
        assert!(debug.contains("test.module"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("/installed"), "{debug}");
        context.progress(Some(0.25), "loading");
        assert!(context.checkpoint().is_ok());
        control.cancel("settings changed");
        assert!(context.is_cancelled());
        let error = context.checkpoint().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.detail, "settings changed");
    }

    #[test]
    fn a_context_publishes_bounded_deduplicated_artifacts_and_checks_cancellation_first() {
        let root = temp("context-publish");
        fs::create_dir_all(&root).unwrap();
        let control = JobControl::new();
        let outcome = Arc::new(TaskOutcome::default());
        let writer = EditorService::open(&root.join("catalog.sqlite"))
            .unwrap()
            .artifact_writer()
            .unwrap();
        let context = ModuleContext::new(
            "test.module",
            Arc::new(MemorySecretStore::new()),
            control.clone(),
        )
        .with_artifacts(writer, outcome.clone());
        assert_eq!(
            context.send("echo").unwrap_err().kind,
            ErrorKind::Validation
        );
        assert_eq!(
            context.origin("echo").unwrap_err().kind,
            ErrorKind::Validation
        );
        // Publishing is bounded and deduplicated; the same bytes are one artifact.
        let meta = ArtifactMeta {
            kind: "test".into(),
            width: None,
            height: None,
            colour: None,
        };
        let first = context
            .publish_artifact(b"twelve bytes", meta.clone())
            .unwrap();
        let again = context
            .publish_artifact(b"twelve bytes", meta.clone())
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(outcome.published_ids(), std::slice::from_ref(&first));
        for index in 1..MAX_TASK_ARTIFACTS {
            context
                .publish_artifact(format!("artifact {index}").as_bytes(), meta.clone())
                .unwrap();
        }
        assert_eq!(
            context
                .publish_artifact(b"one too many", meta.clone())
                .unwrap_err()
                .kind,
            ErrorKind::ResourceLimit
        );
        assert_eq!(outcome.take_published().len(), MAX_TASK_ARTIFACTS);
        assert!(outcome.published_ids().is_empty());
        // Every capability call checks the cancel flag first.
        control.cancel("the job was cancelled");
        for error in [
            context.send("echo").unwrap_err(),
            context.publish_artifact(b"late", meta).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Cancelled);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_send_posts_the_sampled_grid_with_the_bearer_credential_and_maps_statuses() {
        let endpoint = ProofEndpoint::start("proof-key").unwrap();
        let store = Arc::new(MemorySecretStore::new());
        let key = SecretKey::new("test.module", Some("profile-1"), "api-key");
        store
            .set(&key, &SecretValue::new("proof-key".into()))
            .unwrap();
        let root = temp("context-send");
        fs::create_dir_all(&root).unwrap();
        let mut service = EditorService::open(&root.join("catalog.sqlite")).unwrap();
        let photo =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let state = service.import(&photo).unwrap();
        let (asset, entry) = (state.asset.id, state.current_entry.id);
        // The grid an independent client reads with one point sample per cell centre.
        let (width, height) = (state.asset.width, state.asset.height);
        let samples: Vec<[u8; 3]> = (0..8)
            .flat_map(|row| (0..8).map(move |column| (column, row)))
            .map(|(column, row)| {
                let rgba = service
                    .sample_entry(
                        &asset,
                        &entry,
                        (2 * column + 1) * width / 16,
                        (2 * row + 1) * height / 16,
                    )
                    .unwrap()
                    .rgba;
                [rgba[0], rgba[1], rgba[2]]
            })
            .collect();
        let adapter = AdapterDescriptor {
            id: "proof-echo".into(),
            title: "Proof echo".into(),
            auth: AdapterAuth::Bearer,
            data: vec![DataClass::SampleGrid8],
            max_request_bytes: 4096,
            max_response_bytes: 4096,
            timeout_ms: 5000,
            retention: None,
            cost: AdapterCost::Free,
        };
        let context = |control: Arc<JobControl>| {
            ModuleContext::new("test.module", store.clone(), control)
                .with_profile(
                    ProfileView {
                        id: "profile-1".into(),
                        adapter: "proof-echo".into(),
                        values: Map::new(),
                    },
                    vec!["api-key".into()],
                )
                .with_send(
                    "echo",
                    GrantedSend {
                        transport: Arc::new(SharedTransport::new(TransportConfig {
                            trust: TlsTrust::Roots(Vec::new()),
                            ..TransportConfig::default()
                        })),
                        endpoint: parse_endpoint(
                            &endpoint.generate_url(),
                            &[EndpointClass::Loopback],
                        )
                        .unwrap(),
                        adapter: adapter.clone(),
                        credential: Some("api-key".into()),
                        data: DisclosedData::new(
                            DataClass::SampleGrid8,
                            service.sample_plan(&asset).unwrap(),
                        ),
                    },
                )
        };
        let sending = context(JobControl::new());
        assert_eq!(sending.origin("echo").unwrap(), endpoint.base_url());
        let answer: Value = serde_json::from_slice(&sending.send("echo").unwrap()).unwrap();
        let received = endpoint.requests();
        let last = received.last().unwrap();
        assert_eq!(answer["rgb"], serde_json::json!(last.rgb.unwrap()));
        assert_eq!(last.status, 200);
        assert!(last.authorized);
        assert_eq!(
            last.body_bytes,
            DataClass::SampleGrid8.request_bytes() as usize
        );
        assert_eq!(last.samples.as_deref(), Some(samples.as_slice()));
        assert_eq!(
            last.header("content-type"),
            Some("application/json"),
            "the class's media type"
        );
        assert!(!format!("{received:?}").contains("proof-key"));
        endpoint.fail_next(500);
        let failed = sending.send("echo").unwrap_err();
        assert_eq!(
            (failed.kind, failed.detail.as_str()),
            (ErrorKind::FileAccess, "proof-echo answered 500")
        );
        store
            .set(&key, &SecretValue::new("wrong-key".into()))
            .unwrap();
        let refused = sending.send("echo").unwrap_err();
        assert_eq!(refused.detail, "proof-echo answered 401");
        assert!(!endpoint.requests().last().unwrap().authorized);
        store.clear(&key).unwrap();
        assert_eq!(
            sending.send("echo").unwrap_err().kind,
            ErrorKind::NotReady,
            "a missing credential sends nothing"
        );
        let cancelled = JobControl::new();
        cancelled.cancel("permission revoked");
        let before = endpoint.requests().len();
        assert_eq!(
            context(cancelled).send("echo").unwrap_err().detail,
            "permission revoked"
        );
        assert_eq!(endpoint.requests().len(), before, "nothing was sent");
        let _ = fs::remove_dir_all(root);
    }
}
