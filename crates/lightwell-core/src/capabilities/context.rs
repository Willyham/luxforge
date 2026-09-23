//! What a module receives on the capability worker: the host's view of its settings, the secrets
//! it declares, the resources its job was given, and the job's progress and cancellation. It is
//! the only way module code reaches anything the host owns, and it enforces what the job runs
//! under: a module never names a key, a path or a URL of its own. See
//! `docs/design/module-capabilities.md#outcome-and-boundary`.
//!
//! The host builds one per job on the owner, where settings are read, and moves it to the worker.
//! Each capability a job is granted adds one field and one method here, set by the host with a
//! `with_*` builder before the job is queued: a task's profile (the `profile` and
//! `profile_secret_fields` fields `secret` already consults), its selected file, its transport with
//! the endpoint and grant it may send under, and the artifact writer it publishes through. Module
//! code only ever receives what the host checked.
use super::{
    jobs::JobControl,
    secrets::{SecretKey, SecretStore, SecretValue},
};
use crate::{Error, ErrorKind};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// One provider profile as a task sees it: its identity, the adapter it names and its non-secret
/// values.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileView {
    pub id: String,
    pub adapter: String,
    pub values: Map<String, Value>,
}

/// A module's capability context for one job. `Send`, so it moves to the worker with the job.
pub struct ModuleContext {
    module_id: String,
    /// The effective, valid, non-secret module-level setting values.
    values: Map<String, Value>,
    /// The module-level secret fields it declares.
    secret_fields: Vec<String>,
    profile: Option<ProfileView>,
    /// The secret fields of the profile, when the job has one.
    profile_secret_fields: Vec<String>,
    secrets: Arc<dyn SecretStore>,
    /// The installed resources the job was given, by resource identity.
    resources: BTreeMap<String, PathBuf>,
    control: Arc<JobControl>,
}

impl std::fmt::Debug for ModuleContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleContext")
            .field("module_id", &self.module_id)
            .field("values", &self.values)
            .field("profile", &self.profile)
            .field("resources", &self.resources)
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

    /// One installed resource the job may read, at the path the host installed it to.
    pub(crate) fn with_resource(mut self, resource_id: &str, path: PathBuf) -> Self {
        self.resources.insert(resource_id.to_owned(), path);
        self
    }

    pub fn module_id(&self) -> &str {
        &self.module_id
    }

    /// Every valid, non-secret module-level value, defaults included.
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
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "module {} declares no secret setting {setting_id} for this job",
                    self.module_id
                ),
            ));
        };
        self.secrets.read(&key)?.ok_or_else(|| {
            Error::new(
                ErrorKind::NotReady,
                format!("secret setting {setting_id} is not set"),
            )
        })
    }

    /// Where an installed resource the job was given lives. Its bytes were checked against the
    /// pinned hash when it was installed.
    pub fn resource_path(&self, resource_id: &str) -> Result<&Path, Error> {
        self.resources
            .get(resource_id)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotReady,
                    format!(
                        "resource {resource_id} of module {} is not available to this job",
                        self.module_id
                    ),
                )
            })
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
    use crate::capabilities::secrets::MemorySecretStore;

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
        let mut context = ModuleContext::new("test.module", store.clone(), control.clone())
            .with_settings(values, vec!["token".into(), "unset".into()])
            .with_resource("palette", PathBuf::from("/installed/palette"));
        assert_eq!(
            context.secret("api-key").unwrap_err().kind,
            ErrorKind::Validation,
            "a profile secret needs the job's profile"
        );
        context.profile = Some(ProfileView {
            id: "profile-1".into(),
            adapter: "echo".into(),
            values: Map::new(),
        });
        context.profile_secret_fields = vec!["api-key".into()];
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
        assert!(format!("{context:?}").contains("test.module"));
        assert!(!format!("{context:?}").contains("secret"));
        context.progress(Some(0.25), "loading");
        assert!(context.checkpoint().is_ok());
        control.cancel("settings changed");
        assert!(context.is_cancelled());
        let error = context.checkpoint().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.detail, "settings changed");
    }
}
