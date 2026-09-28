//! The provider index: descriptors validated once at registration, then hash lookups by effect,
//! action, query and task identity. Registration touches no image, catalog, settings, secret,
//! network or resource file.
//!
//! Registration lives here; `lookups` resolves an identity to its provider, `placement` decides
//! where a committed layer joins a stack, and `compile` admits and compiles a stack against the
//! registered providers.
mod compile;
#[cfg(test)]
mod compile_tests;
mod lookups;
#[cfg(test)]
mod lookups_tests;
mod placement;
#[cfg(test)]
mod placement_tests;
#[cfg(test)]
pub(crate) mod tests;
mod variants;
#[cfg(test)]
mod variants_tests;

#[cfg(test)]
pub(crate) use compile::stack_compiles;
pub use lookups::{ActionRef, QueryRef};
pub use placement::insertion_index_among;
pub use variants::Superseded;

use super::{
    BasicModule, CanvasInteraction, CapabilitiesProofModule, CapabilityModule, ControlsModule,
    CropModule, MixerModule, ModuleDescriptor, PixelModule, PresenceModule, PresetsModule,
    Processing, RawModule, Stage, ToolModule, TransformModule, VignetteModule,
};
use crate::Error;
#[cfg(test)]
use crate::ErrorKind;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// The linked providers a run serves, in the order a registry lists them: presets first, because
/// the module owns no layer and its section leads the tools panel, then the pixel proof, RAW, Basic,
/// presence, the colour mixer, transforms, crop, the vignette and the controls proof. The two
/// proofs are test modules — their descriptors declare `developer` — so only a `developer` run gets
/// them. [`ModuleRegistry::assemble`] registers these; a test that builds a variant registry of its
/// own starts from them too. External loading is a later, separately measured step.
pub(crate) fn linked_modules(developer: bool) -> Vec<Arc<dyn ToolModule>> {
    let linked: [Arc<dyn ToolModule>; 10] = [
        Arc::new(PresetsModule::new()),
        Arc::new(PixelModule::new()),
        Arc::new(RawModule::new()),
        Arc::new(BasicModule::new()),
        Arc::new(PresenceModule::new()),
        Arc::new(MixerModule::new()),
        Arc::new(TransformModule::new()),
        Arc::new(CropModule::new()),
        Arc::new(VignetteModule::new()),
        Arc::new(ControlsModule::new()),
    ];
    linked
        .into_iter()
        .filter(|module| developer || !module.descriptor().developer)
        .collect()
}

/// The reason a module named by `--disable-module` reports.
const DISABLED_REASON: &str = "disabled by --disable-module";

/// What a run serves, as the desktop's and `luxforge-json`'s command lines name it: their
/// `--disable-module`, `--developer` and `--proof-endpoint` mean the same, because both assemble
/// their registry through [`ModuleRegistry::assemble`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RegistryOptions<'a> {
    /// Linked module identities to register unavailable, so a stack that uses one reports the
    /// unavailable effect instead of rendering without it. Naming a module the run does not serve
    /// is refused.
    pub disabled: &'a [String],
    /// A developer run also serves the test modules: every linked module whose descriptor declares
    /// `developer`, and the capability proof when a proof endpoint is named.
    pub developer: bool,
    /// The base URL of a capability proof endpoint a harness started, whose palette the capability
    /// proof installs. Only a developer run takes one.
    pub proof_endpoint: Option<&'a str>,
}

impl RegistryOptions<'_> {
    /// Refuse what no run serves: a proof endpoint outside developer mode, since the capability
    /// proof is a test fixture that never joins a photo-editing workspace. A binary checks this
    /// while reading its arguments, before it starts anything, and [`ModuleRegistry::assemble`]
    /// checks it again.
    pub fn check(&self) -> Result<(), String> {
        if self.proof_endpoint.is_some() && !self.developer {
            return Err("--proof-endpoint requires developer mode (--developer)".into());
        }
        Ok(())
    }
}

/// A provider registered unavailable: the module's own descriptor with its availability replaced,
/// and every other answer the module's own.
///
/// The host never plans, runs a query or task, activates or compiles through an unavailable
/// provider — `apply_action`, `run_query`, the capability host and every compile check
/// availability first — so its effects stay readable and a stack that holds one is reported rather
/// than rendered without it. Forwarding every call keeps that a property of the host's checks, not
/// of what this adapter happens to implement.
struct Unavailable {
    inner: Arc<dyn ToolModule>,
    descriptor: ModuleDescriptor,
}

impl ToolModule for Unavailable {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<super::ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(
        &self,
        input: &super::ActionInput,
        context: &super::StageContext<'_>,
    ) -> Result<super::ActionPlan, Error> {
        self.inner.plan(input, context)
    }
    fn validate_payload(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
    ) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
    ) -> Result<String, Error> {
        self.inner.describe_layer(effect_id, format, payload)
    }
    fn is_neutral(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
    ) -> Result<bool, Error> {
        self.inner.is_neutral(effect_id, format, payload)
    }
    fn label(&self, input: &super::ActionInput) -> Option<String> {
        self.inner.label(input)
    }
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
        self.inner.values(effect_id, format, payload)
    }
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
        self.inner.settings(effect_id, format, payload)
    }
    fn query(
        &self,
        query_id: &str,
        parameters: &serde_json::Map<String, serde_json::Value>,
        context: &super::StageContext<'_>,
    ) -> Result<serde_json::Value, Error> {
        self.inner.query(query_id, parameters, context)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        self.inner.compile(effect_id, format, payload, stage)
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        self.inner.capabilities()
    }
}

#[derive(Default)]
pub struct ModuleRegistry {
    modules: Vec<Arc<dyn ToolModule>>,
    module_ids: HashSet<String>,
    /// Effect identity to (module, effect) position.
    effects: HashMap<String, (usize, usize)>,
    /// Action identity to (module, action) position.
    actions: HashMap<String, (usize, usize)>,
    /// Query identity to (module, query) position. Queries have their own namespace: `query.<id>`
    /// and `edit.<id>` are different methods, so an id claimed here does not claim an action name.
    queries: HashMap<String, (usize, usize)>,
    /// Task identity to (module, task) position. A task generates the method `task.<id>`, so its
    /// identity is unique across the registry in a namespace of its own.
    tasks: HashMap<String, (usize, usize)>,
    /// Canvas mode shortcut to the module that claims it, so one letter selects one mode.
    shortcuts: HashMap<String, usize>,
}

impl std::fmt::Debug for ModuleRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleRegistry")
            .field(
                "modules",
                &self
                    .modules
                    .iter()
                    .map(|module| module.descriptor().id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry a run serves: the linked product modules; in a developer run the test modules
    /// too, and the capability proof when it names a proof endpoint; and each module `options`
    /// disables registered unavailable. The desktop and `luxforge-json` both assemble theirs here,
    /// so they serve and refuse the same things in the same words.
    pub fn assemble(options: &RegistryOptions<'_>) -> Result<Self, String> {
        options.check()?;
        let mut modules = linked_modules(options.developer);
        if options.developer
            && let Some(base) = options.proof_endpoint
        {
            modules.push(Arc::new(CapabilitiesProofModule::new(base)));
        }
        let mut registry = Self::new();
        let mut unknown: Vec<&str> = options.disabled.iter().map(String::as_str).collect();
        for module in modules {
            let id = module.descriptor().id.clone();
            let registered = if options.disabled.contains(&id) {
                unknown.retain(|named| *named != id);
                registry.register_unavailable(module, DISABLED_REASON)
            } else {
                registry.register(module)
            };
            registered.map_err(|error| error.to_string())?;
        }
        if let Some(id) = unknown.first() {
            return Err(format!("--disable-module names no registered module: {id}"));
        }
        registry
            .check_complete()
            .expect("built-in control variants are valid");
        // The host's own descriptors are published beside the modules' and never registered, so
        // they are held to the same rules here, by the host's form of the one validation.
        for host in registry.host_descriptors() {
            host.validate_host()
                .expect("the host's descriptors are valid");
        }
        Ok(registry)
    }

    /// The registry of an ordinary run: every product module, available, and no test module.
    pub fn builtin() -> Self {
        Self::assemble(&RegistryOptions::default()).expect("the product modules register")
    }

    /// The registry of a developer run without a proof endpoint: [`Self::builtin`] with the pixel
    /// and controls proofs, for tests that reach one.
    pub fn developer() -> Self {
        Self::assemble(&RegistryOptions {
            developer: true,
            ..RegistryOptions::default()
        })
        .expect("the product and test modules register")
    }

    /// Register `module` as unavailable, for `reason`: its descriptor, effects, actions, queries and
    /// tasks are registered and listed exactly as [`Self::register`] would, with its availability
    /// `unavailable {reason}`. A stack holding one of its effects stays readable and is reported
    /// rather than rendered without it, and its actions, queries and tasks are refused by name, as
    /// for any unavailable provider. [`RegistryOptions::disabled`] registers through it.
    pub fn register_unavailable(
        &mut self,
        module: Arc<dyn ToolModule>,
        reason: impl Into<String>,
    ) -> Result<(), Error> {
        let descriptor = ModuleDescriptor {
            availability: super::Availability::Unavailable {
                reason: reason.into(),
            },
            ..module.descriptor().clone()
        };
        self.register(Arc::new(Unavailable {
            inner: module,
            descriptor,
        }))
    }

    /// Validate a descriptor and index its effects and actions. Identities are unique across the
    /// whole registry, so discovery and dispatch can never resolve to two providers.
    pub fn register(&mut self, module: Arc<dyn ToolModule>) -> Result<(), Error> {
        let descriptor = module.descriptor();
        // A mask command lives in the host's own namespace, as `history.*` and `version.*` do, and
        // its identity carries a dot, which `valid_name` forbids inside an action identity. So the
        // two families cannot collide however either grows — and the rule is checked here rather
        // than assumed, before the shape check below, so the refusal names the real reason instead
        // of reporting a malformed identity.
        for declared in descriptor.actions.iter().chain(&descriptor.queries) {
            if crate::mask::commands::find(&declared.id).is_some()
                || crate::mask::commands::find_query(&declared.id).is_some()
            {
                return Err(Error::validation(format!(
                    "{} declares {}, which is a host mask command",
                    descriptor.id, declared.id
                )));
            }
        }
        descriptor.validate()?;
        if module.capabilities().is_none()
            && let Some(declared) = super::capability::needs_capabilities(descriptor)
        {
            return Err(Error::validation(format!(
                "{} declares {declared}, which needs the module's capability hooks, and it \
                 provides none",
                descriptor.id
            )));
        }
        if self.module_ids.contains(&descriptor.id) {
            return Err(Error::validation(format!(
                "duplicate module {}",
                descriptor.id
            )));
        }
        for effect in &descriptor.effects {
            if let Some((existing, _)) = self.effects.get(&effect.id) {
                return Err(Error::validation(format!(
                    "effect {} is already provided by {}",
                    effect.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for action in &descriptor.actions {
            if let Some((existing, _)) = self.actions.get(&action.id) {
                return Err(Error::validation(format!(
                    "action {} is already provided by {}",
                    action.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for query in &descriptor.queries {
            if let Some((existing, _)) = self.queries.get(&query.id) {
                return Err(Error::validation(format!(
                    "query {} is already provided by {}",
                    query.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for task in &descriptor.tasks {
            if let Some((existing, _)) = self.tasks.get(&task.id) {
                return Err(Error::validation(format!(
                    "task {} of module {} is already provided by {}",
                    task.id,
                    descriptor.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        let shortcut = descriptor
            .canvas
            .as_ref()
            .and_then(CanvasInteraction::shortcut);
        if let Some(letter) = shortcut
            && let Some(existing) = self.shortcuts.get(letter)
        {
            return Err(Error::validation(format!(
                "canvas shortcut {letter} is already claimed by {}",
                self.modules[*existing].descriptor().id
            )));
        }
        let index = self.modules.len();
        if let Some(letter) = shortcut {
            self.shortcuts.insert(letter.to_owned(), index);
        }
        self.module_ids.insert(descriptor.id.clone());
        for (position, effect) in descriptor.effects.iter().enumerate() {
            self.effects.insert(effect.id.clone(), (index, position));
        }
        for (position, action) in descriptor.actions.iter().enumerate() {
            self.actions.insert(action.id.clone(), (index, position));
        }
        for (position, query) in descriptor.queries.iter().enumerate() {
            self.queries.insert(query.id.clone(), (index, position));
        }
        for (position, task) in descriptor.tasks.iter().enumerate() {
            self.tasks.insert(task.id.clone(), (index, position));
        }
        self.modules.push(module);
        Ok(())
    }
}
