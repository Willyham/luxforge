//! Plain-data capability declarations a module adds to its descriptor: typed settings and provider
//! profiles, the capabilities it may be granted, the resources it may install and the worker tasks
//! it offers. Registration validates them and does no I/O. See
//! `docs/design/module-capabilities.md#configuration-contract`.
//!
//! A setting is a [`ParameterDescriptor`] with a label, serialized flat exactly as a parameter is:
//! `{"name": "strength", "kind": "number", "min": 0, "max": 1, "required": false, …, "label":
//! "Strength"}`. A capability is serialized flat the same way.
//! Flattening rules out `deny_unknown_fields` on those two types, so an unknown field there is
//! ignored on read; every other capability type refuses unknown fields.
use super::endpoint::{EndpointClass, parse_endpoint};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error, ModuleDescriptor, ParameterDescriptor, ParameterKind,
    modules::{check_declaration, check_parameter_declarations},
    valid_name,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Url;

/// Module-level and profile fields are each at most this many, so a settings panel and a read
/// result stay bounded whatever a module declares.
pub(crate) const MAX_SETTING_FIELDS: usize = 32;
/// The most provider profiles one module may hold.
pub(crate) const MAX_PROFILES: u8 = 16;
/// The largest resource a module may pin, which is also the most a download may stream to disk.
pub(crate) const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The largest request body or response an adapter may declare. It is the artifact limit, since a
/// response becomes at most one artifact, and it bounds the buffer the transport holds.
pub(crate) const MAX_ADAPTER_BYTES: u64 = 256 * 1024 * 1024;
/// The longest whole-request deadline an adapter may declare.
pub(crate) const MAX_ADAPTER_TIMEOUT_MS: u64 = 10 * 60 * 1000;
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
/// incomplete. What only a setting has sits beside the parameter: the label a settings view shows.
/// A secret declares only its presence anywhere it is reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingDescriptor {
    #[serde(flatten)]
    pub parameter: ParameterDescriptor,
    pub label: String,
}

impl SettingDescriptor {
    pub(crate) fn new(parameter: ParameterDescriptor, label: impl Into<String>) -> Self {
        Self {
            parameter,
            label: label.into(),
        }
    }

    /// The setting's identity: its parameter's name.
    pub fn id(&self) -> &str {
        &self.parameter.name
    }

    pub fn kind(&self) -> &ParameterKind {
        &self.parameter.kind
    }

    pub(crate) fn is_secret(&self) -> bool {
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
    pub(crate) fn adapter(&self, id: &str) -> Option<&AdapterDescriptor> {
        self.adapters.iter().find(|adapter| adapter.id == id)
    }

    pub(crate) fn field(&self, id: &str) -> Option<&SettingDescriptor> {
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
pub(crate) const SAMPLE_GRID_SIDE: u32 = 8;

impl DataClass {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::SampleGrid8 => "sample-grid-8",
        }
    }

    /// What the body carries, in the words a consent notice uses.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::SampleGrid8 => "an 8 × 8 grid of rendered colour samples of this photo",
        }
    }

    /// The exact size of the body the host sends for this class, which the consent notice
    /// discloses.
    pub(crate) fn request_bytes(self) -> u64 {
        match self {
            Self::SampleGrid8 => super::data::SAMPLE_GRID_BYTES as u64,
        }
    }

    /// The media type of the body the host sends for this class.
    pub(crate) fn content_type(self) -> &'static str {
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

/// The implemented capabilities. `managed-storage` is not a kind until its first consumer defines
/// it, so a descriptor naming it is refused as an unknown variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CapabilityKind {
    /// Send one declared data class of one asset to a profile's endpoint through its adapter.
    RemoteImageRequest { adapter: String, data: DataClass },
    /// Install a declared resource from its pinned URL.
    DownloadArtifact { resource: String },
    /// Hold one declared resource loaded by the host's local inference runtime between tasks, in
    /// a host-owned slot that counts `resident_bytes` against the host's resident budget: opened
    /// as a `model` on the first of `backends` the runtime offers, at `precision`. Loaded by
    /// `module.runtime.load` or by the first task that uses it, never by a listing, a catalog open
    /// or a launch; unloaded by `module.runtime.unload`, after an idle interval, on a memory signal
    /// and when the resource is removed. Needs no grant: nothing leaves the machine.
    LocalRuntime {
        resource: String,
        model: ModelKind,
        resident_bytes: u64,
        backends: Vec<String>,
        precision: crate::inference::Precision,
    },
}

/// What a `local-runtime` capability opens its resource as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelKind {
    /// A tensor session ([`crate::inference::Session`]).
    Session,
    /// A generative model ([`crate::inference::Generator`]).
    Generator,
}

impl ModelKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Generator => "generator",
        }
    }
}

/// The most a `local-runtime` capability may declare one model holds resident: 64 GiB.
pub(crate) const MAX_RESIDENT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// The most backends a `local-runtime` capability may name.
pub(crate) const MAX_BACKENDS: usize = 8;

/// Which stage of the asset a task's host-planned read reads ([`TaskDescriptor::reads`]). The host
/// chooses the stage boundary from this and plans the read when the task is requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadBoundary {
    /// The stage the repair layer receives: after source development and before colour, in upright
    /// content coordinates, which a fill reads its crop from. Its input identity is a new top
    /// correction's.
    RepairInput,
}

impl ReadBoundary {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::RepairInput => "repair-input",
        }
    }
}

/// Pinned files a module may install together: exact bytes, hash and origin. The resource's own
/// file is `url`, `bytes` and `sha256`, installed under the resource's identity; `files` adds the
/// further files of a model that is several (an encoder and a decoder, a transformer and its
/// autoencoder), each installed beside it under its own name. They install together or not at
/// all, are removed together, and come from one origin, which the download grant names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The resource's further files, at most [`MAX_RESOURCE_FILES`] with its own, each pinned as
    /// its own file is and served from the same origin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ResourceFile>,
    /// The plain trade-off a person chooses this model by, such as "Finds smaller objects; slower
    /// to start and uses more memory": one line, never a technical name alone. Every model offered
    /// as a choice beside another carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tradeoff: Option<String>,
    /// One plain line, present only when the licence restricts how the person may use their own
    /// results, such as "Non-commercial use only". The resource card and the download disclosure
    /// show it; nothing is gated on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_restriction: Option<String>,
    /// The least memory a machine must have to install the resource. For a model a `local-runtime`
    /// capability holds, it counts the model resident, the candidates held across jobs and the
    /// analysis cache together, so it is at least their sum ([`machine_floor`]). A machine with
    /// less sees the requirement and cannot install it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory_bytes: Option<u64>,
}

/// One further file of a resource, pinned as the resource's own file is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFile {
    /// A plain file name, installed beside the resource's own file.
    pub name: String,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One file of a resource as an install fetches it: its own file, named after the resource, or a
/// further file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedFile<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub bytes: u64,
    pub sha256: &'a str,
}

impl ResourceDescriptor {
    /// Every file of the resource, its own first.
    pub fn pinned_files(&self) -> impl Iterator<Item = PinnedFile<'_>> {
        std::iter::once(PinnedFile {
            name: &self.id,
            url: &self.url,
            bytes: self.bytes,
            sha256: &self.sha256,
        })
        .chain(self.files.iter().map(|file| PinnedFile {
            name: &file.name,
            url: &file.url,
            bytes: file.bytes,
            sha256: &file.sha256,
        }))
    }

    /// The bytes of every file together: what an install downloads and the quota counts.
    pub fn total_bytes(&self) -> u64 {
        self.pinned_files()
            .fold(0u64, |total, file| total.saturating_add(file.bytes))
    }
}

/// A resource holds at most this many files, its own included.
pub(crate) const MAX_RESOURCE_FILES: usize = 8;
/// The longest trade-off label and use restriction, in characters: one line of a card.
pub(crate) const MAX_TRADEOFF_CHARS: usize = 120;
pub(crate) const MAX_USE_RESTRICTION_CHARS: usize = 160;
/// The machine memory a resource may require: from 1 GiB to 4 TiB.
pub(crate) const MIN_MACHINE_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_MACHINE_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;
/// What the analysis cache holds beside the resident models, as the AI editing design records it,
/// which a model's machine minimum counts. The cache arrives with analysis; this is the recorded
/// default it will be measured against.
pub const ANALYSIS_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// The least machine memory a model held resident at `resident_bytes` may require: the model, the
/// candidates held across jobs and the analysis cache together.
pub fn machine_floor(resident_bytes: u64) -> u64 {
    resident_bytes
        .saturating_add(crate::artifacts::candidates::MAX_HELD_CANDIDATE_BYTES)
        .saturating_add(ANALYSIS_CACHE_BYTES)
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
    /// The capabilities the task may use: a remote request needs its grant, and a resource's
    /// download capability needs the resource installed, whose path the task then reads.
    #[serde(default)]
    pub uses: Vec<String>,
    /// Declared and validated exactly like an action's parameters.
    #[serde(default)]
    pub parameters: Vec<ParameterDescriptor>,
    /// The action a client may offer to apply the task's artifact with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<TaskApply>,
    /// The stage of its asset the task reads through its context's host-planned reference read
    /// ([`crate::ModuleContext`]): the host binds the asset's current entry when the task is
    /// requested, and the task reads bounded rectangles of that stage on its lane. Only a task that
    /// takes an asset may declare one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads: Option<ReadBoundary>,
}

/// Apply names a declared action and its `artifact` parameter, which receives the task's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskApply {
    pub action: String,
    pub parameter: String,
}

/// Every capability declaration of one module, checked against the rest of its descriptor. What is
/// referred to is checked before what refers to it: settings and resources, then the capabilities
/// over them, then the tasks that use the capabilities.
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
    // The host sends only to a profile's endpoint, so a module-level one would do nothing.
    if let Some(field) = settings.fields.iter().find(|field| field.is_endpoint()) {
        return Err(Error::validation(format!(
            "setting {} of module {module} is an endpoint; only a profile field declares one",
            field.id()
        )));
    }
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
        CapabilityKind::LocalRuntime {
            resource,
            resident_bytes,
            backends,
            ..
        } => validate_local_runtime(module, id, resource, *resident_bytes, backends)?,
    }
    Ok(())
}

/// A `local-runtime` capability holds one declared resource that a `download-artifact` capability
/// can install, held by no other `local-runtime` capability (`module.runtime.load` names the
/// resource), with a resident budget of 1 byte to [`MAX_RESIDENT_BYTES`] and 1 to [`MAX_BACKENDS`]
/// distinct backend names.
fn validate_local_runtime(
    module: &ModuleDescriptor,
    id: &str,
    resource: &str,
    resident_bytes: u64,
    backends: &[String],
) -> Result<(), Error> {
    if module.resource(resource).is_none() {
        return Err(Error::validation(format!(
            "capability {id} names undeclared resource {resource}"
        )));
    }
    if !module.capabilities.iter().any(|capability| {
        matches!(&capability.kind, CapabilityKind::DownloadArtifact { resource: held } if held == resource)
    }) {
        return Err(Error::validation(format!(
            "local-runtime capability {id} holds resource {resource}, which no download-artifact capability installs"
        )));
    }
    if let Some(other) = module.capabilities.iter().find(|capability| {
        capability.id != id
            && matches!(&capability.kind, CapabilityKind::LocalRuntime { resource: held, .. } if held == resource)
    }) {
        return Err(Error::validation(format!(
            "local-runtime capabilities {id} and {} both hold resource {resource}",
            other.id
        )));
    }
    if !(1..=MAX_RESIDENT_BYTES).contains(&resident_bytes) {
        return Err(Error::validation(format!(
            "capability {id} declares resident_bytes {resident_bytes}; it is 1..={MAX_RESIDENT_BYTES}"
        )));
    }
    let floor = machine_floor(resident_bytes);
    if let Some(minimum) = module
        .resource(resource)
        .and_then(|declared| declared.min_memory_bytes)
        .filter(|minimum| *minimum < floor)
    {
        return Err(Error::validation(format!(
            "resource {resource} declares min_memory_bytes {minimum}; capability {id} holds it at {resident_bytes} resident bytes, so it is at least {floor}: the model, the held candidates and the analysis cache together"
        )));
    }
    if backends.is_empty() || backends.len() > MAX_BACKENDS {
        return Err(Error::validation(format!(
            "capability {id} declares {} backends; it is 1..={MAX_BACKENDS}",
            backends.len()
        )));
    }
    let mut seen = HashSet::with_capacity(backends.len());
    for backend in backends {
        if !valid_name(backend) {
            return Err(Error::validation(format!(
                "capability {id} declares invalid backend name {backend:?}"
            )));
        }
        if !seen.insert(backend.as_str()) {
            return Err(Error::validation(format!(
                "capability {id} declares backend {backend} twice"
            )));
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
    if !plain_name(version) {
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
    if !sha256_hex(&resource.sha256) {
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
    validate_files(resource)?;
    for (name, value, max) in [
        ("tradeoff", &resource.tradeoff, MAX_TRADEOFF_CHARS),
        (
            "use_restriction",
            &resource.use_restriction,
            MAX_USE_RESTRICTION_CHARS,
        ),
    ] {
        let Some(value) = value else { continue };
        if value.trim().is_empty() {
            return Err(Error::validation(format!(
                "resource {id} declares an empty {name}; one that says nothing is left out"
            )));
        }
        let characters = value.chars().count();
        if characters > max {
            return Err(Error::validation(format!(
                "resource {id} declares a {name} of {characters} characters; it is one line of at most {max}"
            )));
        }
        if value.chars().any(char::is_control) || value.trim() != value {
            return Err(Error::validation(format!(
                "resource {id} declares a {name} that is not one plain line"
            )));
        }
    }
    if let Some(minimum) = resource.min_memory_bytes
        && !(MIN_MACHINE_MEMORY_BYTES..=MAX_MACHINE_MEMORY_BYTES).contains(&minimum)
    {
        return Err(Error::validation(format!(
            "resource {id} declares min_memory_bytes {minimum}; it is {MIN_MACHINE_MEMORY_BYTES}..={MAX_MACHINE_MEMORY_BYTES}"
        )));
    }
    Ok(())
}

/// A resource's further files: at most [`MAX_RESOURCE_FILES`] with its own, each a plain name that
/// is not the resource's own file, the install record's or another's, each pinned as the resource
/// is, all from the origin of the resource's own URL, and at most [`MAX_RESOURCE_BYTES`] together.
fn validate_files(resource: &ResourceDescriptor) -> Result<(), Error> {
    let id = &resource.id;
    if resource.files.len() + 1 > MAX_RESOURCE_FILES {
        return Err(Error::validation(format!(
            "resource {id} declares {} files; a resource has at most {MAX_RESOURCE_FILES}, its own included",
            resource.files.len() + 1
        )));
    }
    let origin = |url: &str| {
        parse_endpoint(url, &[EndpointClass::Remote, EndpointClass::Loopback])
            .map(|endpoint| endpoint.origin())
    };
    let own_origin = origin(&resource.url)?;
    let mut names = HashSet::with_capacity(resource.files.len() + 1);
    names.insert(id.as_str());
    for file in &resource.files {
        let name = &file.name;
        if !plain_name(name) {
            return Err(Error::validation(format!(
                "resource {id} declares file name {name:?}; a file name is 1..={MAX_VERSION_LENGTH} ASCII letters, digits, '.', '-' or '_' and does not start with '.'"
            )));
        }
        if name == super::resources::INSTALLED_FILE {
            return Err(Error::validation(format!(
                "resource {id} declares file name {name}, which the install's own record takes"
            )));
        }
        if !names.insert(name.as_str()) {
            return Err(Error::validation(format!(
                "resource {id} declares file {name} twice; its own file is named {id}"
            )));
        }
        let served = origin(&file.url).map_err(|error| {
            Error::validation(format!(
                "resource {id} file {name} declares url {}: {}",
                file.url, error.detail
            ))
        })?;
        if served != own_origin {
            return Err(Error::validation(format!(
                "resource {id} file {name} is served from {served}; every file of a resource comes from {own_origin}, the origin its download grant names"
            )));
        }
        if !sha256_hex(&file.sha256) {
            return Err(Error::validation(format!(
                "resource {id} file {name} declares sha256 {:?}; it is 64 lowercase hexadecimal digits",
                file.sha256
            )));
        }
        if file.bytes == 0 {
            return Err(Error::validation(format!(
                "resource {id} file {name} declares bytes 0; a file holds at least one byte"
            )));
        }
    }
    let total = resource
        .pinned_files()
        .fold(0u128, |total, file| total + u128::from(file.bytes));
    if total > u128::from(MAX_RESOURCE_BYTES) {
        return Err(Error::validation(format!(
            "resource {id} declares {total} bytes across its files; a resource is at most {MAX_RESOURCE_BYTES}"
        )));
    }
    Ok(())
}

/// A plain directory or file name: 1..=[`MAX_VERSION_LENGTH`] ASCII letters, digits, '.', '-' or
/// '_', not starting with '.'.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_VERSION_LENGTH
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !name.starts_with('.')
}

fn sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `https://host[:port]` exactly as the origin serializes: no credentials, path, query or fragment,
/// and the default port left out, so a redirect is matched by string equality with the origin the
/// transport computes.
fn https_origin(text: &str) -> bool {
    Url::parse(text)
        .is_ok_and(|url| url.scheme() == "https" && url.origin().ascii_serialization() == text)
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
    if let Some(reads) = task.reads
        && !task.asset
    {
        return Err(Error::validation(format!(
            "task {id} reads the {} stage without declaring asset",
            reads.name()
        )));
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

    /// A further file of the capability descriptor's palette, served from its origin.
    fn file(name: &str) -> ResourceFile {
        ResourceFile {
            name: name.into(),
            url: format!("https://example.com/{name}"),
            bytes: 12,
            sha256: "b".repeat(64),
        }
    }

    #[test]
    fn a_model_resource_with_files_labels_and_a_machine_minimum_round_trips() {
        let mut descriptor = capability_descriptor();
        let resource = &mut descriptor.resources[0];
        resource.files = vec![file("decoder"), file("embedding.bin")];
        resource.tradeoff = Some("Finds smaller objects; slower to start".into());
        resource.use_restriction = Some("Non-commercial use only".into());
        resource.min_memory_bytes = Some(8 << 30);
        descriptor.validate().unwrap();
        let resource = &descriptor.resources[0];
        assert_eq!(resource.total_bytes(), 36);
        assert_eq!(
            resource
                .pinned_files()
                .map(|file| file.name)
                .collect::<Vec<_>>(),
            ["palette", "decoder", "embedding.bin"]
        );
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(
            value["resources"][0]["files"][1],
            json!({"name": "embedding.bin", "url": "https://example.com/embedding.bin",
                   "bytes": 12, "sha256": "b".repeat(64)})
        );
        assert_eq!(
            value["resources"][0]["tradeoff"],
            "Finds smaller objects; slower to start"
        );
        assert_eq!(
            value["resources"][0]["use_restriction"],
            "Non-commercial use only"
        );
        assert_eq!(value["resources"][0]["min_memory_bytes"], json!(8u64 << 30));
        assert_eq!(ModuleDescriptor::deserialize(&value).unwrap(), descriptor);
        // A resource without them serializes none of them.
        let plain = serde_json::to_value(capability_descriptor()).unwrap();
        for key in ["files", "tradeoff", "use_restriction", "min_memory_bytes"] {
            assert!(plain["resources"][0].get(key).is_none(), "{key}");
        }
        // An unknown resource field is refused rather than ignored.
        let mut unknown = value["resources"][0].clone();
        unknown["training_data"] = json!("a note");
        assert!(serde_json::from_value::<ResourceDescriptor>(unknown).is_err());
    }

    #[test]
    fn a_full_capability_descriptor_round_trips_through_json() {
        let descriptor = capability_descriptor();
        descriptor.validate().unwrap();
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(ModuleDescriptor::deserialize(&value).unwrap(), descriptor);
        // A setting is a parameter, serialized exactly as an action's parameter is, with its label
        // beside it; a capability is flat the same way.
        let strength = &descriptor.settings.as_ref().unwrap().fields[0];
        let mut parameter = serde_json::to_value(&strength.parameter).unwrap();
        parameter["label"] = json!("strength");
        assert_eq!(value["settings"]["fields"][0], parameter);
        assert_eq!(
            value["settings"]["fields"][0],
            json!({
                "name": "strength", "kind": "number", "min": 0.0, "max": 1.0,
                "required": false, "default": 0.5, "unit": null, "step": 0.01, "precision": 2,
                "notes": "", "label": "strength",
            })
        );
        assert_eq!(
            value["settings"]["profiles"]["fields"][1],
            json!({
                "name": "api-key", "kind": "secret", "max_length": 128, "required": true,
                "default": null, "unit": null, "step": null, "precision": null, "notes": "",
                "label": "api key",
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
            for key in ["settings", "capabilities", "resources", "tasks"] {
                assert!(
                    value.get(key).is_none(),
                    "{} serializes an empty {key}",
                    descriptor.id
                );
            }
            assert_eq!(&ModuleDescriptor::deserialize(&value).unwrap(), descriptor);
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
                |d| profiles(d).fields[0].parameter.default = Some(json!("http://127.0.0.1/")),
                "endpoint parameter endpoint declares a default",
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
                    profiles(d).fields[0].parameter.kind = ParameterKind::Endpoint {
                        classes: Vec::new(),
                    }
                },
                "endpoint parameter endpoint declares no class",
            ),
            (
                |d| {
                    profiles(d).fields[0].parameter.kind = ParameterKind::Endpoint {
                        classes: vec![EndpointClass::Loopback, EndpointClass::Loopback],
                    }
                },
                "endpoint parameter endpoint declares a class twice",
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
            // The host sends only to a profile's endpoint, so a module-level one is refused.
            (
                |d| {
                    settings(d)
                        .fields
                        .push(setting(ParameterDescriptor::endpoint(
                            "local-service",
                            [EndpointClass::Loopback],
                        )))
                },
                "setting local-service of module test.capabilities is an endpoint; only a profile field declares one",
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
                    settings(d).fields.extend((0..26).map(|index| {
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
            // A resource's further files, its labels and its machine minimum.
            (
                |d| d.resources[0].files = vec![file("decoder"); MAX_RESOURCE_FILES],
                "resource palette declares 9 files; a resource has at most 8, its own included",
            ),
            (
                |d| d.resources[0].files = vec![file("../decoder")],
                "resource palette declares file name \"../decoder\"",
            ),
            (
                |d| d.resources[0].files = vec![file("installed.json")],
                "resource palette declares file name installed.json, which the install's own record takes",
            ),
            (
                |d| d.resources[0].files = vec![file("palette")],
                "resource palette declares file palette twice; its own file is named palette",
            ),
            (
                |d| d.resources[0].files = vec![file("decoder"), file("decoder")],
                "resource palette declares file decoder twice",
            ),
            (
                |d| {
                    d.resources[0].files = vec![ResourceFile {
                        url: "https://cdn.example.com/decoder".into(),
                        ..file("decoder")
                    }]
                },
                "resource palette file decoder is served from https://cdn.example.com; every file of a resource comes from https://example.com",
            ),
            (
                |d| {
                    d.resources[0].files = vec![ResourceFile {
                        url: "http://example.com/decoder".into(),
                        ..file("decoder")
                    }]
                },
                "resource palette file decoder declares url http://example.com/decoder: a remote endpoint must use https",
            ),
            (
                |d| {
                    d.resources[0].files = vec![ResourceFile {
                        sha256: "B".repeat(64),
                        ..file("decoder")
                    }]
                },
                "resource palette file decoder declares sha256",
            ),
            (
                |d| {
                    d.resources[0].files = vec![ResourceFile {
                        bytes: 0,
                        ..file("decoder")
                    }]
                },
                "resource palette file decoder declares bytes 0",
            ),
            (
                |d| {
                    d.resources[0].files = vec![ResourceFile {
                        bytes: MAX_RESOURCE_BYTES,
                        ..file("decoder")
                    }]
                },
                "resource palette declares 17179869196 bytes across its files; a resource is at most 17179869184",
            ),
            (
                |d| d.resources[0].tradeoff = Some(" ".into()),
                "resource palette declares an empty tradeoff",
            ),
            (
                |d| d.resources[0].tradeoff = Some("x".repeat(MAX_TRADEOFF_CHARS + 1)),
                "resource palette declares a tradeoff of 121 characters; it is one line of at most 120",
            ),
            (
                |d| d.resources[0].tradeoff = Some("Faster\nbut rougher".into()),
                "resource palette declares a tradeoff that is not one plain line",
            ),
            (
                |d| d.resources[0].use_restriction = Some(String::new()),
                "resource palette declares an empty use_restriction",
            ),
            (
                |d| {
                    d.resources[0].use_restriction = Some("x".repeat(MAX_USE_RESTRICTION_CHARS + 1))
                },
                "resource palette declares a use_restriction of 161 characters",
            ),
            (
                |d| d.resources[0].use_restriction = Some(" Non-commercial use only".into()),
                "resource palette declares a use_restriction that is not one plain line",
            ),
            (
                |d| d.resources[0].min_memory_bytes = Some(MIN_MACHINE_MEMORY_BYTES - 1),
                "resource palette declares min_memory_bytes 1073741823; it is 1073741824..=4398046511104",
            ),
            (
                |d| d.resources[0].min_memory_bytes = Some(MAX_MACHINE_MEMORY_BYTES + 1),
                "resource palette declares min_memory_bytes 4398046511105",
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
                |d| {
                    d.tasks[0].uses.retain(|used| used != "echo");
                    d.capabilities.retain(|capability| capability.id != "echo");
                    settings(d).profiles = None;
                },
                "task generate-test-tint takes a profile but module test.capabilities declares no profiles",
            ),
            // A task control names a task this module declares.
            (
                |d| d.controls = vec![Control::task("missing", "Run").into()],
                "task control of module test.capabilities names undeclared task missing",
            ),
            (
                |d| d.controls = vec![Control::task(TASK, "").into()],
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
        }
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

    /// The capability descriptor with a `local-runtime` capability holding its palette.
    fn local_runtime() -> ModuleDescriptor {
        let mut descriptor = capability_descriptor();
        descriptor.capabilities.push(CapabilityDescriptor {
            id: "fill".into(),
            kind: CapabilityKind::LocalRuntime {
                resource: "palette".into(),
                model: ModelKind::Generator,
                resident_bytes: 1 << 20,
                backends: vec!["coreml".into(), "cpu".into()],
                precision: crate::inference::Precision::Fp16,
            },
            purpose: "Fill a selection.".into(),
        });
        descriptor
    }

    #[test]
    fn a_local_runtime_capability_round_trips_and_managed_storage_is_refused() {
        let descriptor = local_runtime();
        descriptor.validate().unwrap();
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(
            value["capabilities"][2],
            json!({
                "id": "fill", "kind": "local-runtime", "resource": "palette",
                "model": "generator", "resident_bytes": 1 << 20,
                "backends": ["coreml", "cpu"], "precision": "fp16",
                "purpose": "Fill a selection.",
            })
        );
        assert_eq!(ModuleDescriptor::deserialize(&value).unwrap(), descriptor);
        // A task that reads a stage says which; one that does not serializes no `reads`.
        assert!(value["tasks"][0].get("reads").is_none());
        let mut reading = capability_descriptor();
        reading.tasks[0].reads = Some(ReadBoundary::RepairInput);
        reading.validate().unwrap();
        let value = serde_json::to_value(&reading).unwrap();
        assert_eq!(value["tasks"][0]["reads"], json!("repair-input"));
        assert_eq!(ModuleDescriptor::deserialize(&value).unwrap(), reading);
        // managed-storage stays an unknown kind until its first consumer defines it.
        let refused = serde_json::from_value::<CapabilityDescriptor>(json!({
            "id": "store", "kind": "managed-storage", "purpose": "Keep files.",
        }))
        .unwrap_err();
        assert!(refused.to_string().contains("managed-storage"), "{refused}");
    }

    #[test]
    fn local_runtime_and_read_declarations_are_refused_by_name() {
        type Change = fn(&mut ModuleDescriptor);
        fn runtime(d: &mut ModuleDescriptor) -> &mut CapabilityKind {
            &mut d.capabilities[2].kind
        }
        let cases: Vec<(Change, &str)> = vec![
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { resource, .. } = runtime(d) {
                        *resource = "missing".into();
                    }
                },
                "capability fill names undeclared resource missing",
            ),
            (
                |d| {
                    d.capabilities.remove(1);
                },
                "local-runtime capability fill holds resource palette, which no download-artifact capability installs",
            ),
            (
                |d| {
                    let mut copy = d.capabilities[2].clone();
                    copy.id = "second".into();
                    d.capabilities.push(copy);
                },
                "local-runtime capabilities fill and second both hold resource palette",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { resident_bytes, .. } = runtime(d) {
                        *resident_bytes = 0;
                    }
                },
                "capability fill declares resident_bytes 0",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { resident_bytes, .. } = runtime(d) {
                        *resident_bytes = MAX_RESIDENT_BYTES + 1;
                    }
                },
                "capability fill declares resident_bytes 68719476737",
            ),
            (
                // 1 MiB resident, 1 GiB of held candidates and 256 MiB of analysis cache.
                |d| d.resources[0].min_memory_bytes = Some(MIN_MACHINE_MEMORY_BYTES),
                "resource palette declares min_memory_bytes 1073741824; capability fill holds it at 1048576 resident bytes, so it is at least 1343225856: the model, the held candidates and the analysis cache together",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { backends, .. } = runtime(d) {
                        backends.clear();
                    }
                },
                "capability fill declares 0 backends; it is 1..=8",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { backends, .. } = runtime(d) {
                        *backends = (0..=MAX_BACKENDS).map(|n| format!("b{n}")).collect();
                    }
                },
                "capability fill declares 9 backends; it is 1..=8",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { backends, .. } = runtime(d) {
                        backends[0] = "Core ML".into();
                    }
                },
                "capability fill declares invalid backend name \"Core ML\"",
            ),
            (
                |d| {
                    if let CapabilityKind::LocalRuntime { backends, .. } = runtime(d) {
                        backends[1] = "coreml".into();
                    }
                },
                "capability fill declares backend coreml twice",
            ),
            (
                |d| {
                    d.tasks[0].asset = false;
                    d.tasks[0].profile = false;
                    d.tasks[0].uses = vec!["fill".into()];
                    d.tasks[0].reads = Some(ReadBoundary::RepairInput);
                },
                "task generate-test-tint reads the repair-input stage without declaring asset",
            ),
        ];
        for (change, expected) in cases {
            let mut descriptor = local_runtime();
            change(&mut descriptor);
            let detail = rejection(&descriptor);
            assert!(
                detail.contains(expected),
                "expected {expected:?}, got {detail:?}"
            );
        }
        // A task may use a local-runtime capability without an asset or a profile.
        let mut local = local_runtime();
        local.tasks[0].uses = vec!["palette".into(), "fill".into()];
        local.tasks[0].asset = false;
        local.tasks[0].profile = false;
        local.validate().unwrap();
    }

    #[test]
    fn no_permission_gates_a_local_runtime_capability() {
        let descriptor = local_runtime();
        let error =
            crate::capabilities::grants::GrantKind::of(&descriptor.capabilities[2]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "capability fill is local-runtime, which no permission gates"
        );
    }
}
