//! The capability host the catalog owner holds: where settings live, which secret store holds
//! credentials, and the owner-answered methods over them. Every call here is a short file or
//! secret-store attribute call; nothing hashes, downloads, loads or reads a secret's data on the
//! owner. See `docs/design/module-capabilities.md`.
use super::{
    secrets::{SecretStore, SecretValue, UnavailableSecretStore},
    settings::{
        CLEAR_SECRET, CREATE_PROFILE, READ, REMOVE_PROFILE, RESET, SET, SET_SECRET, SettingsStore,
        SettingsWrite,
    },
};
use crate::{ApiRequest, ClientId, Error, ErrorKind, ModuleDescriptor, ModuleRegistry, Mutation};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::{path::PathBuf, sync::Arc};

/// Where the host keeps what it owns for modules. Tests and evidence runs point every directory at
/// an isolated location and pass an in-memory secret store, so they never touch the person's
/// configuration or login keychain.
#[derive(Clone)]
pub struct HostConfig {
    /// The directory that holds `settings.json`, normally `<config>/modules`. `None` refuses every
    /// settings method with `not-ready`. Nothing is created until the first settings write.
    pub config_dir: Option<PathBuf>,
    /// The directory managed resources are installed under, normally `<data>/modules/resources`.
    pub resource_dir: Option<PathBuf>,
    pub secrets: Arc<dyn SecretStore>,
}

impl HostConfig {
    /// No directories and no secure store: every settings and secret method reports `not-ready`.
    pub fn unconfigured() -> Self {
        Self {
            config_dir: None,
            resource_dir: None,
            secrets: Arc::new(UnavailableSecretStore::new("no secure store is configured")),
        }
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("config_dir", &self.config_dir)
            .field("resource_dir", &self.resource_dir)
            .field("secrets", &self.secrets.name())
            .finish()
    }
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn params<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    serde_json::from_value(value.clone()).map_err(|error| validation(error.to_string()))
}

fn encode(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleParams {
    module_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    module_id: String,
    #[serde(default)]
    profile_id: Option<String>,
    values: Map<String, Value>,
    mutation: Mutation,
}

/// `set-secret` without its value, which is taken out of the request before anything else reads
/// it, and `clear-secret`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretParams {
    module_id: String,
    #[serde(default)]
    profile_id: Option<String>,
    setting: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetParams {
    module_id: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProfileParams {
    module_id: String,
    adapter: String,
    label: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveProfileParams {
    module_id: String,
    profile_id: String,
    mutation: Mutation,
}

/// Split a `set-secret` request into its secret and the rest. The value is moved straight into a
/// [`SecretValue`], and a malformed one is refused without echoing it, which serde's own message
/// for a wrong type would do.
fn secret_params(request: &Value) -> Result<(SecretParams, SecretValue), Error> {
    let mut object = match request {
        Value::Object(object) => object.clone(),
        _ => return Err(validation("params must be a JSON object")),
    };
    let value = match object.remove("value") {
        Some(Value::String(value)) => SecretValue::new(value),
        Some(_) => return Err(validation("value must be a string")),
        None => return Err(validation("missing field `value`")),
    };
    Ok((params(&Value::Object(object))?, value))
}

/// The owner's capability state: the configuration, and the settings store over its directory.
pub(crate) struct CapabilityHost {
    config: HostConfig,
    settings: Option<SettingsStore>,
}

impl CapabilityHost {
    pub(crate) fn new(config: HostConfig) -> Self {
        Self {
            settings: config.config_dir.clone().map(SettingsStore::new),
            config,
        }
    }

    fn settings(&self) -> Result<&SettingsStore, Error> {
        self.settings.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::NotReady,
                "no application data directory is configured",
            )
        })
    }

    fn secrets(&self) -> &dyn SecretStore {
        self.config.secrets.as_ref()
    }

    /// Answer one capability method, or `None` when the method is not one this host owns. `client`
    /// is the caller, for the methods whose authority depends on who asks.
    pub(crate) fn answer(
        &mut self,
        registry: &ModuleRegistry,
        _client: ClientId,
        request: &ApiRequest,
    ) -> Option<Result<Value, Error>> {
        let params = &request.params;
        Some(match request.method.as_str() {
            READ => self.read(registry, params),
            SET => self.set(registry, params),
            SET_SECRET => self.set_secret(registry, params),
            CLEAR_SECRET => self.clear_secret(registry, params),
            RESET => self.reset(registry, params),
            CREATE_PROFILE => self.create_profile(registry, params),
            REMOVE_PROFILE => self.remove_profile(registry, params),
            _ => return None,
        })
    }

    fn read(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        encode(self.settings()?.read(descriptor, self.secrets())?)
    }

    fn set(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: SetParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.set(
            descriptor,
            request.profile_id.as_deref(),
            &request.values,
            &request.mutation,
        )?;
        self.written(descriptor, write)
    }

    fn set_secret(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let (request, value) = secret_params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.set_secret(
            descriptor,
            self.secrets(),
            request.profile_id.as_deref(),
            &request.setting,
            &value,
            &request.mutation,
        )?;
        self.written(descriptor, write)
    }

    fn clear_secret(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: SecretParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.clear_secret(
            descriptor,
            self.secrets(),
            request.profile_id.as_deref(),
            &request.setting,
            &request.mutation,
        )?;
        self.written(descriptor, write)
    }

    fn reset(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: ResetParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self
            .settings()?
            .reset(descriptor, self.secrets(), &request.mutation)?;
        self.written(descriptor, write)
    }

    fn create_profile(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: CreateProfileParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.create_profile(
            descriptor,
            &request.adapter,
            &request.label,
            &request.mutation,
        )?;
        self.written(descriptor, write)
    }

    fn remove_profile(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: RemoveProfileParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.remove_profile(
            descriptor,
            self.secrets(),
            &request.profile_id,
            &request.mutation,
        )?;
        self.written(descriptor, write)
    }

    /// A write's response: its result, which a retry receives unchanged, and the module's settings
    /// as they read now. Neither holds a secret.
    fn written(&self, descriptor: &ModuleDescriptor, write: SettingsWrite) -> Result<Value, Error> {
        let mut value = encode(&write.result)?;
        value["settings"] = encode(self.settings()?.read(descriptor, self.secrets())?)?;
        Ok(value)
    }
}

/// The registered module a settings method names, which must declare settings.
fn module<'a>(registry: &'a ModuleRegistry, id: &str) -> Result<&'a ModuleDescriptor, Error> {
    let descriptor = registry
        .module(id)
        .map(|module| module.descriptor())
        .ok_or_else(|| validation(format!("unknown module {id}")))?;
    if descriptor.settings.is_none() {
        return Err(validation(format!("module {id} declares no settings")));
    }
    Ok(descriptor)
}

/// Every owner-answered capability method, for the method table's tests.
#[cfg(test)]
pub(crate) const METHODS: &[&str] = &[
    READ,
    SET,
    SET_SECRET,
    CLEAR_SECRET,
    RESET,
    CREATE_PROFILE,
    REMOVE_PROFILE,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiResponse, OwnerHandle,
        capabilities::{
            secrets::{MemorySecretStore, SecretKey},
            testing::{ADAPTER, MODULE, TASK, capability_descriptor, temp},
        },
        modules::TestModule,
        redact_request,
    };
    use serde_json::json;
    use std::{
        fs,
        path::{Path, PathBuf},
        thread::JoinHandle,
    };

    fn registry() -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(TestModule::from_descriptor(capability_descriptor()))
            .unwrap();
        Arc::new(registry)
    }

    /// A catalog, a settings directory that does not exist yet and a file a setting can select, all
    /// under one temporary root that is removed at the end.
    struct Fixture {
        root: PathBuf,
        secrets: Arc<MemorySecretStore>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("input.bin"), b"tint").unwrap();
            Self {
                root,
                secrets: Arc::new(MemorySecretStore::new()),
            }
        }

        fn config(&self) -> PathBuf {
            self.root.join("config").join("modules")
        }

        fn start(&self) -> (OwnerHandle, JoinHandle<()>) {
            OwnerHandle::start_with_host(
                &self.root.join("catalog.sqlite"),
                registry(),
                HostConfig {
                    config_dir: Some(self.config()),
                    resource_dir: None,
                    secrets: self.secrets.clone(),
                },
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn call(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> ApiResponse {
        owner
            .call(
                client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(owner: &OwnerHandle, client: ClientId, id: &str, method: &str, params: Value) -> Value {
        let response = call(owner, client, id, method, params);
        assert!(response.error.is_none(), "{id}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn failure(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> (String, String) {
        let error = call(owner, client, id, method, params)
            .error
            .unwrap_or_else(|| panic!("{id} was expected to fail"));
        (error.code, error.message)
    }

    fn mutation(revision: u64, request: &str) -> Value {
        json!({"expected_revision": revision, "request_id": request, "actor": "test"})
    }

    fn stop(owner: OwnerHandle, join: JoinHandle<()>) {
        owner.stop();
        join.join().unwrap();
    }

    /// Every file under `root`, read whole.
    fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        found
    }

    #[test]
    fn discovery_and_reopen_touch_no_settings_file_or_secret_store() {
        let fixture = Fixture::new("discovery");
        let declared = serde_json::to_value(capability_descriptor()).unwrap();
        for round in 0..2 {
            let (owner, join) = fixture.start();
            let client = owner.register();
            let modules = ok(&owner, client, "list", "module.list", json!({}));
            let listed = modules["modules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|module| module["id"] == json!(MODULE))
                .expect("the module is listed");
            assert_eq!(listed, &declared, "round {round}: exactly the declarations");
            let schema = ok(&owner, client, "schema", "schema.list", json!({}));
            let in_schema = schema["modules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|module| module["id"] == json!(MODULE))
                .unwrap();
            assert_eq!(in_schema, &declared);
            let methods = schema["methods"].as_object().unwrap();
            for method in METHODS {
                assert!(methods.contains_key(*method), "{method} is discoverable");
            }
            assert!(
                !methods.contains_key(&format!("task.{TASK}")),
                "task methods are not generated yet"
            );
            stop(owner, join);
        }
        assert_eq!(
            fixture.secrets.calls().total(),
            0,
            "discovery and reopen never ask the secret store"
        );
        assert!(
            !fixture.root.join("config").exists(),
            "discovery and reopen never create the settings directory"
        );
    }

    #[test]
    fn settings_persist_across_an_owner_restart_and_only_commits_emit_events() {
        let fixture = Fixture::new("restart");
        let input = fixture.root.join("input.bin");
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(0));
        assert_eq!(read["state"], json!("incomplete"), "input-file is required");
        assert_eq!(
            read["fields"]["strength"],
            json!({"value": 0.5, "default": 0.5, "source": "default", "valid": true})
        );
        assert_eq!(
            read["fields"]["token"],
            json!({"secret_present": false, "valid": true})
        );
        assert!(!fixture.config().exists(), "a read creates nothing");
        let set = ok(
            &owner,
            client,
            "set-1",
            SET,
            json!({
                "module_id": MODULE,
                "values": {"mode": "fast", "input-file": input},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(set["outcome"], json!("committed"));
        assert_eq!(set["revision"], json!(1));
        assert_eq!(set["changed"], json!(["input-file", "mode"]));
        assert_eq!(set["invalidates_activation"], json!(true));
        assert_eq!(set["settings"]["state"], json!("ready"));
        assert_eq!(set["settings"]["fields"]["mode"]["value"], json!("fast"));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        assert_eq!(
            events["events"],
            json!([{"sequence": 1, "method": SET, "request_id": "set-1"}])
        );
        // A no-op keeps the revision and emits nothing; a retry returns the original result.
        let again = ok(
            &owner,
            client,
            "set-2",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "fast"}, "mutation": mutation(1, "set-2")}),
        );
        assert_eq!(again["outcome"], json!("no-op"));
        assert_eq!(again["revision"], json!(1));
        let retry = ok(
            &owner,
            client,
            "set-1",
            SET,
            json!({
                "module_id": MODULE,
                "values": {"mode": "fast", "input-file": input},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(retry["deduplicated"], json!(true));
        assert_eq!(retry["revision"], json!(1));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        assert_eq!(events["current_sequence"], json!(1));
        assert_eq!(events["events"].as_array().unwrap().len(), 1);
        let (code, message) = failure(
            &owner,
            client,
            "stale",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "exact"}, "mutation": mutation(0, "stale")}),
        );
        assert_eq!(code, "conflict");
        assert!(
            message.starts_with("stale settings revision 0"),
            "{message}"
        );
        stop(owner, join);
        // A new owner over the same directory reads what the first one committed.
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(1));
        assert_eq!(read["fields"]["mode"]["value"], json!("fast"));
        assert_eq!(read["state"], json!("ready"));
        let (code, _) = failure(
            &owner,
            client,
            "stale",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "exact"}, "mutation": mutation(0, "after")}),
        );
        assert_eq!(code, "conflict");
        stop(owner, join);
    }

    #[test]
    fn profiles_and_secrets_through_the_api_report_presence_only() {
        let fixture = Fixture::new("profiles");
        let (owner, join) = fixture.start();
        let client = owner.register();
        let created = ok(
            &owner,
            client,
            "create",
            CREATE_PROFILE,
            json!({"module_id": MODULE, "adapter": ADAPTER, "label": "Echo", "mutation": mutation(0, "create")}),
        );
        let profile = created["profile"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created["profile"]["adapter"], json!(ADAPTER));
        assert_eq!(
            created["settings"]["profiles"][0]["status"],
            json!("incomplete")
        );
        ok(
            &owner,
            client,
            "endpoint",
            SET,
            json!({
                "module_id": MODULE, "profile_id": profile,
                "values": {"endpoint": "http://localhost:9/echo"}, "mutation": mutation(1, "endpoint"),
            }),
        );
        let secret = ok(
            &owner,
            client,
            "secret",
            SET_SECRET,
            json!({
                "module_id": MODULE, "profile_id": profile, "setting": "api-key",
                "value": "key-value", "mutation": mutation(2, "secret"),
            }),
        );
        assert_eq!(secret["outcome"], json!("committed"));
        let fields = &secret["settings"]["profiles"][0]["fields"];
        assert_eq!(
            fields["api-key"],
            json!({"secret_present": true, "valid": true})
        );
        assert_eq!(
            fields["endpoint"]["value"],
            json!("http://localhost:9/echo")
        );
        assert_eq!(secret["settings"]["profiles"][0]["status"], json!("ready"));
        let key = SecretKey::new(MODULE, Some(&profile), "api-key");
        assert_eq!(
            fixture.secrets.read(&key).unwrap().unwrap().expose(),
            "key-value"
        );
        let cleared = ok(
            &owner,
            client,
            "clear",
            CLEAR_SECRET,
            json!({"module_id": MODULE, "profile_id": profile, "setting": "api-key", "mutation": mutation(3, "clear")}),
        );
        assert_eq!(
            cleared["settings"]["profiles"][0]["status"],
            json!("missing-credentials")
        );
        let removed = ok(
            &owner,
            client,
            "remove",
            REMOVE_PROFILE,
            json!({"module_id": MODULE, "profile_id": profile, "mutation": mutation(4, "remove")}),
        );
        assert_eq!(removed["profile"]["id"], json!(profile));
        assert_eq!(removed["settings"]["profiles"], json!([]));
        // Nothing is left to reset, so a reset is a no-op and announces nothing.
        let reset = ok(
            &owner,
            client,
            "reset",
            RESET,
            json!({"module_id": MODULE, "mutation": mutation(5, "reset")}),
        );
        assert_eq!(reset["outcome"], json!("no-op"));
        assert_eq!(reset["revision"], json!(5));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        let methods: Vec<&str> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            [
                CREATE_PROFILE,
                SET,
                SET_SECRET,
                CLEAR_SECRET,
                REMOVE_PROFILE
            ]
        );
        // A locked store fails the secret write with not-ready and nothing is kept in plain text.
        fixture.secrets.fail_with(Some(Error::new(
            ErrorKind::NotReady,
            "the macOS Keychain is locked",
        )));
        let (code, message) = failure(
            &owner,
            client,
            "locked",
            SET_SECRET,
            json!({"module_id": MODULE, "setting": "token", "value": "locked-plain", "mutation": mutation(5, "locked")}),
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("not-ready", "the macOS Keychain is locked")
        );
        for (path, bytes) in files(&fixture.root) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("locked-plain"),
                "{} holds the secret",
                path.display()
            );
        }
        stop(owner, join);
    }

    #[test]
    fn settings_methods_refuse_without_a_directory_and_for_unknown_modules() {
        let fixture = Fixture::new("refusals");
        let (owner, join) =
            OwnerHandle::start_with(&fixture.root.join("unconfigured.sqlite"), registry()).unwrap();
        let client = owner.register();
        for (method, params) in [
            (READ, json!({"module_id": MODULE})),
            (
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": "x", "mutation": mutation(0, "a")}),
            ),
            (
                RESET,
                json!({"module_id": MODULE, "mutation": mutation(0, "b")}),
            ),
        ] {
            assert_eq!(
                failure(&owner, client, method, method, params),
                (
                    "not-ready".into(),
                    "no application data directory is configured".into()
                )
            );
        }
        stop(owner, join);
        let (owner, join) = fixture.start();
        let client = owner.register();
        for (params, expected) in [
            (
                json!({"module_id": "test.missing"}),
                "unknown module test.missing",
            ),
            (
                json!({"module_id": "lightwell.basic"}),
                "module lightwell.basic declares no settings",
            ),
            (json!({}), "missing field `module_id`"),
            (
                json!({"module_id": MODULE, "extra": 1}),
                "unknown field `extra`",
            ),
        ] {
            let (code, message) = failure(&owner, client, "read", READ, params);
            assert_eq!(code, "validation");
            assert!(message.contains(expected), "{message}");
        }
        let (code, message) = failure(
            &owner,
            client,
            "wrong",
            SET_SECRET,
            json!({"module_id": MODULE, "setting": "token", "value": 12345, "mutation": mutation(0, "c")}),
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("validation", "value must be a string")
        );
        stop(owner, join);
    }

    #[test]
    fn a_sentinel_secret_appears_on_no_observable_surface() {
        let fixture = Fixture::new("sentinel");
        let sentinel = format!("SENTINEL-{}", uuid::Uuid::new_v4().simple());
        let (owner, join) = fixture.start();
        let client = owner.register();
        let mut observed = Vec::new();
        let mut send = |id: &str, method: &str, params: Value| {
            let response = call(&owner, client, id, method, params);
            observed.push(serde_json::to_string(&response).unwrap());
            response
        };
        let created = send(
            "create",
            CREATE_PROFILE,
            json!({"module_id": MODULE, "adapter": ADAPTER, "label": "Echo", "mutation": mutation(0, "create")}),
        );
        let profile = created.result.unwrap()["profile"]["id"].clone();
        let module_secret = json!({
            "module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(1, "token"),
        });
        assert!(
            send("token", SET_SECRET, module_secret.clone())
                .error
                .is_none()
        );
        assert!(
            send("token", SET_SECRET, module_secret.clone())
                .error
                .is_none(),
            "a retry"
        );
        let profile_secret = json!({
            "module_id": MODULE, "profile_id": profile, "setting": "api-key", "value": sentinel,
            "mutation": mutation(2, "api-key"),
        });
        assert!(send("api-key", SET_SECRET, profile_secret).error.is_none());
        // Refused requests carrying the secret echo none of it.
        for (id, method, params) in [
            (
                "long",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": format!("{sentinel}{}", "x".repeat(64)), "mutation": mutation(3, "long")}),
            ),
            (
                "object",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": {"nested": sentinel}, "mutation": mutation(3, "object")}),
            ),
            (
                "array",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": [sentinel], "mutation": mutation(3, "array")}),
            ),
            (
                "extra",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "extra": sentinel, "mutation": mutation(3, "extra")}),
            ),
            (
                "wrong-setting",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "note", "value": sentinel, "mutation": mutation(3, "wrong")}),
            ),
            (
                "as-value",
                SET,
                json!({"module_id": MODULE, "values": {"token": sentinel}, "mutation": mutation(3, "as-value")}),
            ),
            (
                "clear-with-value",
                CLEAR_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(3, "clear")}),
            ),
            (
                "stale",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(0, "stale")}),
            ),
        ] {
            assert!(send(id, method, params).error.is_some(), "{id} was refused");
        }
        send("read", READ, json!({"module_id": MODULE}));
        send("events", "events.since", json!({"after": 0}));
        send("list", "module.list", json!({}));
        send("schema", "schema.list", json!({}));
        send("status", "session.state", json!({}));
        stop(owner, join);
        assert!(observed.len() >= 14);
        for text in &observed {
            assert!(
                !text.contains(&sentinel),
                "a response carries the secret: {text}"
            );
        }
        for (path, bytes) in files(&fixture.root) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(&sentinel),
                "{} holds the secret",
                path.display()
            );
        }
        // The secret did reach the store, under both of its keys.
        for key in [
            SecretKey::new(MODULE, None, "token"),
            SecretKey::new(MODULE, profile.as_str(), "api-key"),
        ] {
            assert_eq!(
                fixture.secrets.read(&key).unwrap().unwrap().expose(),
                sentinel
            );
        }
        // And a request log, evidence capture or Copy as JSON of the request carries none of it.
        let request = ApiRequest {
            id: "token".into(),
            method: SET_SECRET.into(),
            params: module_secret,
            token: None,
        };
        assert!(serde_json::to_string(&request).unwrap().contains(&sentinel));
        let redacted = serde_json::to_string(&redact_request(&request)).unwrap();
        assert!(!redacted.contains(&sentinel), "{redacted}");
        assert!(redacted.contains("<redacted>"));
    }
}
