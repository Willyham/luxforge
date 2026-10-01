//! Resolving an identity to what provides it: a registered module's effect, action, query or task,
//! or one of the host's own `mask.*` actions and queries, and the declarations those carry. Every
//! lookup is a hash probe or a scan of a handful of modules; none reads a stack.
use super::{ModuleRegistry, Provider};
use crate::{
    Error,
    capabilities::descriptor::TaskDescriptor,
    modules::{
        ActionDescriptor, CapabilityModule, EffectDescriptor, EffectStage, ModuleDescriptor,
    },
};

/// One resolved action: a registered module's, or one the host declares for its own objects.
///
/// Both are declared with the same [`ActionDescriptor`], checked by the same generic parameter
/// check and committed, drafted and deduplicated through the editor's one action path; they differ
/// only in who plans them. A module plans a layer change against a lazy stage context, and the host
/// plans a `mask.*` command's change to the mask table ([`crate::mask::commands`]).
#[derive(Clone, Copy)]
pub enum ActionRef<'r> {
    Module(Provider<'r>, &'r ActionDescriptor),
    Host(&'static crate::mask::commands::MaskCommand),
}

impl<'r> ActionRef<'r> {
    /// The action's declaration: its identity, parameters and whether it is a patch.
    pub(crate) fn descriptor(&self) -> &'r ActionDescriptor {
        match self {
            Self::Module(_, action) => action,
            Self::Host(command) => &command.action,
        }
    }

    /// The API method this action is called through: `edit.<id>` for a module's, and a host
    /// action's own identity, which already carries its family's namespace (`mask.create-linear`).
    pub fn method(&self) -> String {
        match self {
            Self::Module(_, action) => format!("edit.{}", action.id),
            Self::Host(command) => command.method.to_owned(),
        }
    }
}

/// One resolved read-only query: a registered module's, answered through the one plan path, or one
/// the host answers about its own objects.
#[derive(Clone, Copy)]
pub enum QueryRef<'r> {
    Module(Provider<'r>, &'r ActionDescriptor),
    Host(&'static ActionDescriptor),
}

impl<'r> QueryRef<'r> {
    /// The query's declaration.
    pub(crate) fn descriptor(&self) -> &'r ActionDescriptor {
        match self {
            Self::Module(_, query) => query,
            Self::Host(query) => query,
        }
    }

    /// The API method this query is called through: `query.<id>` for a module's, and a host
    /// query's own identity (`mask.list`).
    pub fn method(&self) -> String {
        match self {
            Self::Module(_, query) => format!("query.{}", query.id),
            Self::Host(query) => query.id.clone(),
        }
    }
}

impl ModuleRegistry {
    /// Every registered module's descriptor, as the registry publishes it: with the availability
    /// each was registered with.
    pub fn descriptors(&self) -> Vec<&ModuleDescriptor> {
        self.entries
            .iter()
            .map(|entry| entry.provider().descriptor())
            .collect()
    }

    /// The registered module at `index`, which an identity index names.
    fn provider_at(&self, index: usize) -> Provider<'_> {
        self.entries[index].provider()
    }

    pub fn action(&self, id: &str) -> Option<(Provider<'_>, &ActionDescriptor)> {
        let (module, position) = self.actions.get(id)?;
        let provider = self.provider_at(*module);
        Some((provider, &provider.descriptor().actions[*position]))
    }

    /// The one answer to "is this a presettable action": registered, declared `patch: true` and
    /// provided by an available module. The preset library's validation and its `unavailable`
    /// list, capture, a composite plan's steps and the Lightroom mapping all resolve through it.
    /// It keys on the declaration, not on which module plans the action, so `set-raw` is
    /// presettable like every other field patch.
    ///
    /// It is independent of the photo: whether the module applies to a source kind, and which
    /// fields are superseded on it, are layered on by the callers that have one. An unknown action
    /// is `validation: unknown action <id>`, a declared one that is not a field patch
    /// `validation: <id> is not a field-patch action`, and one whose module is registered
    /// unavailable `incompatible: unavailable module <module>` — the one refusal of the three a
    /// composite plan defers until it knows the step applies to the photo. A hash probe; it
    /// allocates only the refusal.
    pub(crate) fn patch_action(
        &self,
        id: &str,
    ) -> Result<(Provider<'_>, &ActionDescriptor), Error> {
        let (module, action) = self
            .action(id)
            .ok_or_else(|| Error::validation(format!("unknown action {id}")))?;
        if !action.patch {
            return Err(Error::validation(format!(
                "{id} is not a field-patch action"
            )));
        }
        if !action.preset {
            return Err(Error::validation(format!("{id} is not presettable")));
        }
        module.descriptor().check_available()?;
        Ok((module, action))
    }

    /// The one action lookup every caller resolves an action through: a registered module's
    /// action, or one of the host's own `mask.*` commands, which the host descriptor declares
    /// (`crate::mask::commands::descriptor`). The two namespaces cannot collide — a host action
    /// identity carries a dot, which a module action's may not, and [`Self::register`] refuses a
    /// module that declares one anyway — so an identity names at most one of them.
    pub fn resolve_action(&self, id: &str) -> Option<ActionRef<'_>> {
        match self.action(id) {
            Some((module, action)) => Some(ActionRef::Module(module, action)),
            None => crate::mask::commands::find(id).map(ActionRef::Host),
        }
    }

    /// The action an API method calls, the inverse of [`ActionRef::method`]: `edit.<id>` names a
    /// module's action and a host action is named by its own identity.
    pub(crate) fn action_for_method(&self, method: &str) -> Option<ActionRef<'_>> {
        match method.strip_prefix("edit.") {
            Some(id) => self
                .action(id)
                .map(|(module, action)| ActionRef::Module(module, action)),
            None => crate::mask::commands::find(method).map(ActionRef::Host),
        }
    }

    /// The query an API method calls, the inverse of [`QueryRef::method`].
    pub(crate) fn query_for_method(&self, method: &str) -> Option<QueryRef<'_>> {
        match method.strip_prefix("query.") {
            Some(id) => self
                .query(id)
                .map(|(module, query)| QueryRef::Module(module, query)),
            None => crate::mask::commands::find_query(method).map(QueryRef::Host),
        }
    }

    /// The one query lookup, as [`Self::resolve_action`] is for actions: a registered module's
    /// query, or one of the host's own reads (`mask.list`, `mask.sample-input`).
    pub fn resolve_query(&self, id: &str) -> Option<QueryRef<'_>> {
        match self.query(id) {
            Some((module, query)) => Some(QueryRef::Module(module, query)),
            None => crate::mask::commands::find_query(id).map(QueryRef::Host),
        }
    }

    /// The descriptors the host publishes for its own objects beside the modules' — today the one
    /// for masks — in the shape a module's descriptor takes, so a client discovers a host action
    /// or query exactly as it discovers a module's.
    pub fn host_descriptors(&self) -> [&'static ModuleDescriptor; 1] {
        [crate::mask::commands::descriptor()]
    }

    /// The module that answers this read-only query, and the query's declared parameters. An
    /// unavailable provider keeps its identity here exactly as it does for actions and effects; the
    /// caller reports that rather than silently answering nothing.
    pub(crate) fn query(&self, id: &str) -> Option<(Provider<'_>, &ActionDescriptor)> {
        let (module, position) = self.queries.get(id)?;
        let provider = self.provider_at(*module);
        Some((provider, &provider.descriptor().queries[*position]))
    }

    /// The module that offers this worker task, and the task's declaration.
    pub(crate) fn task(&self, id: &str) -> Option<(Provider<'_>, &TaskDescriptor)> {
        let (module, position) = self.tasks.get(id)?;
        let provider = self.provider_at(*module);
        Some((provider, &provider.descriptor().tasks[*position]))
    }

    /// The registered module with this identity. A linear scan: a registry holds a handful of
    /// modules, and the capability methods that ask are not on a per-pixel path.
    pub fn module(&self, id: &str) -> Option<Provider<'_>> {
        self.entries
            .iter()
            .map(|entry| entry.provider())
            .find(|provider| provider.descriptor().id == id)
    }

    /// The capability hooks of the registered module with this identity, which the capability
    /// host calls to check its resources and run its tasks. Registration refused every
    /// module whose declarations need them and that provides none, so `None` means the module is
    /// not registered or declares nothing that needs them.
    pub(crate) fn capabilities(&self, id: &str) -> Option<&dyn CapabilityModule> {
        self.module(id)?.module().capabilities()
    }

    pub fn effect(&self, id: &str) -> Option<(Provider<'_>, &EffectDescriptor)> {
        let (module, position) = self.effects.get(id)?;
        let provider = self.provider_at(*module);
        Some((provider, &provider.descriptor().effects[*position]))
    }

    /// The stage an effect's payload addresses, or `None` when no provider declares it.
    pub(crate) fn effect_stage(&self, effect_id: &str) -> Option<EffectStage> {
        self.effect(effect_id).map(|(_, effect)| effect.stage)
    }

    /// Whether a layer of this effect may carry a mask, as the effect's own descriptor declares.
    pub(crate) fn effect_maskable(&self, effect_id: &str) -> bool {
        self.effect(effect_id)
            .is_some_and(|(_, effect)| effect.maskable)
    }

    /// Whether a stack holds at most one layer of this effect per target, as the effect's own
    /// descriptor declares.
    pub(crate) fn effect_single(&self, effect_id: &str) -> bool {
        self.effect(effect_id)
            .is_some_and(|(_, effect)| effect.single)
    }

    /// Whether this action carries the host's optional `mask` target field.
    ///
    /// The field belongs to the actions of a maskable effect, and an action is declared by a module
    /// rather than by an effect, so the module that owns the action is what answers: a module that
    /// declares a maskable effect accepts the field on its actions. Every delivered maskable module
    /// declares exactly one effect, so there is no case where this is wider than the design's
    /// sentence; a later module that declared both a maskable and a non-maskable effect would need
    /// an action-to-effect link that no descriptor carries today.
    pub(crate) fn action_accepts_mask(&self, action_id: &str) -> bool {
        let Some((module, _)) = self.actions.get(action_id) else {
            return false;
        };
        self.provider_at(*module)
            .descriptor()
            .effects
            .iter()
            .any(|effect| effect.maskable)
    }

    /// Whether this query carries the host's optional `mask` target field: the queries of a module
    /// that declares a maskable effect do, by the rule [`Self::action_accepts_mask`] states for its
    /// actions.
    pub(crate) fn query_accepts_mask(&self, query_id: &str) -> bool {
        let Some((module, _)) = self.queries.get(query_id) else {
            return false;
        };
        self.provider_at(*module)
            .descriptor()
            .effects
            .iter()
            .any(|effect| effect.maskable)
    }

    /// The provider that can evaluate this effect, or `None` when none is registered or the
    /// registered one reports itself unavailable.
    pub(super) fn provider(&self, effect_id: &str) -> Option<Provider<'_>> {
        let (provider, _) = self.effect(effect_id)?;
        provider.descriptor().is_available().then_some(provider)
    }

    /// Every registered module in registration order, with the availability it was registered
    /// with: the order a photograph's first-open actions are asked in.
    pub(crate) fn providers(&self) -> impl Iterator<Item = Provider<'_>> {
        self.entries.iter().map(|entry| entry.provider())
    }

    /// Block until every available module can answer [`crate::ToolModule::first_open`]. The
    /// source worker calls it before a photograph's first preparation completes, so the catalog
    /// owner never waits.
    pub(crate) fn await_first_open(&self) {
        for provider in self.providers() {
            if provider.descriptor().is_available() {
                provider.await_first_open();
            }
        }
    }
}
