//! User-level module settings and provider profiles, outside every catalog, in one
//! [`JsonDocument`] per user. A write is one locked read-modify-write of that document that checks
//! the module's revision. A write sets values rather than changing them, so a retry either conflicts
//! on the revision it was made against or, while the owner that answered it runs, is answered from
//! the owner's request table before it gets here. A file of any other format is refused and never
//! rewritten. Secrets never enter this file: they go to the [`SecretStore`]. See
//! `docs/design/module-capabilities.md#settings-store`.
use super::{
    descriptor::{AdapterAuth, ProfilesDescriptor, SettingDescriptor, SettingsDescriptor},
    document::JsonDocument,
    secrets::{SecretKey, SecretStore, SecretValue},
    transport::parse_endpoint,
};
use crate::{Error, ErrorKind, ModuleDescriptor, Mutation, ParameterKind, check_value};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The only settings file format this build reads or writes.
pub const SETTINGS_FORMAT: u32 = 1;
/// The largest settings file, read or written.
pub const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
/// The longest profile label, in characters.
pub const MAX_PROFILE_LABEL: usize = 128;

pub const SETTINGS_FILE: &str = "settings.json";

/// The one settings method that only reads.
pub const READ: &str = "module.settings.read";
/// The API methods that write settings.
pub const SET: &str = "module.settings.set";
pub const SET_SECRET: &str = "module.settings.set-secret";
pub const CLEAR_SECRET: &str = "module.settings.clear-secret";
pub const RESET: &str = "module.settings.reset";
pub const CREATE_PROFILE: &str = "module.profile.create";
pub const REMOVE_PROFILE: &str = "module.profile.remove";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// `{format: 1, modules: {<module_id>: <entry>}}`. Entries stay raw JSON until a module is
/// operated on, so an entry of a module that is not registered is written back exactly as read.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    modules: Map<String, Value>,
}

/// One module's stored settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleEntry {
    schema: u32,
    revision: u64,
    #[serde(default)]
    values: Map<String, Value>,
    #[serde(default)]
    profiles: Vec<StoredProfile>,
}

impl ModuleEntry {
    fn new(schema: u32) -> Self {
        Self {
            schema,
            revision: 0,
            values: Map::new(),
            profiles: Vec::new(),
        }
    }
}

/// One provider profile as stored: its host-generated identity, the adapter it names, its label and
/// its non-secret values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredProfile {
    pub id: String,
    pub adapter: String,
    pub label: String,
    #[serde(default)]
    pub values: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteOutcome {
    Committed,
    NoOp,
}

/// A profile's identity, adapter and label, without its values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSummary {
    pub id: String,
    pub adapter: String,
    pub label: String,
}

/// What one settings write did. It holds no value of any field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteResult {
    pub module_id: String,
    pub outcome: WriteOutcome,
    /// The module's settings revision after the write; a no-op keeps it.
    pub revision: u64,
    /// The profile the write addressed, created or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// The profile a create made or a remove took away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSummary>,
    /// The fields whose value or secret changed, in the addressed scope.
    #[serde(default)]
    pub changed: Vec<String>,
    /// Whether any changed field declares `invalidates_activation`.
    #[serde(default)]
    pub invalidates_activation: bool,
}

/// A write as the host sees it: the result a client receives, and what the host needs to revoke
/// grants scoped to old values.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsWrite {
    pub result: WriteResult,
    /// The previous stored value of every changed non-secret field, `null` where none was stored.
    pub previous: Map<String, Value>,
    /// Profiles the write removed, with the values they held.
    pub removed: Vec<StoredProfile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsState {
    /// Every required module-level field holds a valid value.
    Ready,
    /// A required module-level field is missing, or a stored value is no longer valid.
    Incomplete,
    /// The stored entry has another schema or shape; it is kept untouched and only a reset clears
    /// it.
    Incompatible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueSource {
    User,
    Default,
}

/// One field as a client reads it. A secret reports only whether it is set; `secret_present` is
/// `null` when the secret store could not answer, and `error` says why. Untagged, so a plain field
/// is tried first: it needs `value` and `source`, which a secret never has.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldRead {
    Value {
        value: Value,
        default: Value,
        source: ValueSource,
        valid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Secret {
        secret_present: Option<bool>,
        valid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl FieldRead {
    pub fn valid(&self) -> bool {
        match self {
            Self::Secret { valid, .. } | Self::Value { valid, .. } => *valid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileStatus {
    Ready,
    /// A required field is missing, or a value is not valid.
    Incomplete,
    /// The adapter authenticates and the profile's credential is not set.
    MissingCredentials,
    /// The profile names an adapter the module no longer declares, or the module's stored settings
    /// are incompatible.
    Incompatible,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileRead {
    pub id: String,
    pub adapter: String,
    pub label: String,
    pub fields: BTreeMap<String, FieldRead>,
    pub status: ProfileStatus,
}

/// `module.settings.read`: every declared field's value, default, source and validity, and each
/// profile's status. Reading a settings file and asking the secret store about presence is all it
/// costs; nothing is loaded and no secret is read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsRead {
    pub module_id: String,
    /// The schema the module declares.
    pub schema: u32,
    /// The schema the stored entry has, when it differs from the declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_schema: Option<u32>,
    pub revision: u64,
    pub state: SettingsState,
    /// Why the stored settings are incompatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub fields: BTreeMap<String, FieldRead>,
    pub profiles: Vec<ProfileRead>,
}

impl SettingsRead {
    pub fn profile(&self, id: &str) -> Option<&ProfileRead> {
        self.profiles.iter().find(|profile| profile.id == id)
    }
}

/// A stored entry as far as this build can use it. An incompatible entry is kept verbatim in the
/// file; what could be recovered from it — its revision and its profile identities — lets a reset
/// keep the revision counting and clear the profiles' secrets.
struct Loaded {
    entry: ModuleEntry,
    /// `Some(reason)` when the stored entry cannot be used as it is.
    incompatible: Option<String>,
}

fn load_entry(raw: Option<&Value>, schema: u32) -> Loaded {
    let Some(raw) = raw else {
        return Loaded {
            entry: ModuleEntry::new(schema),
            incompatible: None,
        };
    };
    match serde_json::from_value::<ModuleEntry>(raw.clone()) {
        Ok(entry) if entry.schema == schema => Loaded {
            entry,
            incompatible: None,
        },
        Ok(entry) => Loaded {
            incompatible: Some(format!(
                "stored settings have schema {}; the module declares schema {schema}; module.settings.reset clears them",
                entry.schema
            )),
            entry,
        },
        Err(error) => Loaded {
            entry: salvage(raw),
            incompatible: Some(format!(
                "stored settings are not in the current shape ({error}); module.settings.reset clears them"
            )),
        },
    }
}

/// What a reset needs from an entry this build cannot read as a whole.
fn salvage(raw: &Value) -> ModuleEntry {
    let number = |name: &str| raw.get(name).and_then(Value::as_u64);
    let profiles = raw
        .get("profiles")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    ModuleEntry {
        schema: number("schema")
            .and_then(|schema| u32::try_from(schema).ok())
            .unwrap_or(0),
        revision: number("revision").unwrap_or(0),
        values: Map::new(),
        profiles: profiles
            .iter()
            .filter_map(|profile| {
                let text =
                    |name: &str| profile.get(name).and_then(Value::as_str).map(str::to_owned);
                Some(StoredProfile {
                    id: text("id")?,
                    adapter: text("adapter").unwrap_or_default(),
                    label: text("label").unwrap_or_default(),
                    values: Map::new(),
                })
            })
            .collect(),
    }
}

/// The settings a module declares, or a validation error naming the module.
fn declared(descriptor: &ModuleDescriptor) -> Result<&SettingsDescriptor, Error> {
    descriptor
        .settings
        .as_ref()
        .ok_or_else(|| validation(format!("module {} declares no settings", descriptor.id)))
}

fn declared_profiles<'a>(
    descriptor: &ModuleDescriptor,
    settings: &'a SettingsDescriptor,
) -> Result<&'a ProfilesDescriptor, Error> {
    settings
        .profiles
        .as_ref()
        .ok_or_else(|| validation(format!("module {} declares no profiles", descriptor.id)))
}

/// The fields of one scope: the module's own, or its profiles'.
fn scope<'a>(
    descriptor: &ModuleDescriptor,
    settings: &'a SettingsDescriptor,
    profile_id: Option<&str>,
) -> Result<&'a [SettingDescriptor], Error> {
    match profile_id {
        None => Ok(&settings.fields),
        Some(_) => Ok(&declared_profiles(descriptor, settings)?.fields),
    }
}

fn field<'a>(
    fields: &'a [SettingDescriptor],
    profile_id: Option<&str>,
    id: &str,
) -> Result<&'a SettingDescriptor, Error> {
    fields.iter().find(|field| field.id() == id).ok_or_else(|| {
        validation(match profile_id {
            None => format!("unknown setting {id}"),
            Some(profile) => format!("unknown setting {id} of profile {profile}"),
        })
    })
}

/// The value to store for one field, or why it is refused: the parameter vocabulary's own check of
/// its kind. An endpoint is stored as the URL the transport policy parsed, so what is stored is
/// what is used.
fn normalize(field: &SettingDescriptor, value: &Value) -> Result<Value, Error> {
    match field.kind() {
        ParameterKind::Secret { .. } => Err(validation(format!(
            "setting {} is a secret; set it with module.settings.set-secret",
            field.id()
        ))),
        ParameterKind::Endpoint { classes } => {
            check_value(&field.parameter, value)?;
            let text = value.as_str().unwrap_or_default();
            let endpoint = parse_endpoint(text, classes)?;
            Ok(Value::String(endpoint.url.as_str().to_owned()))
        }
        _ => {
            check_value(&field.parameter, value)?;
            Ok(value.clone())
        }
    }
}

/// Whether a stored value is still valid for its field: its kind may have narrowed, or a URL may no
/// longer be accepted.
fn check_stored(field: &SettingDescriptor, value: &Value) -> Result<(), Error> {
    normalize(field, value).map(|_| ())
}

fn field_read(
    module_id: &str,
    profile_id: Option<&str>,
    field: &SettingDescriptor,
    stored: &Map<String, Value>,
    secrets: &dyn SecretStore,
) -> FieldRead {
    if field.is_secret() {
        return match secrets.present(&SecretKey::new(module_id, profile_id, field.id())) {
            Ok(present) => {
                let valid = present || !field.parameter.required;
                FieldRead::Secret {
                    secret_present: Some(present),
                    valid,
                    error: (!valid).then(|| format!("setting {} is required", field.id())),
                }
            }
            Err(error) => FieldRead::Secret {
                secret_present: None,
                valid: false,
                error: Some(error.to_string()),
            },
        };
    }
    let default = field.parameter.default.clone().unwrap_or(Value::Null);
    match stored.get(field.id()) {
        Some(value) => {
            let checked = check_stored(field, value);
            FieldRead::Value {
                value: value.clone(),
                default,
                source: ValueSource::User,
                valid: checked.is_ok(),
                error: checked.err().map(|error| error.detail),
            }
        }
        None => {
            let valid = !field.parameter.required || field.parameter.default.is_some();
            FieldRead::Value {
                value: default.clone(),
                default,
                source: ValueSource::Default,
                valid,
                error: (!valid).then(|| format!("setting {} is required", field.id())),
            }
        }
    }
}

fn profile_read(
    module_id: &str,
    profiles: &ProfilesDescriptor,
    profile: &StoredProfile,
    secrets: &dyn SecretStore,
) -> ProfileRead {
    let Some(adapter) = profiles.adapter(&profile.adapter) else {
        return ProfileRead {
            id: profile.id.clone(),
            adapter: profile.adapter.clone(),
            label: profile.label.clone(),
            fields: BTreeMap::new(),
            status: ProfileStatus::Incompatible,
        };
    };
    let fields: BTreeMap<String, FieldRead> = profiles
        .fields
        .iter()
        .map(|field| {
            let read = field_read(
                module_id,
                Some(&profile.id),
                field,
                &profile.values,
                secrets,
            );
            (field.id().to_owned(), read)
        })
        .collect();
    let invalid_values = fields
        .values()
        .any(|field| matches!(field, FieldRead::Value { valid: false, .. }));
    let absent_secret = fields.values().any(|field| {
        matches!(
            field,
            FieldRead::Secret {
                secret_present: Some(false),
                ..
            }
        )
    });
    let status = if invalid_values {
        ProfileStatus::Incomplete
    } else if adapter.auth == AdapterAuth::Bearer && absent_secret {
        ProfileStatus::MissingCredentials
    } else if fields.values().any(|field| !field.valid()) {
        ProfileStatus::Incomplete
    } else {
        ProfileStatus::Ready
    };
    ProfileRead {
        id: profile.id.clone(),
        adapter: profile.adapter.clone(),
        label: profile.label.clone(),
        fields,
        status,
    }
}

fn settings_read(
    descriptor: &ModuleDescriptor,
    settings: &SettingsDescriptor,
    loaded: &Loaded,
    secrets: &dyn SecretStore,
) -> SettingsRead {
    let module_id = &descriptor.id;
    let entry = &loaded.entry;
    if let Some(reason) = &loaded.incompatible {
        return SettingsRead {
            module_id: module_id.clone(),
            schema: settings.schema,
            stored_schema: Some(entry.schema),
            revision: entry.revision,
            state: SettingsState::Incompatible,
            error: Some(reason.clone()),
            fields: BTreeMap::new(),
            profiles: entry
                .profiles
                .iter()
                .map(|profile| ProfileRead {
                    id: profile.id.clone(),
                    adapter: profile.adapter.clone(),
                    label: profile.label.clone(),
                    fields: BTreeMap::new(),
                    status: ProfileStatus::Incompatible,
                })
                .collect(),
        };
    }
    let fields: BTreeMap<String, FieldRead> = settings
        .fields
        .iter()
        .map(|field| {
            let read = field_read(module_id, None, field, &entry.values, secrets);
            (field.id().to_owned(), read)
        })
        .collect();
    let state = if fields.values().all(FieldRead::valid) {
        SettingsState::Ready
    } else {
        SettingsState::Incomplete
    };
    let profiles = match &settings.profiles {
        Some(profiles) => entry
            .profiles
            .iter()
            .map(|profile| profile_read(module_id, profiles, profile, secrets))
            .collect(),
        None => Vec::new(),
    };
    SettingsRead {
        module_id: module_id.clone(),
        schema: settings.schema,
        stored_schema: None,
        revision: entry.revision,
        state,
        error: None,
        fields,
        profiles,
    }
}

/// What one write changed, before the host numbers it.
struct Applied {
    outcome: WriteOutcome,
    profile_id: Option<String>,
    profile: Option<ProfileSummary>,
    changed: Vec<String>,
    invalidates_activation: bool,
    previous: Map<String, Value>,
    removed: Vec<StoredProfile>,
}

impl Applied {
    fn new(profile_id: Option<&str>) -> Self {
        Self {
            outcome: WriteOutcome::NoOp,
            profile_id: profile_id.map(str::to_owned),
            profile: None,
            changed: Vec::new(),
            invalidates_activation: false,
            previous: Map::new(),
            removed: Vec::new(),
        }
    }

    fn change(&mut self, field: &SettingDescriptor) {
        self.outcome = WriteOutcome::Committed;
        self.changed.push(field.id().to_owned());
        self.invalidates_activation |= field.invalidates_activation;
    }
}

fn stored_profile<'a>(
    entry: &'a mut ModuleEntry,
    profiles: &ProfilesDescriptor,
    profile_id: &str,
) -> Result<&'a mut StoredProfile, Error> {
    let profile = entry
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| validation(format!("unknown profile {profile_id}")))?;
    if profiles.adapter(&profile.adapter).is_none() {
        return Err(validation(format!(
            "profile {profile_id} names adapter {}, which the module no longer declares; remove the profile",
            profile.adapter
        )));
    }
    Ok(profile)
}

/// The settings file of one user, in one directory. Holds no state of its own: every call reads
/// the file again.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    document: JsonDocument<Document>,
}

impl SettingsStore {
    /// A store over `<dir>/settings.json`. Nothing is created until the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            document: JsonDocument::new(dir, SETTINGS_FILE, MAX_SETTINGS_BYTES, SETTINGS_FORMAT),
        }
    }

    pub fn dir(&self) -> &Path {
        self.document.dir()
    }

    /// One module's settings.
    pub fn read(
        &self,
        descriptor: &ModuleDescriptor,
        secrets: &dyn SecretStore,
    ) -> Result<SettingsRead, Error> {
        let settings = declared(descriptor)?;
        let document = self.document.read()?.unwrap_or_default();
        let loaded = load_entry(document.modules.get(&descriptor.id), settings.schema);
        Ok(settings_read(descriptor, settings, &loaded, secrets))
    }

    /// `module.settings.set`: validate every named non-secret field of one scope and commit them
    /// together; `null` returns a field to its default.
    pub fn set(
        &self,
        descriptor: &ModuleDescriptor,
        profile_id: Option<&str>,
        values: &Map<String, Value>,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        let fields = scope(descriptor, settings, profile_id)?;
        // Every value is checked before the file is touched, so one refused field commits nothing.
        let mut normalized = Vec::with_capacity(values.len());
        for (name, value) in values {
            let field = field(fields, profile_id, name)?;
            let stored = match value {
                Value::Null if !field.is_secret() => None,
                value => Some(normalize(field, value)?),
            };
            normalized.push((field, stored));
        }
        self.transact(descriptor, mutation, false, |entry| {
            let mut applied = Applied::new(profile_id);
            let stored = match profile_id {
                None => &mut entry.values,
                Some(profile_id) => {
                    let profiles = declared_profiles(descriptor, settings)?;
                    &mut stored_profile(entry, profiles, profile_id)?.values
                }
            };
            for (field, value) in normalized {
                let previous = stored.get(field.id()).cloned();
                if previous == value {
                    continue;
                }
                applied.change(field);
                applied
                    .previous
                    .insert(field.id().to_owned(), previous.unwrap_or(Value::Null));
                match value {
                    Some(value) => stored.insert(field.id().to_owned(), value),
                    None => stored.remove(field.id()),
                };
            }
            Ok(applied)
        })
    }

    /// `module.settings.set-secret`: store one secret field's value in the secret store. The value
    /// is never written here; writing a secret is idempotent.
    pub fn set_secret(
        &self,
        descriptor: &ModuleDescriptor,
        secrets: &dyn SecretStore,
        profile_id: Option<&str>,
        setting: &str,
        value: &SecretValue,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        let field = field(
            scope(descriptor, settings, profile_id)?,
            profile_id,
            setting,
        )?;
        let ParameterKind::Secret { max_length } = *field.kind() else {
            return Err(validation(format!(
                "setting {setting} is not a secret; set it with module.settings.set"
            )));
        };
        let length = value.chars();
        if length == 0 || length > max_length {
            return Err(validation(format!(
                "secret {setting} must be 1..={max_length} characters; clear it with module.settings.clear-secret"
            )));
        }
        self.transact(descriptor, mutation, false, |entry| {
            if let Some(profile_id) = profile_id {
                stored_profile(entry, declared_profiles(descriptor, settings)?, profile_id)?;
            }
            secrets.set(&SecretKey::new(&descriptor.id, profile_id, setting), value)?;
            let mut applied = Applied::new(profile_id);
            applied.change(field);
            Ok(applied)
        })
    }

    /// `module.settings.clear-secret`: remove only that secret. Clearing an absent secret is a
    /// no-op.
    pub fn clear_secret(
        &self,
        descriptor: &ModuleDescriptor,
        secrets: &dyn SecretStore,
        profile_id: Option<&str>,
        setting: &str,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        let field = field(
            scope(descriptor, settings, profile_id)?,
            profile_id,
            setting,
        )?;
        if !field.is_secret() {
            return Err(validation(format!("setting {setting} is not a secret")));
        }
        self.transact(descriptor, mutation, false, |entry| {
            if let Some(profile_id) = profile_id {
                stored_profile(entry, declared_profiles(descriptor, settings)?, profile_id)?;
            }
            let key = SecretKey::new(&descriptor.id, profile_id, setting);
            let mut applied = Applied::new(profile_id);
            if secrets.present(&key)? {
                secrets.clear(&key)?;
                applied.change(field);
            }
            Ok(applied)
        })
    }

    /// `module.settings.reset`: delete the module's stored values and profiles and clear their
    /// secrets. It is the explicit way out of an incompatible entry, so it is the one write an
    /// incompatible entry accepts. The entry keeps its revision, so the revision keeps counting.
    pub fn reset(
        &self,
        descriptor: &ModuleDescriptor,
        secrets: &dyn SecretStore,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        self.transact(descriptor, mutation, true, |entry| {
            let mut applied = Applied::new(None);
            for field in &settings.fields {
                let changed = match field.kind() {
                    ParameterKind::Secret { .. } => {
                        let key = SecretKey::new(&descriptor.id, None, field.id());
                        let present = secrets.present(&key)?;
                        secrets.clear(&key)?;
                        present
                    }
                    _ => entry.values.contains_key(field.id()),
                };
                if changed {
                    applied.change(field);
                }
            }
            // A profile's secrets are cleared under the fields the module declares now; an entry
            // from another schema can only be matched by the profile identities it recorded.
            let profile_secrets = settings
                .profiles
                .iter()
                .flat_map(|profiles| profiles.fields.iter())
                .filter(|field| field.is_secret());
            for profile in &entry.profiles {
                for field in profile_secrets.clone() {
                    secrets.clear(&SecretKey::new(
                        &descriptor.id,
                        Some(&profile.id),
                        field.id(),
                    ))?;
                }
            }
            if !entry.values.is_empty() || !entry.profiles.is_empty() {
                applied.outcome = WriteOutcome::Committed;
            }
            applied.previous = std::mem::take(&mut entry.values);
            applied.removed = std::mem::take(&mut entry.profiles);
            Ok(applied)
        })
    }

    /// `module.profile.create`: a new, empty profile of a declared adapter, with a host-generated
    /// identity.
    pub fn create_profile(
        &self,
        descriptor: &ModuleDescriptor,
        adapter: &str,
        label: &str,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        let profiles = declared_profiles(descriptor, settings)?;
        if profiles.adapter(adapter).is_none() {
            return Err(validation(format!(
                "module {} declares no adapter {adapter}",
                descriptor.id
            )));
        }
        let trimmed = label.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_PROFILE_LABEL {
            return Err(validation(format!(
                "a profile label is 1..={MAX_PROFILE_LABEL} characters"
            )));
        }
        self.transact(descriptor, mutation, false, |entry| {
            if entry.profiles.len() >= usize::from(profiles.max) {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "module {} already holds its maximum of {} profiles",
                        descriptor.id, profiles.max
                    ),
                ));
            }
            let profile = StoredProfile {
                id: format!("profile-{}", uuid::Uuid::new_v4().simple()),
                adapter: adapter.to_owned(),
                label: trimmed.to_owned(),
                values: Map::new(),
            };
            let mut applied = Applied::new(Some(&profile.id));
            applied.outcome = WriteOutcome::Committed;
            applied.profile = Some(ProfileSummary {
                id: profile.id.clone(),
                adapter: profile.adapter.clone(),
                label: profile.label.clone(),
            });
            entry.profiles.push(profile);
            Ok(applied)
        })
    }

    /// `module.profile.remove`: clear the profile's secrets, then remove it and its values. The
    /// removed profile travels back to the host with its values, so grants scoped to it can be
    /// revoked.
    pub fn remove_profile(
        &self,
        descriptor: &ModuleDescriptor,
        secrets: &dyn SecretStore,
        profile_id: &str,
        mutation: &Mutation,
    ) -> Result<SettingsWrite, Error> {
        let settings = declared(descriptor)?;
        let profiles = declared_profiles(descriptor, settings)?;
        self.transact(descriptor, mutation, false, |entry| {
            let index = entry
                .profiles
                .iter()
                .position(|profile| profile.id == profile_id)
                .ok_or_else(|| validation(format!("unknown profile {profile_id}")))?;
            let mut applied = Applied::new(Some(profile_id));
            applied.outcome = WriteOutcome::Committed;
            for field in &profiles.fields {
                let changed = match field.kind() {
                    ParameterKind::Secret { .. } => {
                        let key = SecretKey::new(&descriptor.id, Some(profile_id), field.id());
                        let present = secrets.present(&key)?;
                        secrets.clear(&key)?;
                        present
                    }
                    _ => entry.profiles[index].values.contains_key(field.id()),
                };
                if changed {
                    applied.change(field);
                }
            }
            let removed = entry.profiles.remove(index);
            applied.profile = Some(ProfileSummary {
                id: removed.id.clone(),
                adapter: removed.adapter.clone(),
                label: removed.label.clone(),
            });
            applied.removed.push(removed);
            Ok(applied)
        })
    }

    /// One locked read-modify-write of one module's entry: check the envelope, refuse an
    /// incompatible entry unless this is a reset, check the revision, apply and number the result.
    /// Only a committed change is written; a no-op or a refusal leaves the file as it was.
    fn transact(
        &self,
        descriptor: &ModuleDescriptor,
        mutation: &Mutation,
        reset: bool,
        apply: impl FnOnce(&mut ModuleEntry) -> Result<Applied, Error>,
    ) -> Result<SettingsWrite, Error> {
        mutation.validate()?;
        let settings = declared(descriptor)?;
        self.document.transact(|document| {
            let Loaded {
                mut entry,
                incompatible,
            } = load_entry(document.modules.get(&descriptor.id), settings.schema);
            if let Some(reason) = &incompatible
                && !reset
            {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!("module {}: {reason}", descriptor.id),
                ));
            }
            if entry.revision != mutation.expected_revision {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!(
                        "stale settings revision {}; the current revision of module {} is {}",
                        mutation.expected_revision, descriptor.id, entry.revision
                    ),
                ));
            }
            let mut applied = apply(&mut entry)?;
            // Clearing an entry this build cannot use is itself the change a reset makes.
            if reset && incompatible.is_some() {
                applied.outcome = WriteOutcome::Committed;
            }
            if applied.outcome == WriteOutcome::Committed {
                entry.revision = entry.revision.saturating_add(1);
                entry.schema = settings.schema;
                document.modules.insert(
                    descriptor.id.clone(),
                    serde_json::to_value(&entry)
                        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?,
                );
            }
            Ok(SettingsWrite {
                result: WriteResult {
                    module_id: descriptor.id.clone(),
                    outcome: applied.outcome,
                    revision: entry.revision,
                    profile_id: applied.profile_id,
                    profile: applied.profile,
                    changed: applied.changed,
                    invalidates_activation: applied.invalidates_activation,
                },
                previous: applied.previous,
                removed: applied.removed,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{
        secrets::MemorySecretStore,
        testing::{ADAPTER, capability_descriptor, temp},
    };
    use serde_json::json;
    use std::{fs, sync::Arc};

    fn mutation(revision: u64, request: &str) -> Mutation {
        Mutation {
            expected_revision: revision,
            request_id: request.into(),
            actor: "test".into(),
        }
    }

    fn values(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object of values").clone()
    }

    /// A settings store in its own directory, with a file beside it a file setting can select.
    struct Fixture {
        root: PathBuf,
        store: SettingsStore,
        secrets: MemorySecretStore,
        descriptor: ModuleDescriptor,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("input.bin"), b"tint").unwrap();
            Self {
                store: SettingsStore::new(root.join("modules")),
                root,
                secrets: MemorySecretStore::new(),
                descriptor: capability_descriptor(),
            }
        }

        fn file(&self) -> PathBuf {
            self.store.dir().join(SETTINGS_FILE)
        }

        fn read(&self) -> SettingsRead {
            self.store.read(&self.descriptor, &self.secrets).unwrap()
        }

        fn set(
            &self,
            profile: Option<&str>,
            value: Value,
            revision: u64,
            request: &str,
        ) -> Result<SettingsWrite, Error> {
            self.store.set(
                &self.descriptor,
                profile,
                &values(value),
                &mutation(revision, request),
            )
        }

        fn raw(&self) -> Value {
            serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn value_of(read: &SettingsRead, id: &str) -> (Value, ValueSource, bool) {
        match &read.fields[id] {
            FieldRead::Value {
                value,
                source,
                valid,
                ..
            } => (value.clone(), *source, *valid),
            other => panic!("{id} is not a plain field: {other:?}"),
        }
    }

    #[test]
    fn values_are_validated_by_kind_and_committed_together() {
        let fixture = Fixture::new("kinds");
        let refused = [
            (
                json!({"strength": 1.5}),
                "parameter strength must be a number within 0..=1",
            ),
            (
                json!({"strength": "0.5"}),
                "parameter strength must be a number",
            ),
            (
                json!({"mode": "slow"}),
                "parameter mode must be one of fast, exact",
            ),
            (
                json!({"count": 9}),
                "parameter count must be an integer within 1..=8",
            ),
            (json!({"count": 1.5}), "parameter count must be an integer"),
            (json!({"enabled": 1}), "parameter enabled must be a boolean"),
            (
                json!({"note": "x".repeat(17)}),
                "parameter note must be at most 16 characters",
            ),
            (
                json!({"note": "a\u{7}b"}),
                "parameter note must not contain control characters",
            ),
            (
                json!({"local-service": "https://example.com/"}),
                "parameter local-service: remote endpoints are not allowed here",
            ),
            (
                json!({"local-service": "ftp://127.0.0.1/"}),
                "parameter local-service: scheme ftp is not allowed",
            ),
            (
                json!({"local-service": "http://user:pass@127.0.0.1/"}),
                "parameter local-service: URLs with credentials are not allowed",
            ),
            (
                json!({"token": "abc"}),
                "setting token is a secret; set it with module.settings.set-secret",
            ),
            (json!({"missing": 1}), "unknown setting missing"),
            // One refused field commits none of the others.
            (
                json!({"strength": 0.25, "mode": "slow"}),
                "parameter mode must be one of",
            ),
        ];
        for (value, expected) in refused {
            let error = fixture.set(None, value.clone(), 0, "refused").unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{value}");
            assert!(error.detail.contains(expected), "{value}: {}", error.detail);
        }
        assert!(
            !fixture.store.dir().exists(),
            "a refused write creates nothing"
        );
        let write = fixture
            .set(
                None,
                json!({
                    "strength": 0.25, "mode": "fast", "count": 3, "enabled": false, "note": "hello",
                    "local-service": "http://127.0.0.1:8080",
                    "label": "tint",
                }),
                0,
                "all",
            )
            .unwrap();
        assert_eq!(write.result.outcome, WriteOutcome::Committed);
        assert_eq!(write.result.revision, 1);
        assert_eq!(
            write.result.changed,
            [
                "count",
                "enabled",
                "label",
                "local-service",
                "mode",
                "note",
                "strength"
            ]
        );
        assert!(
            write.result.invalidates_activation,
            "label invalidates activation"
        );
        assert!(write.previous.values().all(Value::is_null));
        let read = fixture.read();
        assert_eq!(read.revision, 1);
        assert_eq!(read.state, SettingsState::Ready);
        assert_eq!(
            value_of(&read, "local-service"),
            (json!("http://127.0.0.1:8080/"), ValueSource::User, true),
            "an endpoint is stored as the URL the policy parsed"
        );
        assert_eq!(
            value_of(&read, "label"),
            (json!("tint"), ValueSource::User, true)
        );
        assert_eq!(
            value_of(&read, "count"),
            (json!(3), ValueSource::User, true)
        );
        // `null` returns a field to its default.
        let write = fixture
            .set(None, json!({"strength": null}), 1, "default")
            .unwrap();
        assert_eq!(write.result.changed, ["strength"]);
        assert_eq!(write.previous, values(json!({"strength": 0.25})));
        assert!(!write.result.invalidates_activation);
        assert_eq!(
            value_of(&fixture.read(), "strength"),
            (json!(0.5), ValueSource::Default, true)
        );
        // A required field returns to unset with `null`, and the module is incomplete until it is
        // set again.
        let write = fixture
            .set(None, json!({"label": null}), 2, "unset-label")
            .unwrap();
        assert_eq!(write.result.changed, ["label"]);
        let read = fixture.read();
        assert_eq!(read.state, SettingsState::Incomplete);
    }

    /// The store keeps no request log: a write sets values, so a retry that reaches the store after
    /// the first attempt committed conflicts on the revision it was made against, and one made at
    /// the new revision changes nothing. The owner's request table answers a retry before it gets
    /// here (see the host's tests).
    #[test]
    fn a_stale_revision_conflicts_and_a_repeated_write_changes_nothing() {
        let fixture = Fixture::new("revisions");
        let first = fixture
            .set(None, json!({"mode": "fast"}), 0, "one")
            .unwrap();
        assert_eq!(first.result.revision, 1);
        let stale = fixture
            .set(None, json!({"mode": "exact"}), 0, "two")
            .unwrap_err();
        assert_eq!(stale.kind, ErrorKind::Conflict);
        assert_eq!(
            stale.detail,
            "stale settings revision 0; the current revision of module test.capabilities is 1"
        );
        let retry = fixture
            .set(None, json!({"mode": "fast"}), 0, "one")
            .unwrap_err();
        assert_eq!(retry.kind, ErrorKind::Conflict, "the retry is stale");
        let again = fixture
            .set(None, json!({"mode": "fast"}), 1, "one")
            .unwrap();
        assert_eq!(again.result.outcome, WriteOutcome::NoOp);
        assert_eq!(fixture.read().revision, 1);
        assert_eq!(value_of(&fixture.read(), "mode").0, json!("fast"));
        assert!(
            !fixture.raw()["modules"]["test.capabilities"]
                .as_object()
                .unwrap()
                .contains_key("requests"),
            "no request is recorded"
        );
        let invalid = fixture
            .store
            .set(
                &fixture.descriptor,
                None,
                &values(json!({"mode": "fast"})),
                &Mutation {
                    expected_revision: 1,
                    request_id: String::new(),
                    actor: "test".into(),
                },
            )
            .unwrap_err();
        assert_eq!(invalid.kind, ErrorKind::Validation);
    }

    #[test]
    fn a_write_that_changes_nothing_is_a_no_op_and_keeps_the_revision() {
        let fixture = Fixture::new("no-op");
        let empty = fixture.set(None, json!({}), 0, "empty").unwrap();
        assert_eq!(empty.result.outcome, WriteOutcome::NoOp);
        assert_eq!(empty.result.revision, 0);
        assert!(!fixture.file().exists(), "a no-op writes nothing");
        fixture
            .set(None, json!({"mode": "fast"}), 0, "one")
            .unwrap();
        let written = fs::read(fixture.file()).unwrap();
        let again = fixture
            .set(None, json!({"mode": "fast"}), 1, "two")
            .unwrap();
        assert_eq!(again.result.outcome, WriteOutcome::NoOp);
        assert_eq!(again.result.revision, 1);
        assert!(again.result.changed.is_empty());
        assert_eq!(fs::read(fixture.file()).unwrap(), written);
        // Returning an unset field to its default changes nothing either.
        let unset = fixture
            .set(None, json!({"note": null}), 1, "three")
            .unwrap();
        assert_eq!(unset.result.outcome, WriteOutcome::NoOp);
        assert_eq!(fixture.read().revision, 1);
    }

    #[test]
    fn an_unsupported_or_unreadable_file_is_refused_and_never_rewritten() {
        for (name, contents) in [
            ("format-2", br#"{"format": 2, "modules": {}}"#.to_vec()),
            ("no-marker", br#"{"modules": {}}"#.to_vec()),
            ("not-json", b"{\"format\": 1, \"modules\": {".to_vec()),
            (
                "extra",
                br#"{"format": 1, "modules": {}, "extra": true}"#.to_vec(),
            ),
        ] {
            let fixture = Fixture::new(name);
            fs::create_dir_all(fixture.store.dir()).unwrap();
            fs::write(fixture.file(), &contents).unwrap();
            let failures = [
                fixture
                    .store
                    .read(&fixture.descriptor, &fixture.secrets)
                    .map(|_| ()),
                fixture
                    .set(None, json!({"mode": "fast"}), 0, "set")
                    .map(|_| ()),
                fixture
                    .store
                    .reset(&fixture.descriptor, &fixture.secrets, &mutation(0, "reset"))
                    .map(|_| ()),
                fixture
                    .store
                    .create_profile(&fixture.descriptor, ADAPTER, "Echo", &mutation(0, "p"))
                    .map(|_| ()),
                fixture
                    .store
                    .set_secret(
                        &fixture.descriptor,
                        &fixture.secrets,
                        None,
                        "token",
                        &SecretValue::new("abc".into()),
                        &mutation(0, "secret"),
                    )
                    .map(|_| ()),
            ];
            for failure in failures {
                let error = failure.unwrap_err();
                assert_eq!(
                    error.kind,
                    ErrorKind::Incompatible,
                    "{name}: {}",
                    error.detail
                );
                assert!(
                    error.detail.ends_with("the file is kept unchanged"),
                    "{name}: {}",
                    error.detail
                );
            }
            assert_eq!(fs::read(fixture.file()).unwrap(), contents, "{name}");
            assert!(fixture.secrets.is_empty(), "{name}: no secret was stored");
        }
    }

    #[test]
    fn a_schema_change_reads_incompatible_refuses_writes_and_resets_explicitly() {
        let mut fixture = Fixture::new("schema");
        fixture
            .set(None, json!({"mode": "fast"}), 0, "one")
            .unwrap();
        let profile = fixture
            .store
            .create_profile(&fixture.descriptor, ADAPTER, "Echo", &mutation(1, "two"))
            .unwrap()
            .result
            .profile_id
            .unwrap();
        fixture
            .store
            .set_secret(
                &fixture.descriptor,
                &fixture.secrets,
                Some(&profile),
                "api-key",
                &SecretValue::new("key".into()),
                &mutation(2, "three"),
            )
            .unwrap();
        fixture.descriptor.settings.as_mut().unwrap().schema = 2;
        let read = fixture.read();
        assert_eq!(read.state, SettingsState::Incompatible);
        assert_eq!((read.schema, read.stored_schema), (2, Some(1)));
        assert_eq!(read.revision, 3);
        assert!(
            read.fields.is_empty(),
            "stored values are not reinterpreted"
        );
        assert_eq!(read.profiles.len(), 1);
        assert_eq!(read.profiles[0].status, ProfileStatus::Incompatible);
        assert!(read.error.unwrap().contains("schema 1"));
        let before = fs::read(fixture.file()).unwrap();
        for refused in [
            fixture.set(None, json!({"mode": "exact"}), 3, "four"),
            fixture.store.create_profile(
                &fixture.descriptor,
                ADAPTER,
                "Other",
                &mutation(3, "five"),
            ),
            fixture.store.clear_secret(
                &fixture.descriptor,
                &fixture.secrets,
                None,
                "token",
                &mutation(3, "six"),
            ),
        ] {
            assert_eq!(refused.unwrap_err().kind, ErrorKind::Incompatible);
        }
        assert_eq!(
            fs::read(fixture.file()).unwrap(),
            before,
            "a refused write rewrites nothing"
        );
        let reset = fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(3, "reset"))
            .unwrap();
        assert_eq!(reset.result.outcome, WriteOutcome::Committed);
        assert_eq!(reset.result.revision, 4, "the revision keeps counting");
        assert_eq!(reset.removed.len(), 1);
        assert!(
            fixture.secrets.is_empty(),
            "the profile's secret was cleared"
        );
        let read = fixture.read();
        assert_eq!(read.state, SettingsState::Incomplete, "label is required");
        assert_eq!((read.schema, read.stored_schema), (2, None));
        assert!(read.profiles.is_empty());
        assert_eq!(value_of(&read, "mode").1, ValueSource::Default);
        // A reset made against the old revision is stale, and a second reset of nothing is a no-op.
        let stale = fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(3, "reset"))
            .unwrap_err();
        assert_eq!(stale.kind, ErrorKind::Conflict);
        let again = fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(4, "again"))
            .unwrap();
        assert_eq!(again.result.outcome, WriteOutcome::NoOp);
    }

    #[test]
    fn an_entry_in_another_shape_reads_incompatible_until_reset() {
        let fixture = Fixture::new("shape");
        fs::create_dir_all(fixture.store.dir()).unwrap();
        let stored = json!({"format": 1, "modules": {"test.capabilities": {
            "schema": 1, "revision": 4, "values": "not an object",
            "profiles": [{"id": "profile-old", "adapter": ADAPTER}],
        }}});
        fs::write(fixture.file(), serde_json::to_vec(&stored).unwrap()).unwrap();
        fixture
            .secrets
            .set(
                &SecretKey::new("test.capabilities", Some("profile-old"), "api-key"),
                &SecretValue::new("old".into()),
            )
            .unwrap();
        let read = fixture.read();
        assert_eq!(read.state, SettingsState::Incompatible);
        assert_eq!(read.revision, 4);
        assert!(read.error.unwrap().contains("not in the current shape"));
        assert_eq!(read.profiles[0].id, "profile-old");
        let refused = fixture
            .set(None, json!({"mode": "fast"}), 4, "set")
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Incompatible);
        let reset = fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(4, "reset"))
            .unwrap();
        assert_eq!(reset.result.revision, 5);
        assert!(
            fixture.secrets.is_empty(),
            "the old profile's secret was cleared"
        );
        assert_eq!(fixture.read().state, SettingsState::Incomplete);
    }

    #[test]
    fn entries_of_unregistered_modules_are_kept_verbatim() {
        let fixture = Fixture::new("verbatim");
        fs::create_dir_all(fixture.store.dir()).unwrap();
        let other = json!({"anything": [1, 2.5, {"z": null, "a": "text"}], "schema": 99});
        fs::write(
            fixture.file(),
            serde_json::to_vec(&json!({"format": 1, "modules": {"other.module": other}})).unwrap(),
        )
        .unwrap();
        fixture
            .set(None, json!({"mode": "fast"}), 0, "one")
            .unwrap();
        fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(1, "reset"))
            .unwrap();
        let raw = fixture.raw();
        assert_eq!(raw["modules"]["other.module"], other);
        assert_eq!(raw["format"], json!(SETTINGS_FORMAT));
    }

    #[test]
    fn the_file_is_bounded_to_one_mebibyte() {
        let fixture = Fixture::new("bounded");
        fs::create_dir_all(fixture.store.dir()).unwrap();
        // A file just under the limit is read; a write that would grow it past the limit is
        // refused and leaves it as it was.
        let blob = "x".repeat(MAX_SETTINGS_BYTES as usize - 200);
        let near =
            serde_json::to_vec(&json!({"format": 1, "modules": {"other.module": {"blob": blob}}}))
                .unwrap();
        assert!(near.len() as u64 <= MAX_SETTINGS_BYTES);
        fs::write(fixture.file(), &near).unwrap();
        assert_eq!(fixture.read().revision, 0);
        let error = fixture
            .set(None, json!({"note": "hello"}), 0, "grow")
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
        assert_eq!(fs::read(fixture.file()).unwrap(), near);
        assert!(
            !fixture
                .store
                .dir()
                .join(format!("{SETTINGS_FILE}.tmp"))
                .exists()
        );
        // A file over the limit is refused without being read whole, and kept.
        let mut over = near.clone();
        over.resize(MAX_SETTINGS_BYTES as usize + 1, b' ');
        fs::write(fixture.file(), &over).unwrap();
        let error = fixture
            .store
            .read(&fixture.descriptor, &fixture.secrets)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(fs::read(fixture.file()).unwrap(), over);
    }

    #[test]
    fn two_writers_in_parallel_lose_no_update() {
        let fixture = Fixture::new("parallel");
        let dir = fixture.store.dir().to_path_buf();
        let writers: Vec<_> = [("note", "a"), ("mode", "b")]
            .into_iter()
            .map(|(field, prefix)| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    // Each writer has its own store instance, as a second process would.
                    let store = SettingsStore::new(dir);
                    let descriptor = capability_descriptor();
                    let secrets = MemorySecretStore::new();
                    for index in 0..20 {
                        let value = match field {
                            "note" => json!(format!("{prefix}{index}")),
                            _ => json!(if index % 2 == 0 { "fast" } else { "exact" }),
                        };
                        loop {
                            let revision = store.read(&descriptor, &secrets).unwrap().revision;
                            let request = format!("{prefix}-{index}");
                            match store.set(
                                &descriptor,
                                None,
                                &values(json!({field: value})),
                                &mutation(revision, &request),
                            ) {
                                Ok(write) => {
                                    assert_eq!(write.result.outcome, WriteOutcome::Committed);
                                    break;
                                }
                                // Another writer committed first: read again and retry.
                                Err(error) if error.kind == ErrorKind::Conflict => {}
                                Err(error) => panic!("{error}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let read = fixture.read();
        assert_eq!(read.revision, 40, "every committed write was counted once");
        assert_eq!(value_of(&read, "note").0, json!("a19"));
        assert_eq!(value_of(&read, "mode").0, json!("exact"));
    }

    #[test]
    fn profiles_are_created_bounded_and_removed_with_their_secrets() {
        let fixture = Fixture::new("profiles");
        let secrets = Arc::new(MemorySecretStore::new());
        let create = |label: &str, revision: u64, request: &str| {
            fixture.store.create_profile(
                &fixture.descriptor,
                ADAPTER,
                label,
                &mutation(revision, request),
            )
        };
        let first = create(" First ", 0, "one").unwrap();
        let id = first.result.profile_id.clone().unwrap();
        let hex = id.strip_prefix("profile-").expect("a profile identity");
        assert!(hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            first.result.profile,
            Some(ProfileSummary {
                id: id.clone(),
                adapter: ADAPTER.into(),
                label: "First".into(),
            })
        );
        let second = create("Second", 1, "two")
            .unwrap()
            .result
            .profile_id
            .unwrap();
        let full = create("Third", 2, "three").unwrap_err();
        assert_eq!(full.kind, ErrorKind::ResourceLimit);
        assert!(full.detail.contains("maximum of 2 profiles"));
        assert_eq!(
            create(" ", 2, "four").unwrap_err().kind,
            ErrorKind::Validation
        );
        let unknown = fixture
            .store
            .create_profile(&fixture.descriptor, "missing", "X", &mutation(2, "five"))
            .unwrap_err();
        assert!(unknown.detail.contains("declares no adapter missing"));
        // Profile fields are set per profile and validated by kind.
        let write = fixture
            .set(
                Some(&id),
                json!({"endpoint": "https://example.com/v1", "model": "large"}),
                2,
                "values",
            )
            .unwrap();
        assert_eq!(write.result.changed, ["endpoint", "model"]);
        assert!(
            write.result.invalidates_activation,
            "the endpoint invalidates activation"
        );
        assert_eq!(write.result.profile_id.as_deref(), Some(id.as_str()));
        let refused = fixture
            .set(
                Some(&second),
                json!({"endpoint": "http://example.com/"}),
                3,
                "http",
            )
            .unwrap_err();
        assert!(refused.detail.contains("a remote endpoint must use https"));
        let unknown = fixture
            .set(
                Some("profile-missing"),
                json!({"model": "large"}),
                3,
                "missing",
            )
            .unwrap_err();
        assert_eq!(unknown.detail, "unknown profile profile-missing");
        let module_field = fixture
            .set(Some(&id), json!({"mode": "fast"}), 3, "scope")
            .unwrap_err();
        assert_eq!(
            module_field.detail,
            format!("unknown setting mode of profile {id}")
        );
        let read = fixture
            .store
            .read(&fixture.descriptor, secrets.as_ref())
            .unwrap();
        let profile = read.profile(&id).unwrap();
        assert_eq!(profile.status, ProfileStatus::MissingCredentials);
        assert_eq!(
            read.profile(&second).unwrap().status,
            ProfileStatus::Incomplete
        );
        fixture
            .store
            .set_secret(
                &fixture.descriptor,
                secrets.as_ref(),
                Some(&id),
                "api-key",
                &SecretValue::new("key".into()),
                &mutation(3, "key"),
            )
            .unwrap();
        let read = fixture
            .store
            .read(&fixture.descriptor, secrets.as_ref())
            .unwrap();
        assert_eq!(read.profile(&id).unwrap().status, ProfileStatus::Ready);
        // A client parsing the read gets back exactly what was reported, plain and secret alike.
        let reported = serde_json::to_value(&read).unwrap();
        assert_eq!(
            serde_json::from_value::<SettingsRead>(reported).unwrap(),
            read
        );
        assert_eq!(
            read.profile(&id).unwrap().fields["api-key"],
            FieldRead::Secret {
                secret_present: Some(true),
                valid: true,
                error: None,
            }
        );
        // Removing clears the profile's secrets first, then its values.
        let removed = fixture
            .store
            .remove_profile(
                &fixture.descriptor,
                secrets.as_ref(),
                &id,
                &mutation(4, "remove"),
            )
            .unwrap();
        assert_eq!(removed.result.outcome, WriteOutcome::Committed);
        assert_eq!(removed.result.revision, 5);
        assert_eq!(removed.result.changed, ["endpoint", "api-key", "model"]);
        assert_eq!(removed.removed.len(), 1);
        assert_eq!(
            removed.removed[0].values["endpoint"],
            json!("https://example.com/v1"),
            "the host receives the removed profile's values to revoke grants by"
        );
        assert!(secrets.is_empty());
        let read = fixture
            .store
            .read(&fixture.descriptor, secrets.as_ref())
            .unwrap();
        assert_eq!(read.profiles.len(), 1);
        assert_eq!(read.profiles[0].id, second);
        let again = fixture
            .store
            .remove_profile(
                &fixture.descriptor,
                secrets.as_ref(),
                &id,
                &mutation(5, "again"),
            )
            .unwrap_err();
        assert_eq!(again.detail, format!("unknown profile {id}"));
    }

    #[test]
    fn secrets_are_set_cleared_and_reported_by_presence_only() {
        let fixture = Fixture::new("secrets");
        let set = |value: &str, revision: u64, request: &str| {
            fixture.store.set_secret(
                &fixture.descriptor,
                &fixture.secrets,
                None,
                "token",
                &SecretValue::new(value.into()),
                &mutation(revision, request),
            )
        };
        let write = set("abc", 0, "one").unwrap();
        assert_eq!(write.result.outcome, WriteOutcome::Committed);
        assert_eq!(write.result.changed, ["token"]);
        assert!(
            write.previous.is_empty(),
            "a secret has no previous value to report"
        );
        assert_eq!(
            fixture.read().fields["token"],
            FieldRead::Secret {
                secret_present: Some(true),
                valid: true,
                error: None,
            }
        );
        // A second write against the old revision is stale and stores nothing.
        let stale = set("another", 0, "two").unwrap_err();
        assert_eq!(stale.kind, ErrorKind::Conflict);
        let key = SecretKey::new("test.capabilities", None, "token");
        assert_eq!(fixture.secrets.read(&key).unwrap().unwrap().expose(), "abc");
        assert_eq!(set("", 1, "empty").unwrap_err().kind, ErrorKind::Validation);
        assert!(
            set(&"x".repeat(65), 1, "long")
                .unwrap_err()
                .detail
                .contains("must be 1..=64 characters")
        );
        let not_secret = fixture
            .store
            .set_secret(
                &fixture.descriptor,
                &fixture.secrets,
                None,
                "note",
                &SecretValue::new("abc".into()),
                &mutation(1, "note"),
            )
            .unwrap_err();
        assert!(not_secret.detail.contains("setting note is not a secret"));
        let clear = |revision: u64, request: &str| {
            fixture.store.clear_secret(
                &fixture.descriptor,
                &fixture.secrets,
                None,
                "token",
                &mutation(revision, request),
            )
        };
        let cleared = clear(1, "clear").unwrap();
        assert_eq!(cleared.result.outcome, WriteOutcome::Committed);
        assert_eq!(cleared.result.revision, 2);
        let absent = clear(2, "again").unwrap();
        assert_eq!(absent.result.outcome, WriteOutcome::NoOp);
        assert_eq!(absent.result.revision, 2);
        assert_eq!(
            fixture.read().fields["token"],
            FieldRead::Secret {
                secret_present: Some(false),
                valid: true,
                error: None,
            }
        );
        let text = String::from_utf8(fs::read(fixture.file()).unwrap()).unwrap();
        assert!(!text.contains("abc") && !text.contains("another"));
    }

    #[test]
    fn a_failing_secret_store_is_not_ready_and_nothing_is_kept_in_plain_text() {
        let fixture = Fixture::new("locked");
        fixture.secrets.fail_with(Some(Error::new(
            ErrorKind::NotReady,
            "the macOS Keychain is locked",
        )));
        let error = fixture
            .store
            .set_secret(
                &fixture.descriptor,
                &fixture.secrets,
                None,
                "token",
                &SecretValue::new("plain-secret".into()),
                &mutation(0, "one"),
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotReady);
        assert_eq!(error.detail, "the macOS Keychain is locked");
        assert!(
            !fixture.file().exists(),
            "a failed secret write records nothing"
        );
        // A read still answers, and says which secret it could not check.
        let read = fixture.read();
        assert_eq!(
            read.fields["token"],
            FieldRead::Secret {
                secret_present: None,
                valid: false,
                error: Some("not-ready: the macOS Keychain is locked".into()),
            }
        );
        assert_eq!(read.state, SettingsState::Incomplete);
        // Non-secret settings still work, and nothing anywhere holds the secret.
        fixture
            .set(None, json!({"mode": "fast"}), 0, "two")
            .unwrap();
        for entry in fs::read_dir(fixture.store.dir()).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("plain-secret"));
        }
        let reset = fixture
            .store
            .reset(&fixture.descriptor, &fixture.secrets, &mutation(1, "reset"))
            .unwrap_err();
        assert_eq!(
            reset.kind,
            ErrorKind::NotReady,
            "a reset cannot clear the secret"
        );
        assert_eq!(fixture.read().revision, 1);
    }
}
