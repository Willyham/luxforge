//! Plain-data capability declarations a module adds to its descriptor: typed settings and provider
//! profiles, the capabilities it may be granted, the resources it may install, what activation
//! requires and the worker tasks it offers. Registration validates them and does no I/O. See
//! `docs/design/module-capabilities.md#configuration-contract`.
//!
//! A setting is a [`ParameterDescriptor`] with a label, serialized flat exactly as a parameter is:
//! `{"name": "strength", "kind": "number", "min": 0, "max": 1, "required": false, …, "label":
//! "Strength", "invalidates_activation": false}`. A capability is serialized flat the same way.
//! Flattening rules out `deny_unknown_fields` on those two types, so an unknown field there is
//! ignored on read; every other capability type refuses unknown fields.
use super::transport::{EndpointClass, parse_endpoint};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error, ModuleDescriptor, ParameterDescriptor, ParameterKind,
    modules::{check_declaration, check_parameter_declarations},
    valid_name,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

/// Module-level and profile fields are each at most this many, so a settings panel and a read
/// result stay bounded whatever a module declares.
pub const MAX_SETTING_FIELDS: usize = 32;
/// The most provider profiles one module may hold.
pub const MAX_PROFILES: u8 = 16;
/// The largest resource a module may pin, which is also the most a download may stream to disk.
pub const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The largest request body or response an adapter may declare. It is the artifact limit, since a
/// response becomes at most one artifact, and it bounds the buffer the transport holds.
pub const MAX_ADAPTER_BYTES: u64 = 256 * 1024 * 1024;
/// The longest whole-request deadline an adapter may declare.
pub const MAX_ADAPTER_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// A resource version names a directory under the resource root, so it is a short, plain name.
const MAX_VERSION_LENGTH: usize = 64;

/// A module's user-level settings: its own fields and, optionally, named provider profiles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsDescriptor {
    /// The shape of the stored values. A stored entry with another schema is kept untouched and
    /// reads as incompatible until the module's settings are reset.
    pub schema: u32,
    #[serde(default)]
    pub fields: Vec<SettingDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<ProfilesDescriptor>,
}

impl SettingsDescriptor {
    /// A module-level field.
    pub fn field(&self, id: &str) -> Option<&SettingDescriptor> {
        self.fields.iter().find(|field| field.id() == id)
    }
}

/// One setting, declared in the module parameter vocabulary: its parameter's name is the setting's
/// identity, and its kind, `required`, default, hints and notes mean what they mean for an action's
/// parameter. A missing or invalid value of a required setting makes the module or profile
/// incomplete. What only a setting has sits beside the parameter: the label a settings view shows
/// and whether changing it deactivates the module. A secret declares only its presence anywhere it
/// is reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingDescriptor {
    #[serde(flatten)]
    pub parameter: ParameterDescriptor,
    pub label: String,
    /// Changing this field deactivates an active module, because what it loaded depends on it.
    #[serde(default)]
    pub invalidates_activation: bool,
}

impl SettingDescriptor {
    /// A setting that leaves activation alone.
    pub fn new(parameter: ParameterDescriptor, label: impl Into<String>) -> Self {
        Self {
            parameter,
            label: label.into(),
            invalidates_activation: false,
        }
    }

    /// Changing this setting deactivates an active module.
    pub fn invalidates_activation(mut self) -> Self {
        self.invalidates_activation = true;
        self
    }

    /// The setting's identity: its parameter's name.
    pub fn id(&self) -> &str {
        &self.parameter.name
    }

    pub fn kind(&self) -> &ParameterKind {
        &self.parameter.kind
    }

    pub fn is_secret(&self) -> bool {
        matches!(self.parameter.kind, ParameterKind::Secret { .. })
    }

    pub fn is_endpoint(&self) -> bool {
        matches!(self.parameter.kind, ParameterKind::Endpoint { .. })
    }
}

/// Named provider profiles: each profile names one declared adapter and holds its own values of
/// `fields`, which include the endpoint it sends to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesDescriptor {
    pub label: String,
    /// At most this many profiles, 1..=16.
    pub max: u8,
    pub adapters: Vec<AdapterDescriptor>,
    pub fields: Vec<SettingDescriptor>,
}

impl ProfilesDescriptor {
    pub fn adapter(&self, id: &str) -> Option<&AdapterDescriptor> {
        self.adapters.iter().find(|adapter| adapter.id == id)
    }

    pub fn field(&self, id: &str) -> Option<&SettingDescriptor> {
        self.fields.iter().find(|field| field.id() == id)
    }
}

/// A typed provider contract, not a URL template: what the host may send, how much, how long it may
/// take, how it authenticates and what the person is told about retention and cost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterDescriptor {
    pub id: String,
    pub title: String,
    pub auth: AdapterAuth,
    pub data: Vec<DataClass>,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    pub cost: AdapterCost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterAuth {
    None,
    /// `Authorization: Bearer` with the profile's one secret field.
    Bearer,
}

/// What a request may carry. The host builds the body from the declared class only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataClass {
    /// An 8 × 8 grid of rendered sRGB point samples of the asset's current entry, at the centres of
    /// the grid's cells over its output stage. Its body is `application/json`,
    /// `{"data_class":"sample-grid-8","width":8,"height":8,"samples":[[r,g,b],…]}` with the 64
    /// samples row by row from the top-left, each channel an 8-bit sRGB code written right-aligned
    /// in three characters, so the body is exactly [`super::data::SAMPLE_GRID_BYTES`] bytes
    /// whatever the photo holds. See [`super::data::sample_grid_body`].
    #[serde(rename = "sample-grid-8")]
    SampleGrid8,
}

/// The side of the `sample-grid-8` grid.
pub const SAMPLE_GRID_SIDE: u32 = 8;

impl DataClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::SampleGrid8 => "sample-grid-8",
        }
    }

    /// What the body carries, in the words a consent notice uses.
    pub fn describe(self) -> &'static str {
        match self {
            Self::SampleGrid8 => "an 8 × 8 grid of rendered colour samples of this photo",
        }
    }

    /// The exact size of the body the host sends for this class, which the consent notice
    /// discloses.
    pub fn request_bytes(self) -> u64 {
        match self {
            Self::SampleGrid8 => super::data::SAMPLE_GRID_BYTES as u64,
        }
    }

    /// The media type of the body the host sends for this class.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::SampleGrid8 => "application/json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterCost {
    Free,
    Paid,
    Unknown,
}

/// One capability a module may be granted, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    #[serde(flatten)]
    pub kind: CapabilityKind,
    /// Shown to the person in the consent notice.
    pub purpose: String,
}

/// The implemented capabilities. `managed-storage` and `local-runtime` are refused by name until
/// their first consumer defines them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CapabilityKind {
    /// Send one declared data class of one asset to a profile's endpoint through its adapter.
    RemoteImageRequest { adapter: String, data: DataClass },
    /// Install a declared resource from its pinned URL.
    DownloadArtifact { resource: String },
}

const CAPABILITY_KINDS: &[&str] = &["remote-image-request", "download-artifact"];
const UNIMPLEMENTED_CAPABILITY_KINDS: &[&str] = &["managed-storage", "local-runtime"];

/// A pinned file a module may install: exact bytes, hash and origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDescriptor {
    pub id: String,
    pub title: String,
    /// A plain name that becomes the version's directory.
    pub version: String,
    /// HTTPS, or HTTP to loopback, as the transport policy accepts it.
    pub url: String,
    pub bytes: u64,
    /// 64 lowercase hexadecimal digits.
    pub sha256: String,
    pub format: String,
    pub license: String,
    pub provenance: String,
    /// The HTTPS origins a download may be redirected to; none allows no redirect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_origins: Vec<String>,
}

/// What `module.activate` requires before anything is queued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationDescriptor {
    /// Module-level settings that must hold a valid value.
    #[serde(default)]
    pub requires_settings: Vec<String>,
    /// Resources that must be installed.
    #[serde(default)]
    pub requires_resources: Vec<String>,
    pub notes: String,
}

/// A worker task, reached through the generated `task.<id>` method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDescriptor {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// The request names one asset.
    #[serde(default)]
    pub asset: bool,
    /// The request names one provider profile.
    #[serde(default)]
    pub profile: bool,
    /// The module must be active.
    #[serde(default)]
    pub requires_active: bool,
    /// The capabilities the task may use; each needs its grant.
    #[serde(default)]
    pub uses: Vec<String>,
    /// Declared and validated exactly like an action's parameters.
    #[serde(default)]
    pub parameters: Vec<ParameterDescriptor>,
    /// The action a client may offer to apply the task's artifact with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<TaskApply>,
}

impl TaskDescriptor {
    pub fn parameter(&self, name: &str) -> Option<&ParameterDescriptor> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }
}

/// Apply names a declared action and its `artifact` parameter, which receives the task's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskApply {
    pub action: String,
    pub parameter: String,
}

/// Name an unknown or unimplemented kind before deserialization, which would otherwise report only
/// serde's generic unknown variant without the setting or capability it belongs to.
pub(crate) fn check_raw(descriptor: &Value) -> Result<(), Error> {
    let id = |item: &Value| {
        item.get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned()
    };
    for capability in descriptor
        .get("capabilities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(kind) = capability.get("kind").and_then(Value::as_str) else {
            return Err(Error::validation(format!(
                "capability {} declares no kind",
                id(capability)
            )));
        };
        if UNIMPLEMENTED_CAPABILITY_KINDS.contains(&kind) {
            return Err(Error::validation(format!(
                "capability kind {kind} is not implemented (capability {})",
                id(capability)
            )));
        }
        if !CAPABILITY_KINDS.contains(&kind) {
            return Err(Error::validation(format!(
                "capability {} declares unknown kind {kind}",
                id(capability)
            )));
        }
    }
    // A setting is read on its own first, so a malformed one — an unknown kind above all — is
    // named by the setting it belongs to rather than only by serde's position in the descriptor.
    let settings = descriptor.get("settings");
    let module_fields = settings.and_then(|settings| settings.get("fields"));
    let profile_fields = settings
        .and_then(|settings| settings.get("profiles"))
        .and_then(|profiles| profiles.get("fields"));
    for field in [module_fields, profile_fields]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
    {
        if let Err(error) = serde_json::from_value::<SettingDescriptor>(field.clone()) {
            let name = field
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(Error::validation(format!("setting {name}: {error}")));
        }
    }
    Ok(())
}

/// Every capability declaration of one module, checked against the rest of its descriptor. What is
/// referred to is checked before what refers to it: settings and resources, then the capabilities
/// over them, then activation and the tasks that use the capabilities.
pub(crate) fn validate(module: &ModuleDescriptor) -> Result<(), Error> {
    let id = &module.id;
    if let Some(settings) = &module.settings {
        validate_settings(id, settings)?;
    }
    let mut resources = HashSet::with_capacity(module.resources.len());
    for resource in &module.resources {
        validate_resource(resource, &mut resources)?;
    }
    let mut capabilities = HashSet::with_capacity(module.capabilities.len());
    for capability in &module.capabilities {
        validate_capability(module, capability, &mut capabilities)?;
    }
    if let Some(activation) = &module.activation {
        validate_activation(module, activation)?;
    }
    let mut tasks = HashSet::with_capacity(module.tasks.len());
    for task in &module.tasks {
        validate_task(module, task, &mut tasks)?;
    }
    Ok(())
}

fn validate_settings(module: &str, settings: &SettingsDescriptor) -> Result<(), Error> {
    if settings.schema == 0 {
        return Err(Error::validation(format!(
            "module {module} declares settings schema 0; a schema is at least 1"
        )));
    }
    validate_fields(module, "setting", &settings.fields)?;
    let Some(profiles) = &settings.profiles else {
        return Ok(());
    };
    if profiles.label.trim().is_empty() {
        return Err(Error::validation(format!(
            "module {module} declares an unlabelled profile block"
        )));
    }
    if !(1..=MAX_PROFILES).contains(&profiles.max) {
        return Err(Error::validation(format!(
            "module {module} declares profiles max {}; max is 1..={MAX_PROFILES}",
            profiles.max
        )));
    }
    // A profile names one of these, so a block without one could never hold a profile.
    if profiles.adapters.is_empty() {
        return Err(Error::validation(format!(
            "module {module} declares a profile block with no adapters"
        )));
    }
    let mut adapters = HashSet::with_capacity(profiles.adapters.len());
    for adapter in &profiles.adapters {
        validate_adapter(adapter, &mut adapters)?;
    }
    validate_fields(module, "profile setting", &profiles.fields)?;
    if !profiles.fields.iter().any(SettingDescriptor::is_endpoint) {
        return Err(Error::validation(format!(
            "module {module} declares profile adapters but no endpoint profile field"
        )));
    }
    // A bearer adapter sends one credential, so the profile must say unambiguously which it is.
    let secrets = profiles
        .fields
        .iter()
        .filter(|field| field.is_secret())
        .count();
    if let Some(adapter) = profiles
        .adapters
        .iter()
        .find(|adapter| adapter.auth == AdapterAuth::Bearer)
        && secrets != 1
    {
        return Err(Error::validation(format!(
            "adapter {} authenticates with bearer but the profile fields declare {secrets} secret fields; exactly one holds the credential",
            adapter.id
        )));
    }
    Ok(())
}

fn validate_fields(module: &str, what: &str, fields: &[SettingDescriptor]) -> Result<(), Error> {
    if fields.len() > MAX_SETTING_FIELDS {
        return Err(Error::validation(format!(
            "module {module} declares {} {what} fields; at most {MAX_SETTING_FIELDS}",
            fields.len()
        )));
    }
    let mut seen = HashSet::with_capacity(fields.len());
    for field in fields {
        let id = field.id();
        if !valid_name(id) {
            return Err(Error::validation(format!(
                "invalid {what} identity {id} of module {module}"
            )));
        }
        if !seen.insert(id) {
            return Err(Error::validation(format!(
                "duplicate {what} {id} of module {module}"
            )));
        }
        validate_field(field)?;
    }
    Ok(())
}

/// A setting takes the parameter kinds a settings store can hold and a settings view can edit: the
/// plain values, an endpoint and a secret. A colour, a path, a curve, an artifact or a settings set
/// belongs to an edit, not to a user-level preference, and is refused by name.
fn validate_field(field: &SettingDescriptor) -> Result<(), Error> {
    let id = field.id();
    if field.label.trim().is_empty() {
        return Err(Error::validation(format!("setting {id} has no label")));
    }
    match field.kind() {
        ParameterKind::Boolean
        | ParameterKind::Integer { .. }
        | ParameterKind::Number { .. }
        | ParameterKind::Enum { .. }
        | ParameterKind::String { .. }
        | ParameterKind::Endpoint { .. }
        | ParameterKind::Secret { .. } => {}
        kind => {
            return Err(Error::validation(format!(
                "setting {id} declares kind {}, which a setting does not take",
                kind.name()
            )));
        }
    }
    check_declaration(&field.parameter)
}

fn validate_adapter<'a>(
    adapter: &'a AdapterDescriptor,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    let id = &adapter.id;
    if !valid_name(id) {
        return Err(Error::validation(format!("invalid adapter identity {id}")));
    }
    if !seen.insert(id.as_str()) {
        return Err(Error::validation(format!("duplicate adapter {id}")));
    }
    if adapter.title.trim().is_empty() {
        return Err(Error::validation(format!("adapter {id} has no title")));
    }
    if adapter.data.is_empty() {
        return Err(Error::validation(format!(
            "adapter {id} declares no data class"
        )));
    }
    let mut classes = HashSet::with_capacity(adapter.data.len());
    if !adapter.data.iter().all(|class| classes.insert(*class)) {
        return Err(Error::validation(format!(
            "adapter {id} declares a data class twice"
        )));
    }
    for (name, bytes) in [
        ("max_request_bytes", adapter.max_request_bytes),
        ("max_response_bytes", adapter.max_response_bytes),
    ] {
        if !(1..=MAX_ADAPTER_BYTES).contains(&bytes) {
            return Err(Error::validation(format!(
                "adapter {id} declares {name} {bytes}; it is 1..={MAX_ADAPTER_BYTES}"
            )));
        }
    }
    if !(1..=MAX_ADAPTER_TIMEOUT_MS).contains(&adapter.timeout_ms) {
        return Err(Error::validation(format!(
            "adapter {id} declares timeout_ms {}; it is 1..={MAX_ADAPTER_TIMEOUT_MS}",
            adapter.timeout_ms
        )));
    }
    if adapter
        .retention
        .as_ref()
        .is_some_and(|retention| retention.trim().is_empty())
    {
        return Err(Error::validation(format!(
            "adapter {id} declares an empty retention note"
        )));
    }
    Ok(())
}

fn validate_capability<'a>(
    module: &ModuleDescriptor,
    capability: &'a CapabilityDescriptor,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    let id = &capability.id;
    if !valid_name(id) {
        return Err(Error::validation(format!(
            "invalid capability identity {id}"
        )));
    }
    if !seen.insert(id.as_str()) {
        return Err(Error::validation(format!("duplicate capability {id}")));
    }
    if capability.purpose.trim().is_empty() {
        return Err(Error::validation(format!(
            "capability {id} declares no purpose"
        )));
    }
    let settings = module.settings.as_ref();
    match &capability.kind {
        CapabilityKind::RemoteImageRequest { adapter, data } => {
            let declared = settings
                .and_then(|settings| settings.profiles.as_ref())
                .and_then(|profiles| profiles.adapter(adapter))
                .ok_or_else(|| {
                    Error::validation(format!(
                        "capability {id} names undeclared adapter {adapter}"
                    ))
                })?;
            if !declared.data.contains(data) {
                return Err(Error::validation(format!(
                    "capability {id} sends {} but adapter {adapter} does not declare it",
                    data.name()
                )));
            }
        }
        CapabilityKind::DownloadArtifact { resource } => {
            if module.resource(resource).is_none() {
                return Err(Error::validation(format!(
                    "capability {id} names undeclared resource {resource}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_resource<'a>(
    resource: &'a ResourceDescriptor,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    let id = &resource.id;
    if !valid_name(id) {
        return Err(Error::validation(format!("invalid resource identity {id}")));
    }
    if !seen.insert(id.as_str()) {
        return Err(Error::validation(format!("duplicate resource {id}")));
    }
    for (name, value) in [
        ("title", &resource.title),
        ("format", &resource.format),
        ("license", &resource.license),
        ("provenance", &resource.provenance),
    ] {
        if value.trim().is_empty() {
            return Err(Error::validation(format!(
                "resource {id} declares no {name}"
            )));
        }
    }
    let version = &resource.version;
    let plain = !version.is_empty()
        && version.len() <= MAX_VERSION_LENGTH
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !version.starts_with('.');
    if !plain {
        return Err(Error::validation(format!(
            "resource {id} declares version {version:?}; a version is 1..={MAX_VERSION_LENGTH} ASCII letters, digits, '.', '-' or '_' and does not start with '.'"
        )));
    }
    parse_endpoint(
        &resource.url,
        &[EndpointClass::Remote, EndpointClass::Loopback],
    )
    .map_err(|error| {
        Error::validation(format!(
            "resource {id} declares url {}: {}",
            resource.url, error.detail
        ))
    })?;
    let hex = resource.sha256.len() == 64
        && resource
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !hex {
        return Err(Error::validation(format!(
            "resource {id} declares sha256 {:?}; it is 64 lowercase hexadecimal digits",
            resource.sha256
        )));
    }
    if !(1..=MAX_RESOURCE_BYTES).contains(&resource.bytes) {
        return Err(Error::validation(format!(
            "resource {id} declares bytes {}; it is 1..={MAX_RESOURCE_BYTES}",
            resource.bytes
        )));
    }
    for origin in &resource.redirect_origins {
        if !https_origin(origin) {
            return Err(Error::validation(format!(
                "resource {id} declares redirect origin {origin:?}; it must be an HTTPS origin written as https://host[:port]"
            )));
        }
    }
    Ok(())
}

/// `https://host[:port]` exactly as the origin serializes: no credentials, path, query or fragment,
/// and the default port left out, so a redirect is matched by string equality with the origin the
/// transport computes.
fn https_origin(text: &str) -> bool {
    Url::parse(text)
        .is_ok_and(|url| url.scheme() == "https" && url.origin().ascii_serialization() == text)
}

fn validate_activation(
    module: &ModuleDescriptor,
    activation: &ActivationDescriptor,
) -> Result<(), Error> {
    let mut settings = HashSet::with_capacity(activation.requires_settings.len());
    for setting in &activation.requires_settings {
        if module
            .settings
            .as_ref()
            .and_then(|settings| settings.field(setting))
            .is_none()
        {
            return Err(Error::validation(format!(
                "activation of module {} requires undeclared setting {setting}",
                module.id
            )));
        }
        if !settings.insert(setting.as_str()) {
            return Err(Error::validation(format!(
                "activation of module {} requires setting {setting} twice",
                module.id
            )));
        }
    }
    let mut resources = HashSet::with_capacity(activation.requires_resources.len());
    for resource in &activation.requires_resources {
        if module.resource(resource).is_none() {
            return Err(Error::validation(format!(
                "activation of module {} requires undeclared resource {resource}",
                module.id
            )));
        }
        if !resources.insert(resource.as_str()) {
            return Err(Error::validation(format!(
                "activation of module {} requires resource {resource} twice",
                module.id
            )));
        }
    }
    Ok(())
}

fn validate_task<'a>(
    module: &ModuleDescriptor,
    task: &'a TaskDescriptor,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    let id = &task.id;
    if !valid_name(id) {
        return Err(Error::validation(format!("invalid task identity {id}")));
    }
    if !seen.insert(id.as_str()) {
        return Err(Error::validation(format!("duplicate task {id}")));
    }
    if task.title.trim().is_empty() {
        return Err(Error::validation(format!("task {id} has no title")));
    }
    check_parameter_declarations("task", id, &task.parameters)?;
    let mut uses = HashSet::with_capacity(task.uses.len());
    for used in &task.uses {
        let capability = module.capability(used).ok_or_else(|| {
            Error::validation(format!("task {id} uses undeclared capability {used}"))
        })?;
        if !uses.insert(used.as_str()) {
            return Err(Error::validation(format!(
                "task {id} uses capability {used} twice"
            )));
        }
        // A remote request sends one asset's data to one profile's endpoint, and its grant is
        // scoped to both, so a task that could name neither could never be granted.
        if matches!(capability.kind, CapabilityKind::RemoteImageRequest { .. })
            && !(task.asset && task.profile)
        {
            return Err(Error::validation(format!(
                "task {id} uses remote-image-request capability {used} without declaring asset and profile"
            )));
        }
    }
    if task.profile
        && module
            .settings
            .as_ref()
            .and_then(|settings| settings.profiles.as_ref())
            .is_none()
    {
        return Err(Error::validation(format!(
            "task {id} takes a profile but module {} declares no profiles",
            module.id
        )));
    }
    if task.requires_active && module.activation.is_none() {
        return Err(Error::validation(format!(
            "task {id} requires activation but module {} declares none",
            module.id
        )));
    }
    if let Some(TaskApply { action, parameter }) = &task.apply {
        let declared = module.action(action).ok_or_else(|| {
            Error::validation(format!("task {id} applies with undeclared action {action}"))
        })?;
        let declared = declared.parameter(parameter).ok_or_else(|| {
            Error::validation(format!(
                "task {id} applies through undeclared parameter {parameter} of action {action}"
            ))
        })?;
        if !matches!(declared.kind, ParameterKind::Artifact) {
            return Err(Error::validation(format!(
                "task {id} applies through parameter {parameter} of action {action}, which is not an artifact parameter"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Control, ModuleRegistry,
        capabilities::testing::{ADAPTER, MODULE, TASK, capability_descriptor, setting},
        modules::TestModule,
    };
    use serde_json::json;

    fn settings(descriptor: &mut ModuleDescriptor) -> &mut SettingsDescriptor {
        descriptor.settings.as_mut().unwrap()
    }

    fn profiles(descriptor: &mut ModuleDescriptor) -> &mut ProfilesDescriptor {
        settings(descriptor).profiles.as_mut().unwrap()
    }

    fn field<'a>(descriptor: &'a mut ModuleDescriptor, id: &str) -> &'a mut SettingDescriptor {
        settings(descriptor)
            .fields
            .iter_mut()
            .find(|field| field.id() == id)
            .unwrap()
    }

    fn rejection(descriptor: &ModuleDescriptor) -> String {
        let error = descriptor
            .validate()
            .expect_err("the descriptor was expected to be refused");
        assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
        error.detail
    }

    fn parse_rejection(value: Value) -> String {
        let error =
            ModuleDescriptor::parse(&value).expect_err("the JSON was expected to be refused");
        assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
        error.detail
    }

    #[test]
    fn a_full_capability_descriptor_round_trips_through_parse_and_serde() {
        let descriptor = capability_descriptor();
        descriptor.validate().unwrap();
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(ModuleDescriptor::parse(&value).unwrap(), descriptor);
        // A setting is a parameter, serialized exactly as an action's parameter is, with its label
        // and activation flag beside it; a capability is flat the same way.
        let strength = &descriptor.settings.as_ref().unwrap().fields[0];
        let mut parameter = serde_json::to_value(&strength.parameter).unwrap();
        parameter["label"] = json!("strength");
        parameter["invalidates_activation"] = json!(false);
        assert_eq!(value["settings"]["fields"][0], parameter);
        assert_eq!(
            value["settings"]["fields"][0],
            json!({
                "name": "strength", "kind": "number", "min": 0.0, "max": 1.0,
                "required": false, "default": 0.5, "unit": null, "step": 0.01, "precision": 2,
                "notes": "", "label": "strength", "invalidates_activation": false,
            })
        );
        assert_eq!(
            value["settings"]["profiles"]["fields"][1],
            json!({
                "name": "api-key", "kind": "secret", "max_length": 128, "required": true,
                "default": null, "unit": null, "step": null, "precision": null, "notes": "",
                "label": "api key", "invalidates_activation": false,
            }),
            "a secret declares its presence and limit only"
        );
        assert_eq!(
            value["capabilities"][0],
            json!({
                "id": "echo", "kind": "remote-image-request", "adapter": ADAPTER,
                "data": "sample-grid-8", "purpose": "Ask the echo service for a tint.",
            })
        );
        assert_eq!(
            value["settings"]["profiles"]["adapters"][0]["auth"],
            json!("bearer")
        );
        assert_eq!(
            value["settings"]["profiles"]["adapters"][0]["cost"],
            json!("free")
        );
        assert_eq!(
            value["controls"][0],
            json!({"kind": "task", "task": TASK, "label": "Generate tint"})
        );
        assert_eq!(
            value["tasks"][0]["apply"],
            json!({"action": "apply-test-tint", "parameter": "tint"})
        );
        assert_eq!(
            value["activation"]["requires_resources"],
            json!(["palette"])
        );
        assert_eq!(
            value["resources"][0]["redirect_origins"],
            json!(["https://cdn.example.com"])
        );
        // Loopback resources, and HTTP to loopback, are accepted: the proof endpoint is local.
        let mut local = capability_descriptor();
        local.resources[0].url = "http://127.0.0.1:8080/palette.bin".into();
        local.validate().unwrap();
    }

    #[test]
    fn descriptors_without_capabilities_serialize_exactly_as_before() {
        for descriptor in ModuleRegistry::builtin().descriptors() {
            let value = serde_json::to_value(descriptor).unwrap();
            for key in [
                "settings",
                "capabilities",
                "resources",
                "activation",
                "tasks",
            ] {
                assert!(
                    value.get(key).is_none(),
                    "{} serializes an empty {key}",
                    descriptor.id
                );
            }
            assert_eq!(&ModuleDescriptor::parse(&value).unwrap(), descriptor);
        }
    }

    #[test]
    fn every_named_rejection_names_its_field() {
        type Change = fn(&mut ModuleDescriptor);
        let cases: Vec<(Change, &str)> = vec![
            // Identities: malformed and duplicate, in every namespace.
            (
                |d| field(d, "strength").parameter.name = "Strength".into(),
                "invalid setting identity Strength of module test.capabilities",
            ),
            (
                |d| {
                    settings(d)
                        .fields
                        .push(setting(ParameterDescriptor::boolean("strength")))
                },
                "duplicate setting strength of module test.capabilities",
            ),
            (
                |d| profiles(d).fields[2].parameter.name = "model_name".into(),
                "invalid profile setting identity model_name",
            ),
            (
                |d| {
                    let copy = profiles(d).fields[2].clone();
                    profiles(d).fields.push(copy);
                },
                "duplicate profile setting model of module test.capabilities",
            ),
            (
                |d| profiles(d).adapters[0].id = "Echo".into(),
                "invalid adapter identity Echo",
            ),
            (
                |d| {
                    let copy = profiles(d).adapters[0].clone();
                    profiles(d).adapters.push(copy);
                },
                "duplicate adapter echo-adapter",
            ),
            (
                |d| d.capabilities[0].id = "in put".into(),
                "invalid capability identity in put",
            ),
            (
                |d| d.capabilities[1].id = "echo".into(),
                "duplicate capability echo",
            ),
            (
                |d| d.resources[0].id = "palette.v1".into(),
                "invalid resource identity palette.v1",
            ),
            (
                |d| {
                    let copy = d.resources[0].clone();
                    d.resources.push(copy);
                },
                "duplicate resource palette",
            ),
            (
                |d| d.tasks[0].id = "Generate".into(),
                "invalid task identity Generate",
            ),
            (
                |d| {
                    let copy = d.tasks[0].clone();
                    d.tasks.push(copy);
                },
                "duplicate task generate-test-tint",
            ),
            // Defaults outside their kind, and kinds that never take one: the parameter
            // vocabulary's own checks, naming the setting's parameter.
            (
                |d| field(d, "strength").parameter.default = Some(json!(1.5)),
                "parameter strength must be a number within 0..=1",
            ),
            (
                |d| field(d, "mode").parameter.default = Some(json!("slow")),
                "parameter mode must be one of fast, exact",
            ),
            (
                |d| field(d, "count").parameter.default = Some(json!(0)),
                "parameter count must be an integer within 1..=8",
            ),
            (
                |d| field(d, "enabled").parameter.default = Some(json!("yes")),
                "parameter enabled must be a boolean",
            ),
            (
                |d| field(d, "note").parameter.default = Some(json!("x".repeat(17))),
                "parameter note must be at most 16 characters",
            ),
            (
                |d| field(d, "token").parameter.default = Some(json!("abc")),
                "secret parameter token declares a default",
            ),
            (
                |d| field(d, "local-service").parameter.default = Some(json!("http://127.0.0.1/")),
                "endpoint parameter local-service declares a default",
            ),
            // Kinds whose own declaration is unsound.
            (
                |d| field(d, "count").parameter.kind = ParameterKind::Integer { min: 5, max: 1 },
                "parameter count declares an empty range 5..=1",
            ),
            (
                |d| {
                    field(d, "strength").parameter.kind = ParameterKind::Number {
                        min: 0.0,
                        max: f64::INFINITY,
                    }
                },
                "parameter strength declares an empty range",
            ),
            (
                |d| field(d, "strength").parameter.step = Some(0.0),
                "parameter strength declares a step that is not finite and positive",
            ),
            (
                |d| field(d, "strength").parameter.precision = Some(7),
                "parameter strength declares a precision above 6",
            ),
            (
                |d| {
                    field(d, "mode").parameter.kind = ParameterKind::Enum {
                        options: Vec::new(),
                    }
                },
                "parameter mode declares no options",
            ),
            (
                |d| {
                    field(d, "mode").parameter.kind = ParameterKind::Enum {
                        options: vec!["fast".into(), "fast".into()],
                    }
                },
                "parameter mode declares an empty or duplicate option",
            ),
            (
                |d| field(d, "note").parameter.kind = ParameterKind::String { max_length: 257 },
                "parameter note declares a max_length 257 outside 1..=256",
            ),
            (
                |d| field(d, "token").parameter.kind = ParameterKind::Secret { max_length: 0 },
                "parameter token declares a max_length 0 outside 1..=4096",
            ),
            (
                |d| {
                    field(d, "local-service").parameter.kind = ParameterKind::Endpoint {
                        classes: Vec::new(),
                    }
                },
                "endpoint parameter local-service declares no class",
            ),
            (
                |d| {
                    field(d, "local-service").parameter.kind = ParameterKind::Endpoint {
                        classes: vec![EndpointClass::Loopback, EndpointClass::Loopback],
                    }
                },
                "endpoint parameter local-service declares a class twice",
            ),
            // A setting takes the kinds a settings store holds and a settings view edits.
            (
                |d| field(d, "note").parameter.kind = ParameterKind::Color,
                "setting note declares kind color, which a setting does not take",
            ),
            (
                |d| field(d, "note").parameter.kind = ParameterKind::Artifact,
                "setting note declares kind artifact, which a setting does not take",
            ),
            // Only a setting declares an endpoint or a secret: an action or task parameter of either
            // kind would put a destination or a credential in a request, a recipe or a job.
            (
                |d| {
                    d.actions[0]
                        .parameters
                        .push(ParameterDescriptor::endpoint("to", [EndpointClass::Remote]))
                },
                "parameter to of action apply-test-tint declares kind endpoint, which only a module setting declares",
            ),
            (
                |d| {
                    d.tasks[0]
                        .parameters
                        .push(ParameterDescriptor::secret("key", 16))
                },
                "parameter key of task generate-test-tint declares kind secret, which only a module setting declares",
            ),
            // An identity names one of the host's own objects: neither a setting nor a module's
            // action takes one.
            (
                |d| {
                    field(d, "note").parameter.kind = ParameterKind::Identity {
                        of: crate::IdentityKind::Mask,
                    }
                },
                "setting note declares kind identity, which a setting does not take",
            ),
            (
                |d| {
                    d.actions[0].parameters.push(ParameterDescriptor::identity(
                        "target",
                        crate::IdentityKind::Mask,
                    ))
                },
                "parameter target of action apply-test-tint declares kind identity, which only a host command declares",
            ),
            (
                |d| field(d, "note").label = " ".into(),
                "setting note has no label",
            ),
            (
                |d| settings(d).schema = 0,
                "module test.capabilities declares settings schema 0",
            ),
            // Bounds on the number of fields and profiles.
            (
                |d| {
                    settings(d).fields.extend((0..25).map(|index| {
                        setting(ParameterDescriptor::boolean(format!("extra-{index}")))
                    }))
                },
                "module test.capabilities declares 33 setting fields; at most 32",
            ),
            (
                |d| {
                    profiles(d).fields.extend((0..30).map(|index| {
                        setting(ParameterDescriptor::boolean(format!("extra-{index}")))
                    }))
                },
                "module test.capabilities declares 33 profile setting fields; at most 32",
            ),
            (
                |d| profiles(d).max = 0,
                "declares profiles max 0; max is 1..=16",
            ),
            (
                |d| profiles(d).max = 17,
                "declares profiles max 17; max is 1..=16",
            ),
            // Profiles and adapters.
            (
                |d| profiles(d).fields.retain(|field| field.id() != "endpoint"),
                "module test.capabilities declares profile adapters but no endpoint profile field",
            ),
            (
                |d| profiles(d).adapters.clear(),
                "module test.capabilities declares a profile block with no adapters",
            ),
            (
                |d| profiles(d).label = String::new(),
                "module test.capabilities declares an unlabelled profile block",
            ),
            (
                |d| profiles(d).fields.retain(|field| field.id() != "api-key"),
                "adapter echo-adapter authenticates with bearer but the profile fields declare 0 secret fields",
            ),
            (
                |d| profiles(d).adapters[0].data.clear(),
                "adapter echo-adapter declares no data class",
            ),
            (
                |d| profiles(d).adapters[0].max_response_bytes = 0,
                "adapter echo-adapter declares max_response_bytes 0",
            ),
            (
                |d| profiles(d).adapters[0].max_request_bytes = MAX_ADAPTER_BYTES + 1,
                "adapter echo-adapter declares max_request_bytes 268435457",
            ),
            (
                |d| profiles(d).adapters[0].timeout_ms = 0,
                "adapter echo-adapter declares timeout_ms 0",
            ),
            (
                |d| profiles(d).adapters[0].title = String::new(),
                "adapter echo-adapter has no title",
            ),
            (
                |d| profiles(d).adapters[0].retention = Some(" ".into()),
                "adapter echo-adapter declares an empty retention note",
            ),
            // Capabilities naming what the module does not declare.
            (
                |d| {
                    d.capabilities[0].kind = CapabilityKind::RemoteImageRequest {
                        adapter: "missing".into(),
                        data: DataClass::SampleGrid8,
                    }
                },
                "capability echo names undeclared adapter missing",
            ),
            (
                |d| {
                    d.capabilities[1].kind = CapabilityKind::DownloadArtifact {
                        resource: "missing".into(),
                    }
                },
                "capability palette names undeclared resource missing",
            ),
            (
                |d| d.capabilities[0].purpose = String::new(),
                "capability echo declares no purpose",
            ),
            // Resources.
            (
                |d| d.resources[0].url = "http://example.com/palette.bin".into(),
                "resource palette declares url http://example.com/palette.bin: a remote endpoint must use https",
            ),
            (
                |d| d.resources[0].url = "file:///etc/palette".into(),
                "resource palette declares url file:///etc/palette: scheme file is not allowed",
            ),
            (
                |d| d.resources[0].sha256 = "A".repeat(64),
                "resource palette declares sha256",
            ),
            (
                |d| d.resources[0].sha256 = "a".repeat(63),
                "it is 64 lowercase hexadecimal digits",
            ),
            (
                |d| d.resources[0].bytes = 0,
                "resource palette declares bytes 0; it is 1..=17179869184",
            ),
            (
                |d| d.resources[0].bytes = MAX_RESOURCE_BYTES + 1,
                "resource palette declares bytes 17179869185",
            ),
            (
                |d| d.resources[0].redirect_origins = vec!["http://cdn.example.com".into()],
                "resource palette declares redirect origin \"http://cdn.example.com\"",
            ),
            (
                |d| d.resources[0].redirect_origins = vec!["https://cdn.example.com/path".into()],
                "resource palette declares redirect origin \"https://cdn.example.com/path\"",
            ),
            (
                |d| d.resources[0].version = "../1".into(),
                "resource palette declares version \"../1\"",
            ),
            (
                |d| d.resources[0].license = String::new(),
                "resource palette declares no license",
            ),
            // Activation.
            (
                |d| d.activation.as_mut().unwrap().requires_settings = vec!["missing".into()],
                "activation of module test.capabilities requires undeclared setting missing",
            ),
            (
                |d| d.activation.as_mut().unwrap().requires_resources = vec!["missing".into()],
                "activation of module test.capabilities requires undeclared resource missing",
            ),
            // Tasks.
            (
                |d| d.tasks[0].uses.push("missing".into()),
                "task generate-test-tint uses undeclared capability missing",
            ),
            (
                |d| {
                    d.tasks[0].apply = Some(TaskApply {
                        action: "missing".into(),
                        parameter: "tint".into(),
                    })
                },
                "task generate-test-tint applies with undeclared action missing",
            ),
            (
                |d| {
                    d.tasks[0].apply = Some(TaskApply {
                        action: "apply-test-tint".into(),
                        parameter: "missing".into(),
                    })
                },
                "task generate-test-tint applies through undeclared parameter missing of action apply-test-tint",
            ),
            (
                |d| d.actions[0].parameters[0].kind = ParameterKind::Boolean,
                "task generate-test-tint applies through parameter tint of action apply-test-tint, which is not an artifact parameter",
            ),
            (
                |d| d.tasks[0].parameters[0].name = "Gain".into(),
                "invalid parameter name Gain of task generate-test-tint",
            ),
            (
                |d| d.tasks[0].title = String::new(),
                "task generate-test-tint has no title",
            ),
            (
                |d| d.tasks[0].profile = false,
                "task generate-test-tint uses remote-image-request capability echo without declaring asset and profile",
            ),
            (
                |d| d.activation = None,
                "task generate-test-tint requires activation but module test.capabilities declares none",
            ),
            (
                |d| {
                    d.tasks[0].uses.retain(|used| used != "echo");
                    d.capabilities.retain(|capability| capability.id != "echo");
                    settings(d).profiles = None;
                },
                "task generate-test-tint takes a profile but module test.capabilities declares no profiles",
            ),
            // A task control names a task this module declares.
            (
                |d| {
                    d.controls = vec![Control::Task {
                        task: "missing".into(),
                        label: "Run".into(),
                    }]
                },
                "task control of module test.capabilities names undeclared task missing",
            ),
            (
                |d| {
                    d.controls = vec![Control::Task {
                        task: TASK.into(),
                        label: String::new(),
                    }]
                },
                "task control for generate-test-tint of module test.capabilities has no label",
            ),
        ];
        for (change, expected) in cases {
            let mut descriptor = capability_descriptor();
            change(&mut descriptor);
            let detail = rejection(&descriptor);
            assert!(
                detail.contains(expected),
                "expected {expected:?}, got {detail:?}"
            );
            // A descriptor read from JSON is refused with the same message, whenever JSON can
            // express it: an infinite bound has no JSON form.
            let value = serde_json::to_value(&descriptor).unwrap();
            if serde_json::from_value::<ModuleDescriptor>(value.clone()).ok() == Some(descriptor) {
                assert_eq!(parse_rejection(value), detail);
            }
        }
    }

    #[test]
    fn managed_storage_and_local_runtime_are_refused_by_name() {
        for kind in ["managed-storage", "local-runtime"] {
            let mut value = serde_json::to_value(capability_descriptor()).unwrap();
            value["capabilities"][0] =
                json!({"id": "store", "kind": kind, "purpose": "Keep things."});
            let detail = parse_rejection(value);
            assert!(
                detail.contains(&format!("capability kind {kind} is not implemented")),
                "{detail}"
            );
            assert!(detail.contains("capability store"), "{detail}");
        }
    }

    #[test]
    fn unknown_setting_and_capability_kinds_are_named_when_parsed() {
        let mut value = serde_json::to_value(capability_descriptor()).unwrap();
        value["capabilities"][0]["kind"] = json!("open-socket");
        assert_eq!(
            parse_rejection(value),
            "capability echo declares unknown kind open-socket"
        );
        let mut value = serde_json::to_value(capability_descriptor()).unwrap();
        value["settings"]["fields"][1]["kind"] = json!("colour");
        let detail = parse_rejection(value);
        assert!(
            detail.starts_with("setting mode: unknown variant `colour`"),
            "{detail}"
        );
        let mut value = serde_json::to_value(capability_descriptor()).unwrap();
        value["settings"]["profiles"]["fields"][0]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        let detail = parse_rejection(value);
        assert!(
            detail.starts_with("setting endpoint: missing field `kind`"),
            "{detail}"
        );
        let mut value = serde_json::to_value(capability_descriptor()).unwrap();
        value["tasks"][0]["unexpected"] = json!(true);
        assert!(parse_rejection(value).contains("unknown field `unexpected`"));
    }

    #[test]
    fn task_identities_are_unique_across_the_registry() {
        let mut registry = ModuleRegistry::new();
        registry
            .register(TestModule::from_descriptor(capability_descriptor()))
            .unwrap();
        let (module, task) = registry.task(TASK).expect("the task is indexed");
        assert_eq!(module.descriptor().id, MODULE);
        assert_eq!(task.apply.as_ref().unwrap().action, "apply-test-tint");
        assert!(registry.task("missing").is_none());
        assert_eq!(registry.module(MODULE).unwrap().descriptor().id, MODULE);
        assert!(registry.module("test.missing").is_none());
        // A second module offering the same task would generate the same method.
        let mut other = capability_descriptor();
        other.id = "test.other".into();
        other.effects[0].id = "test.other.tint".into();
        for action in &mut other.actions {
            action.id = format!("other-{}", action.id);
        }
        other.tasks[0].apply = Some(TaskApply {
            action: "other-apply-test-tint".into(),
            parameter: "tint".into(),
        });
        let error = registry
            .register(TestModule::from_descriptor(other))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "task generate-test-tint of module test.other is already provided by test.capabilities"
        );
        assert!(
            registry.module("test.other").is_none(),
            "nothing was registered"
        );
    }
}
