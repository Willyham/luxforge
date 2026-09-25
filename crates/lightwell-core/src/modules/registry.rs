//! The provider index: descriptors validated once at registration, then hash lookups by effect,
//! action, query and task identity. Registration touches no image, catalog, settings, secret,
//! network or resource file.
use super::{
    ActionDescriptor, BasicModule, CanvasInteraction, CropModule, EffectDescriptor, EffectStage,
    MAX_COLOR_UNITS, MAX_MASKED_SPATIAL_LAYERS, MixerModule, ModuleDescriptor, PixelModule,
    PresenceModule, PresetsModule, Processing, RawModule, SPATIAL_TILE, Stage, ToolModule,
    TransformModule, VignetteModule,
};
use crate::{
    Error, ErrorKind, Layer, Mask, MaskId, ProxyApproximation, Recipe,
    artifacts::ArtifactTable,
    capabilities::descriptor::TaskDescriptor,
    mask_field::{MaskField, MaskSampling},
    render::{
        Compiled, Entry, Segment,
        spatial::{SpatialPlan, prefix_hash},
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// One target's position in the order masked layers of an effect take: the global layer is first,
/// and each mask follows at its own index in the recipe's mask list. A reference to a mask the table
/// does not hold sorts last rather than being treated as the global layer; such a stack is refused by
/// [`Recipe::validate`] before anything renders, and this keeps the refusal from depending on a
/// position guess.
fn target_rank(mask: Option<&MaskId>, masks: &[Mask]) -> usize {
    match mask {
        None => 0,
        Some(id) => masks
            .iter()
            .position(|mask| &mask.id == id)
            .map_or(usize::MAX, |index| index + 1),
    }
}

fn unavailable(effect_id: &str, layers: Vec<&str>) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!(
            "unavailable effect {effect_id} (layers {})",
            layers.join(", ")
        ),
    )
}

/// The linked built-in providers, in the order a registry lists them: presets first, because the
/// module owns no layer and its section leads the tools panel, then pixel, RAW, Basic, presence,
/// the colour mixer, transforms, crop and the vignette. [`ModuleRegistry::builtin`] registers
/// exactly these, and a client that serves a different set — the desktop's `--disable-module`,
/// its developer proofs — starts from this list rather than keeping its own. External loading is a
/// later, separately measured step.
pub fn builtin_modules() -> Vec<Arc<dyn ToolModule>> {
    vec![
        Arc::new(PresetsModule::new()),
        Arc::new(PixelModule::new()),
        Arc::new(RawModule::new()),
        Arc::new(BasicModule::new()),
        Arc::new(PresenceModule::new()),
        Arc::new(MixerModule::new()),
        Arc::new(TransformModule::new()),
        Arc::new(CropModule::new()),
        Arc::new(VignetteModule::new()),
    ]
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
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &serde_json::Value,
        stage: Stage,
        artifacts: &[Arc<crate::artifacts::PreparedArtifact>],
    ) -> Result<Processing, Error> {
        self.inner
            .compile_bound(effect_id, format, payload, stage, artifacts)
    }
    fn activate(&self, context: &crate::capabilities::context::ModuleContext) -> Result<(), Error> {
        self.inner.activate(context)
    }
    fn deactivate(&self) {
        self.inner.deactivate();
    }
    fn validate_resource(&self, resource_id: &str, path: &std::path::Path) -> Result<(), Error> {
        self.inner.validate_resource(resource_id, path)
    }
    fn run_task(
        &self,
        task_id: &str,
        parameters: &serde_json::Map<String, serde_json::Value>,
        context: &crate::capabilities::context::ModuleContext,
    ) -> Result<serde_json::Value, Error> {
        self.inner.run_task(task_id, parameters, context)
    }
}

/// One resolved action: a registered module's, or one the host declares for its own objects.
///
/// Both are declared with the same [`ActionDescriptor`], checked by the same generic parameter
/// check and committed, drafted and deduplicated through the editor's one action path; they differ
/// only in who plans them. A module plans a layer change against a lazy stage context, and the host
/// plans a `mask.*` command's change to the mask table ([`crate::mask::commands`]).
#[derive(Clone, Copy)]
pub enum ActionRef<'r> {
    Module(&'r dyn ToolModule, &'r ActionDescriptor),
    Host(&'static crate::mask::commands::MaskCommand),
}

impl<'r> ActionRef<'r> {
    /// The action's declaration: its identity, parameters and whether it is a patch.
    pub fn descriptor(&self) -> &'r ActionDescriptor {
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
    Module(&'r dyn ToolModule, &'r ActionDescriptor),
    Host(&'static ActionDescriptor),
}

impl<'r> QueryRef<'r> {
    /// The query's declaration.
    pub fn descriptor(&self) -> &'r ActionDescriptor {
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

    /// A registry of [`builtin_modules`], every one available.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for module in builtin_modules() {
            registry
                .register(module)
                .expect("built-in module descriptors are valid");
        }
        registry
    }

    /// Register `module` as unavailable, for `reason`: its descriptor, effects, actions, queries and
    /// tasks are registered and listed exactly as [`Self::register`] would, with its availability
    /// `unavailable {reason}`. A stack holding one of its effects stays readable and is reported
    /// rather than rendered without it, and its actions, queries and tasks are refused by name, as
    /// for any unavailable provider. The desktop's `--disable-module` is one.
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
                return Err(validation(format!(
                    "{} declares {}, which is a host mask command",
                    descriptor.id, declared.id
                )));
            }
        }
        descriptor.validate()?;
        if self.module_ids.contains(&descriptor.id) {
            return Err(validation(format!("duplicate module {}", descriptor.id)));
        }
        for effect in &descriptor.effects {
            if let Some((existing, _)) = self.effects.get(&effect.id) {
                return Err(validation(format!(
                    "effect {} is already provided by {}",
                    effect.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for action in &descriptor.actions {
            if let Some((existing, _)) = self.actions.get(&action.id) {
                return Err(validation(format!(
                    "action {} is already provided by {}",
                    action.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for query in &descriptor.queries {
            if let Some((existing, _)) = self.queries.get(&query.id) {
                return Err(validation(format!(
                    "query {} is already provided by {}",
                    query.id,
                    self.modules[*existing].descriptor().id
                )));
            }
        }
        for task in &descriptor.tasks {
            if let Some((existing, _)) = self.tasks.get(&task.id) {
                return Err(validation(format!(
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
            return Err(validation(format!(
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

    pub fn descriptors(&self) -> Vec<&ModuleDescriptor> {
        self.modules
            .iter()
            .map(|module| module.descriptor())
            .collect()
    }

    pub fn action(&self, id: &str) -> Option<(&dyn ToolModule, &ActionDescriptor)> {
        let (module, position) = self.actions.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().actions[*position]))
    }

    /// The one action lookup every caller resolves an action through: a registered module's
    /// action, or one of the host's own `mask.*` commands, which the host descriptor declares
    /// ([`crate::mask::commands::descriptor`]). The two namespaces cannot collide — a host action
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
    pub fn action_for_method(&self, method: &str) -> Option<ActionRef<'_>> {
        match method.strip_prefix("edit.") {
            Some(id) => self
                .action(id)
                .map(|(module, action)| ActionRef::Module(module, action)),
            None => crate::mask::commands::find(method).map(ActionRef::Host),
        }
    }

    /// The query an API method calls, the inverse of [`QueryRef::method`].
    pub fn query_for_method(&self, method: &str) -> Option<QueryRef<'_>> {
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
    pub fn query(&self, id: &str) -> Option<(&dyn ToolModule, &ActionDescriptor)> {
        let (module, position) = self.queries.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().queries[*position]))
    }

    /// The module that offers this worker task, and the task's declaration.
    pub fn task(&self, id: &str) -> Option<(&dyn ToolModule, &TaskDescriptor)> {
        let (module, position) = self.tasks.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().tasks[*position]))
    }

    /// The registered module with this identity. A linear scan: a registry holds a handful of
    /// modules, and the capability methods that ask are not on a per-pixel path.
    pub fn module(&self, id: &str) -> Option<&dyn ToolModule> {
        self.modules
            .iter()
            .map(AsRef::as_ref)
            .find(|module| module.descriptor().id == id)
    }

    pub fn effect(&self, id: &str) -> Option<(&dyn ToolModule, &EffectDescriptor)> {
        let (module, position) = self.effects.get(id)?;
        let module = self.modules[*module].as_ref();
        Some((module, &module.descriptor().effects[*position]))
    }

    /// The stage an effect's payload addresses, or `None` when no provider declares it.
    pub fn effect_stage(&self, effect_id: &str) -> Option<EffectStage> {
        self.effect(effect_id).map(|(_, effect)| effect.stage)
    }

    /// The stage and the within-stage order an effect declares, or `None` when no provider declares
    /// the effect at all.
    fn effect_placement(&self, effect_id: &str) -> Option<(EffectStage, u16)> {
        self.effect(effect_id)
            .map(|(_, effect)| (effect.stage, effect.order))
    }

    /// Where a committed layer of this effect joins a stack, by the stage and the order its
    /// descriptor declares. A module names its own effect rather than repeating its placement.
    /// An effect no provider declares is placed as a geometry effect would be and refused by the
    /// whole-stack compile that follows; a *layer* whose effect no provider declares opens no
    /// region either, because such a stack cannot compile at all and the host reports that rather
    /// than guessing a position.
    pub fn insertion_index_for(&self, layers: &[Layer], effect_id: &str) -> usize {
        let (stage, order) = self
            .effect_placement(effect_id)
            .unwrap_or((EffectStage::Geometry, 0));
        self.insertion_index(layers, stage, order)
    }

    /// Where a committed layer of this effect **bound to this target** joins a stack, the global
    /// layer and each mask being distinct targets (`docs/design/masking.md`, "Order"):
    ///
    /// 1. A masked layer of an effect is placed after the global layer of that effect.
    /// 2. Masked layers of one effect are ordered by their mask's index in `recipe.masks`.
    ///
    /// so overlapping masks apply in the order the mask list shows. Nothing else moves: the stage
    /// region and the within-stage order are [`Self::insertion_index_for`]'s, and this only decides
    /// where among the layers of the *same* effect the new one goes, which is inside that region by
    /// construction. A stack that already holds those layers in another order keeps it and renders
    /// in it, exactly as every other placement rule promises.
    ///
    /// Cost is `O(layers)` descriptor lookups plus `O(layers · masks)` target ranks; it reads no
    /// pixels and allocates nothing.
    pub fn insertion_index_for_target(
        &self,
        layers: &[Layer],
        effect_id: &str,
        mask: Option<&MaskId>,
        masks: &[Mask],
    ) -> usize {
        let base = self.insertion_index_for(layers, effect_id);
        let rank = target_rank(mask, masks);
        let same_effect = |layer: &Layer| layer.effect_id == effect_id;
        if rank == 0 {
            // The global layer of an effect keeps exactly the placement it has always had, except
            // that it must not land after a masked layer of its own effect: nothing about masking
            // changes where an unmasked layer goes.
            return match layers
                .iter()
                .position(|layer| same_effect(layer) && layer.mask.is_some())
            {
                Some(index) if index < base => index,
                _ => base,
            };
        }
        let mut after = None;
        let mut before = None;
        for (index, layer) in layers.iter().enumerate() {
            if !same_effect(layer) {
                continue;
            }
            if target_rank(layer.mask.as_ref(), masks) <= rank {
                after = Some(index + 1);
            } else if before.is_none() {
                before = Some(index);
            }
        }
        after.or(before).unwrap_or(base)
    }

    /// Re-sort the masked layers of every effect into the order the mask list now shows, and move
    /// nothing else. This is the second half of the ordering rule: `mask.reorder` changes the index
    /// of a mask, so the layers bound to it change position among the layers of *their own effect*,
    /// as one host transaction.
    ///
    /// Only positions already held by layers of one effect are rewritten, so no layer crosses a
    /// stage boundary, a spatial layer stays after the pointwise ones, and a stack with no masked
    /// layers is returned untouched. The sort is stable, so two layers of one effect with the same
    /// target — a stack the compile below refuses as ambiguous — keep their relative order rather
    /// than being reshuffled. `O(layers · masks)`; reads no pixels.
    pub fn sort_masked_layers(&self, layers: &mut [Layer], masks: &[Mask]) {
        let effects: Vec<String> = layers
            .iter()
            .filter(|layer| layer.mask.is_some())
            .map(|layer| layer.effect_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for effect_id in effects {
            let positions: Vec<usize> = layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.effect_id == effect_id)
                .map(|(index, _)| index)
                .collect();
            let mut ordered: Vec<Layer> = positions
                .iter()
                .map(|index| layers[*index].clone())
                .collect();
            ordered.sort_by_key(|layer| target_rank(layer.mask.as_ref(), masks));
            for (index, layer) in positions.into_iter().zip(ordered) {
                layers[index] = layer;
            }
        }
    }

    /// Whether a layer of this effect may carry a mask, as the effect's own descriptor declares.
    pub fn effect_maskable(&self, effect_id: &str) -> bool {
        self.effect(effect_id)
            .is_some_and(|(_, effect)| effect.maskable)
    }

    /// Whether a stack holds at most one layer of this effect per target, as the effect's own
    /// descriptor declares.
    pub fn effect_single(&self, effect_id: &str) -> bool {
        self.effect(effect_id)
            .is_some_and(|(_, effect)| effect.single)
    }

    /// Whether `layer` is part of the stack the target `mask` sees: a layer of a maskable effect
    /// only when it carries that same target, and every other layer always. `None` is the global
    /// target. Planning filters a maskable module's stack by it, and [`Self::own_layer`] finds a
    /// module's layer by it, so a capture, a plan and a query all read the layer of one target.
    pub fn in_target(&self, layer: &Layer, mask: Option<&MaskId>) -> bool {
        layer.mask.as_ref() == mask || !self.effect_maskable(&layer.effect_id)
    }

    /// The one layer of `effect_id` that belongs to `target`, with its index, or `None` when the
    /// stack holds none: how every module that owns one layer finds it, through
    /// [`super::StageContext::own_layer`], and how a preset captures one. A stack that holds two
    /// such layers is refused with `ambiguous <module title> layers`, the refusal the whole-stack
    /// compile makes for a `single` effect, rather than resolved by guessing; nothing is rewritten.
    /// `O(layers)`; reads no pixels.
    pub fn own_layer<'l>(
        &self,
        layers: &'l [Layer],
        effect_id: &str,
        target: Option<&MaskId>,
    ) -> Result<Option<(usize, &'l Layer)>, Error> {
        let mut found = None;
        for (index, layer) in layers.iter().enumerate() {
            if layer.effect_id != effect_id || !self.in_target(layer, target) {
                continue;
            }
            if found.is_some() {
                return Err(self.ambiguous(effect_id));
            }
            found = Some((index, layer));
        }
        Ok(found)
    }

    /// The refusal of a stack that holds two layers of one effect for one target, named by the
    /// providing module's title (or the effect, when no provider declares it).
    fn ambiguous(&self, effect_id: &str) -> Error {
        let title = self
            .effect(effect_id)
            .map_or(effect_id, |(module, _)| module.descriptor().title.as_str());
        validation(format!("ambiguous {title} layers"))
    }

    /// Whether this action carries the host's optional `mask` target field.
    ///
    /// The field belongs to the actions of a maskable effect, and an action is declared by a module
    /// rather than by an effect, so the module that owns the action is what answers: a module that
    /// declares a maskable effect accepts the field on its actions. Every delivered maskable module
    /// declares exactly one effect, so there is no case where this is wider than the design's
    /// sentence; a later module that declared both a maskable and a non-maskable effect would need
    /// an action-to-effect link that no descriptor carries today.
    pub fn action_accepts_mask(&self, action_id: &str) -> bool {
        let Some((module, _)) = self.actions.get(action_id) else {
            return false;
        };
        self.modules[*module]
            .descriptor()
            .effects
            .iter()
            .any(|effect| effect.maskable)
    }

    /// Where a committed layer of this stage and order joins a stack:
    ///
    /// | Stage | Placement |
    /// | --- | --- |
    /// | `source` | index zero |
    /// | `pixel`, `color` | before the first spatial, geometry or finish layer |
    /// | `spatial` | before the first geometry or finish layer, after every pixel and colour layer |
    /// | `geometry` | before the first finish layer |
    /// | `finish` | at the end |
    ///
    /// so the geometry tail carries every content-stage edit, a neighbourhood operation reads the
    /// finished pointwise colour, and a finish effect sees the output coordinates the tail
    /// produced. A leading source layer keeps index zero whatever is inserted.
    ///
    /// Within the region its stage chooses, the new layer goes after the last layer of the *same*
    /// stage whose order is at most `order` and before the first whose order is greater. Layers of
    /// other stages inside the region keep their positions, and no existing layer ever moves, so a
    /// stack stored in another order stays exactly as it is and renders in its stored order.
    ///
    /// Cost is `O(layers)` in descriptor lookups; it reads no pixels and allocates nothing.
    pub fn insertion_index(&self, layers: &[Layer], stage: EffectStage, order: u16) -> usize {
        placement_index(layers, stage, order, |effect_id| {
            self.effect_placement(effect_id)
        })
    }

    /// Whether this stack may be rendered against a downscaled proxy source.
    ///
    /// A source-stage, colour-stage, geometry-stage or finish-stage effect is resolution
    /// independent: the source development is pointwise, a colour unit is pointwise, the geometry
    /// payloads are normalized to their own input stage and a finish unit's mask is normalized to
    /// the output stage, so the same recipe compiles unchanged against a smaller content stage and
    /// produces the same picture at display size. A spatial-stage effect is eligible too, but its
    /// neighbourhoods scale with the stage, so its proxy frame is an approximation of the exact
    /// render at display size rather than the same picture; [`Self::proxy_approximation`] says when
    /// a stack renders that way, and the exact phase still produces every number. A pixel-stage
    /// effect is not eligible: its payload addresses content pixels, which a rescaled stage no
    /// longer has. An effect no provider declares is ineligible too, because nothing can say what
    /// stage it addresses.
    ///
    /// Cost is `O(layers)` and reads no pixels. The error names the first ineligible layer's effect
    /// identity and its index, so the caller reports the reason rather than silently taking the
    /// exact path.
    pub fn proxy_eligible(&self, recipe: &Recipe) -> Result<(), Error> {
        for (index, layer) in recipe.layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(
                    EffectStage::Source
                    | EffectStage::Color
                    | EffectStage::Spatial
                    | EffectStage::Geometry
                    | EffectStage::Finish,
                ) => {}
                Some(EffectStage::Pixel) => {
                    return Err(validation(format!(
                        "layer {index} is not proxy-eligible: effect {} is at the pixel stage, \
                         whose coordinates are content pixels and cannot be rescaled",
                        layer.effect_id
                    )));
                }
                None => {
                    return Err(validation(format!(
                        "layer {index} is not proxy-eligible: no provider declares effect {}, so \
                         its stage is unknown",
                        layer.effect_id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Why a proxy render of this stack at this source size is an approximation, which is the
    /// answer a frame is reported with.
    ///
    /// Two reasons, both read from a compilation at exactly the dimensions the proxy phase renders
    /// — the same compilation, so what is reported and what is drawn cannot disagree:
    ///
    /// - **A spatial operation.** Its neighbourhoods scale with the stage it is rendered at, so its
    ///   display-size frame is close to the exact render but not the same picture. It is what the
    ///   stack compiles to that decides, not which stages its effects declare: a neutral spatial
    ///   layer, such as a reset Presence layer, compiles to no operation at all, so its frame is the
    ///   exact recipe at proxy size and is not labelled.
    /// - **A thin mask.** A mask's geometry is normalized, so whether it draws a feature the proxy's
    ///   pixel grid can resolve is a fact about that grid.
    ///
    /// It costs `O(layers + components)` and reads no pixels. A stack that does not compile at that
    /// size has no proxy frame at all, and the caller has already declined it with its own reason,
    /// so there is nothing here to add.
    pub fn proxy_approximation(
        &self,
        recipe: &Recipe,
        source_width: u32,
        source_height: u32,
    ) -> ProxyApproximation {
        match self.compile_sampled(
            source_width,
            source_height,
            recipe,
            MaskSampling::ThinFeature,
        ) {
            Ok(compiled) => ProxyApproximation {
                spatial: compiled.evaluates_spatial(),
                mask: compiled.supersampled_masks(),
            },
            Err(_) => ProxyApproximation::default(),
        }
    }

    /// The provider that can evaluate this effect, or `None` when none is registered or the
    /// registered one reports itself unavailable.
    fn provider(&self, effect_id: &str) -> Option<&dyn ToolModule> {
        let (module, _) = self.effect(effect_id)?;
        module.descriptor().is_available().then_some(module)
    }

    /// Whether this stored layer changes nothing, by its available provider's own rule
    /// ([`ToolModule::is_neutral`]). A layer whose provider is missing or unavailable, or whose
    /// payload the provider cannot read, is not neutral: nothing can say it changes nothing.
    /// Reading the payload only.
    pub fn layer_neutral(&self, layer: &Layer) -> bool {
        self.provider(&layer.effect_id).is_some_and(|module| {
            module
                .is_neutral(&layer.effect_id, layer.effect_format, &layer.payload)
                .unwrap_or(false)
        })
    }

    /// Structural validation stays in the model; effect availability, whether the effect may
    /// reference artifacts and payload validation are the registry's.
    pub fn validate_layer(&self, layer: &Layer) -> Result<(), Error> {
        layer.validate()?;
        let module = self
            .provider(&layer.effect_id)
            .ok_or_else(|| unavailable(&layer.effect_id, vec![layer.id.as_str()]))?;
        self.check_artifacts(layer)?;
        module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)
    }

    /// Everything a stack must satisfy to be written, checked once where it enters the service
    /// ([`crate::EditorService`]'s admission): the layers' structure and mask references, the whole
    /// mask table ([`Recipe::validate_mask_table`]), masks only on stages that can carry one, and
    /// every layer's effect available, its artifacts declared and its payload accepted by its
    /// provider. `O(layers + components + strokes)`; it reads no pixels.
    pub fn validate_recipe(&self, recipe: &Recipe) -> Result<(), Error> {
        #[cfg(test)]
        crate::editor::validations::validated();
        recipe.validate()?;
        recipe.validate_mask_table()?;
        self.validate_masked_stages(recipe)?;
        for layer in &recipe.layers {
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(&recipe.layers, &layer.effect_id))?;
            self.check_artifacts(layer)?;
            module.validate_payload(&layer.effect_id, layer.effect_format, &layer.payload)?;
        }
        Ok(())
    }

    /// A mask's geometry is stored in content-stage coordinates, so only a layer whose input is that
    /// content stage may carry one: a geometry layer changes the stage and a finish layer is defined
    /// in the output coordinates the geometry tail produced, and neither has a content stage to read
    /// a mask in. The rule lives here rather than in the model because the stage is declared by the
    /// effect's provider, not by the recipe. `O(layers)` descriptor lookups, no pixels. An effect no
    /// provider declares is left to the unavailable report that follows, which names it already.
    fn validate_masked_stages(&self, recipe: &Recipe) -> Result<(), Error> {
        for layer in &recipe.layers {
            if layer.mask.is_none() {
                continue;
            }
            if let Some(stage @ (EffectStage::Geometry | EffectStage::Finish)) =
                self.effect_stage(&layer.effect_id)
            {
                return Err(validation(format!(
                    "layer {} carries a mask, which a {} effect cannot: a mask is stored in \
                     content-stage coordinates",
                    layer.id,
                    stage.as_str()
                )));
            }
        }
        Ok(())
    }

    /// Refuse a layer whose mask this build cannot evaluate.
    ///
    /// A mask reaches a colour operation and a spatial operation, and nothing else: a point
    /// replacement writes one stored pixel and has no blend to perform, so there is nothing for a
    /// coverage to modulate. (A geometry or finish layer cannot carry a mask at all, for the earlier
    /// reason that it has no content stage to read one in; that is
    /// [`Self::validate_masked_stages`].) A mask this build cannot evaluate must never be silently
    /// omitted from a frame or an export, so such a stack is refused by name wherever it would be
    /// drawn — and because the host compiles a stack before it persists one, the refusal is also
    /// what keeps such a layer from being committed at all. It reads the stack and rewrites nothing.
    fn refuse_unevaluated_mask(&self, layer: &Layer) -> Result<(), Error> {
        if layer.mask.is_none() {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Incompatible,
            format!(
                "layer {} carries a mask on the {} effect {}, and this build evaluates a mask only \
                 on a colour-stage or spatial-stage effect",
                layer.id,
                self.effect_stage(&layer.effect_id)
                    .map_or("unknown", EffectStage::as_str),
                layer.effect_id
            ),
        ))
    }

    /// The compiled mask one layer is modulated by, against the stage that layer receives, or
    /// `None` for a global layer.
    ///
    /// Compiling is `O(components)` and reads no pixels, so a masked layer costs the same to compile
    /// as an unmasked one plus a handful of closed-form terms per component. Two layers bound to one
    /// mask compile it twice rather than sharing one compilation: the cost is bounded by the
    /// components-per-mask limit and a cache would have to be keyed by stage as well as identity, so
    /// it is not worth the machinery until a measurement says otherwise.
    ///
    /// A reference the mask table does not hold is refused here as well as by [`Recipe::validate`],
    /// because a prefix compile is reached without the recipe.
    fn compiled_mask(
        layer: &Layer,
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        stage: Stage,
        sampling: MaskSampling,
    ) -> Result<Option<MaskField>, Error> {
        let Some(id) = &layer.mask else {
            return Ok(None);
        };
        let mask = masks.iter().find(|mask| &mask.id == id).ok_or_else(|| {
            Error::new(
                ErrorKind::Incompatible,
                format!(
                    "layer {} names mask {id}, which this recipe does not hold",
                    layer.id
                ),
            )
        })?;
        Ok(Some(MaskField::compile(mask, stage, strokes, sampling)?))
    }

    /// Only a layer of an effect that declares `artifacts` may reference any. The host owns the
    /// list, so this is the host's rule, checked before the module sees the payload.
    fn check_artifacts(&self, layer: &Layer) -> Result<(), Error> {
        let declared = self
            .effect(&layer.effect_id)
            .is_some_and(|(_, effect)| effect.artifacts);
        if layer.artifacts.is_empty() || declared {
            Ok(())
        } else {
            Err(validation(format!(
                "layer {} of effect {} references artifacts, which its effect does not declare",
                layer.id, layer.effect_id
            )))
        }
    }

    fn unavailable_in(&self, layers: &[Layer], effect_id: &str) -> Error {
        unavailable(
            effect_id,
            layers
                .iter()
                .filter(|layer| layer.effect_id == effect_id)
                .map(|layer| layer.id.as_str())
                .collect(),
        )
    }

    /// Validate a recipe against the source dimensions and fold its exact geometry into one mapping
    /// per rasterizing pass. A resample is a stage boundary, so it closes the current pass and opens
    /// the next one. Cost is linear in the layer count and allocates only the operation lists.
    pub(crate) fn compile(
        &self,
        source_width: u32,
        source_height: u32,
        recipe: &Recipe,
    ) -> Result<Compiled, Error> {
        self.compile_sampled(source_width, source_height, recipe, MaskSampling::Point)
    }

    /// [`Self::compile`] with the way this render samples its masks as a parameter.
    ///
    /// Every exact render point samples, which is the frozen field; only the proxy phase passes
    /// [`MaskSampling::ThinFeature`], and only a mask that draws a feature narrower than two pixels
    /// *at the stage compiled here* is affected by it. Nothing else about compiling changes, which
    /// is what keeps a proxy frame byte for byte the exact recipe over the exact downscale wherever
    /// the rule does not fire.
    pub(crate) fn compile_sampled(
        &self,
        source_width: u32,
        source_height: u32,
        recipe: &Recipe,
        sampling: MaskSampling,
    ) -> Result<Compiled, Error> {
        // The layer checks every evaluation path shares: the format marker, the layers' structure
        // and each layer's mask reference, and a mask only where a stage can carry one. They cost
        // `O(layers)` and read no pixels, so compiling here is what makes a stack that names a mask
        // it does not carry, or attaches one to the geometry tail, fail rendering, sampling, proxy
        // planning and module planning alike.
        //
        // The mask table itself is not checked again: it was checked once when the recipe entered
        // the service (`Recipe::validate_mask_table`, from admission and from a drafted mask
        // gesture), and a stored recipe was admitted when it was written. What a masked layer needs
        // drawn is refused where it is drawn: compiling its mask parses every component through the
        // kind table and resolves every stroke, so a mask this build cannot evaluate, or a stroke
        // the store has lost, still refuses every path that would draw it by name, and nothing is
        // rewritten or resolved to an empty stroke. A mask no layer draws changes no pixel, so
        // rendering its stack draws exactly what the stack says.
        recipe.validate()?;
        self.validate_masked_stages(recipe)?;
        self.compile_layers_sampled(
            source_width,
            source_height,
            &recipe.layers,
            &recipe.masks,
            &recipe.strokes,
            &recipe.artifacts,
            sampling,
        )
    }

    /// Compile an ordered layer slice whose recipe format is already known good, against the mask
    /// table its layers reference. Asking for the stage one layer receives compiles the prefix
    /// before it through here, so it copies no part of the stack; a prefix carries the whole mask
    /// table, because the masks a prefix layer names are the recipe's and not the prefix's, and
    /// the recipe's bound artifacts for the same reason.
    pub(crate) fn compile_layers(
        &self,
        source_width: u32,
        source_height: u32,
        layers: &[Layer],
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        artifacts: &ArtifactTable,
    ) -> Result<Compiled, Error> {
        self.compile_layers_sampled(
            source_width,
            source_height,
            layers,
            masks,
            strokes,
            artifacts,
            MaskSampling::Point,
        )
    }

    /// [`Self::compile_layers`] with the mask sampling of the render being compiled.
    #[allow(clippy::too_many_arguments)]
    fn compile_layers_sampled(
        &self,
        source_width: u32,
        source_height: u32,
        layers: &[Layer],
        masks: &[Mask],
        strokes: &crate::path::StrokeTable,
        artifacts: &ArtifactTable,
        sampling: MaskSampling,
    ) -> Result<Compiled, Error> {
        let mut layer_ids = HashSet::with_capacity(layers.len());
        // The effects whose module owns exactly one layer of a stack, seen so far, **per target**:
        // the global layer and each mask are distinct targets, so one effect may hold a layer in
        // each mask and still hold one global layer. Two layers of one effect with the same target
        // are what a module cannot resolve, so the host refuses that stack here as well as when the
        // module plans against it, and rewrites nothing.
        let mut single_effects: HashSet<(&str, Option<&MaskId>)> = HashSet::new();
        let mut segments = vec![Segment::new(None, source_width, source_height)];
        // The one order the host cannot evaluate: a finish effect is defined in the output
        // coordinates of the geometry tail, so a geometry layer after it has no stage to address.
        // The stack is refused as it stands and nothing is rewritten or reordered.
        let mut finish_layer: Option<&Layer> = None;
        // Masked spatial layers seen so far, against the declared cap. Each one is a stage boundary
        // and therefore a sequential full frame, which is the whole reason there is a cap.
        let mut masked_spatial = 0_usize;
        for (index, layer) in layers.iter().enumerate() {
            match self.effect_stage(&layer.effect_id) {
                Some(EffectStage::Source) if index != 0 => {
                    return Err(validation("source-stage effect must be at index zero"));
                }
                Some(EffectStage::Finish) => finish_layer = finish_layer.or(Some(layer)),
                Some(EffectStage::Geometry) => {
                    if let Some(finish) = finish_layer {
                        return Err(validation(format!(
                            "finish layer precedes geometry (finish {}, geometry {})",
                            finish.id, layer.id
                        )));
                    }
                }
                _ => {}
            }
            if !layer_ids.insert(&layer.id) {
                return Err(validation("duplicate layer identity"));
            }
            let module = self
                .provider(&layer.effect_id)
                .ok_or_else(|| self.unavailable_in(layers, &layer.effect_id))?;
            if self.effect_single(&layer.effect_id)
                && !single_effects.insert((layer.effect_id.as_str(), layer.mask.as_ref()))
            {
                return Err(self.ambiguous(&layer.effect_id));
            }
            let segment = segments.last_mut().expect("one segment always exists");
            let stage = Stage {
                width: segment.width,
                height: segment.height,
            };
            let processing = if layer.artifacts.is_empty() {
                module.compile(&layer.effect_id, layer.effect_format, &layer.payload, stage)?
            } else {
                // The recipe carries the verified bytes it was bound with, so resolving them is a
                // lookup; an artifact the recipe was not bound with is refused, never skipped.
                self.check_artifacts(layer)?;
                let bound = layer
                    .artifacts
                    .iter()
                    .map(|id| {
                        artifacts.get(id).cloned().ok_or_else(|| {
                            Error::new(
                                ErrorKind::SourceUnavailable,
                                format!(
                                    "artifact {id} of layer {} is not bound to this recipe",
                                    layer.id
                                ),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                module.compile_bound(
                    &layer.effect_id,
                    layer.effect_format,
                    &layer.payload,
                    stage,
                    &bound,
                )?
            };
            match processing {
                Processing::ExactGeometry(step) => {
                    if !step.reads_inside(segment.width, segment.height) {
                        return Err(validation(format!(
                            "an exact mapping to {}x{} reads outside its {}x{} input stage",
                            step.output_width, step.output_height, segment.width, segment.height
                        )));
                    }
                    segment.geometry = segment.geometry.then(step);
                    segment.width = step.output_width;
                    segment.height = step.output_height;
                    segment.operations.push(Processing::ExactGeometry(step));
                }
                Processing::PointReplace { x, y, rgb } => {
                    // A point replacement has no blend to perform, so a mask on one would be a mask
                    // this build cannot evaluate. No delivered pixel-stage effect declares itself
                    // maskable; refusing by name is what keeps a later one from silently rendering
                    // its replacement everywhere instead of through the selection.
                    self.refuse_unevaluated_mask(layer)?;
                    segment.has_pixels = true;
                    segment
                        .operations
                        .push(Processing::PointReplace { x, y, rgb });
                }
                Processing::Color(operation) => {
                    if operation.len() > MAX_COLOR_UNITS {
                        return Err(validation(format!(
                            "a colour operation declares {} units, more than the {MAX_COLOR_UNITS} the host evaluates",
                            operation.len()
                        )));
                    }
                    if !operation.is_finite() {
                        return Err(validation(
                            "a colour operation declares a unit whose coefficients are not finite",
                        ));
                    }
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // segment keeps the identity byte path and the shared source buffer, mask or no
                    // mask. Masking nothing is nothing, so no mask is compiled for it either.
                    if !operation.is_empty() {
                        // The mask is the host's, attached here — where a compiled layer becomes
                        // `Processing` — against the stage this layer receives, which for a
                        // content-stage layer is the content stage its geometry is normalized to. A
                        // module returned a plain operation and never saw the reference.
                        let operation =
                            match Self::compiled_mask(layer, masks, strokes, stage, sampling)? {
                                Some(mask) => operation.with_mask(mask),
                                None => operation,
                            };
                        segment.has_color = true;
                        segment.operations.push(Processing::Color(operation));
                    }
                }
                Processing::Spatial(operation) => {
                    // A neutral payload compiles to no units, and no units is no processing: the
                    // stack keeps its single pass, the identity byte path and the shared source
                    // buffer, exactly as a neutral colour payload does. Masking nothing is nothing,
                    // so no mask is compiled for it either.
                    if operation.is_empty() {
                        continue;
                    }
                    // The mask is the host's, attached here — where a compiled layer becomes
                    // `Processing` — against the stage this layer receives. A spatial layer is a
                    // stage boundary, so that stage is also the frame the operation reads and
                    // writes, which is what lets the tile loop read the mask at a tile's own
                    // coordinates. The module returned a plain operation and never saw the
                    // reference.
                    let operation = match Self::compiled_mask(
                        layer, masks, strokes, stage, sampling,
                    )? {
                        Some(mask) => {
                            masked_spatial += 1;
                            if masked_spatial > MAX_MASKED_SPATIAL_LAYERS {
                                return Err(Error::new(
                                    ErrorKind::ResourceLimit,
                                    format!(
                                        "this recipe holds {masked_spatial} masked spatial layers, more than the \
                                         {MAX_MASKED_SPATIAL_LAYERS} the host evaluates: each one is a stage \
                                         boundary and therefore a sequential full frame"
                                    ),
                                ));
                            }
                            operation.with_mask(mask)
                        }
                        None => operation,
                    };
                    // Everything stage-dependent the operation declares — the unit count, their
                    // finiteness and the summed halo — is checked here, before a pixel is read.
                    // Nothing is rewritten or reduced to fit, and what a tile costs in memory,
                    // which a mask adds two tile planes to, never refuses it.
                    SpatialPlan::new(&operation, stage, SPATIAL_TILE)?;
                    let prefix_hash = prefix_hash(&layers[..index], masks, sampling)?;
                    segments.push(Segment::new(
                        Some(Entry::Spatial {
                            operation,
                            prefix_hash,
                        }),
                        stage.width,
                        stage.height,
                    ));
                }
                Processing::Resample(resample) => {
                    if resample.output_width == 0 || resample.output_height == 0 {
                        return Err(validation("a resample declares an empty output stage"));
                    }
                    if !resample.inverse.iter().all(|value| value.is_finite()) {
                        return Err(validation(
                            "a resample declares a mapping that is not finite",
                        ));
                    }
                    segments.push(Segment::new(
                        Some(Entry::Resample(resample)),
                        resample.output_width,
                        resample.output_height,
                    ));
                }
            }
        }
        Ok(Compiled { segments })
    }
}

/// The placement rule of [`ModuleRegistry::insertion_index`] over any lookup of an effect's
/// declared stage and order, so the host's registry and a client's module list answer alike.
fn placement_index(
    layers: &[Layer],
    stage: EffectStage,
    order: u16,
    placement: impl Fn(&str) -> Option<(EffectStage, u16)>,
) -> usize {
    if stage == EffectStage::Source {
        return 0;
    }
    // A source layer prepares the content stage and always stays at index zero.
    let mut lower = usize::from(layers.first().is_some_and(|layer| {
        placement(&layer.effect_id).map(|(stage, _)| stage) == Some(EffectStage::Source)
    }));
    let opens_region = |candidate: EffectStage| match stage {
        EffectStage::Pixel | EffectStage::Color => matches!(
            candidate,
            EffectStage::Spatial | EffectStage::Geometry | EffectStage::Finish
        ),
        EffectStage::Spatial => {
            matches!(candidate, EffectStage::Geometry | EffectStage::Finish)
        }
        EffectStage::Geometry => candidate == EffectStage::Finish,
        EffectStage::Source | EffectStage::Finish => false,
    };
    let mut upper = layers.len();
    // The first layer of the same stage whose order is greater: the new layer goes before it.
    let mut successor = None;
    for (index, layer) in layers.iter().enumerate() {
        let Some((layer_stage, layer_order)) = placement(&layer.effect_id) else {
            continue;
        };
        if opens_region(layer_stage) {
            upper = index;
            break;
        }
        // A spatial layer reads what the pointwise colour run produced, so it never lands
        // before a pixel or colour layer a stored stack kept later than usual.
        if stage == EffectStage::Spatial
            && matches!(layer_stage, EffectStage::Pixel | EffectStage::Color)
        {
            lower = index + 1;
        }
        if layer_stage == stage && layer_order > order && successor.is_none() {
            successor = Some(index);
        }
    }
    let upper = upper.max(lower);
    successor.unwrap_or(upper).clamp(lower, upper)
}

/// Where a committed layer of `effect_id` joins `layers`, by the host's placement rule read from a
/// client's module descriptors (what `module.list` returns) instead of a registry. A client that
/// has to show a stage the host will address, such as a crop draft showing the crop's input stage
/// before any crop exists, asks this rather than repeating the rule. An effect no listed module
/// declares is placed as a geometry effect would be, as the host places it.
pub fn insertion_index_among(
    modules: &[ModuleDescriptor],
    layers: &[Layer],
    effect_id: &str,
) -> usize {
    let placement = |id: &str| {
        modules
            .iter()
            .flat_map(|module| module.effects.iter())
            .find(|effect| effect.id == id)
            .map(|effect| (effect.stage, effect.order))
    };
    let (stage, order) = placement(effect_id).unwrap_or((EffectStage::Geometry, 0));
    placement_index(layers, stage, order, placement)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        AssetId, BASIC_EFFECT, CROP_EFFECT, Component, ComponentMode, EFFECT_FORMAT, LayerId, Mask,
        ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT, RAW_EFFECT, RECIPE_FORMAT, SnapshotId,
        SourceImage,
        modules::{
            ActionInput, ActionPlan, Availability, CropPayload, EffectStage, ModuleDescriptor,
            StageContext,
        },
        render, sample,
    };
    use serde_json::{Map, Value, json};

    /// A minimal module used to prove registration rules and missing-provider behavior.
    pub(crate) struct TestModule(ModuleDescriptor);

    impl TestModule {
        pub(crate) fn new(
            id: &str,
            effect: &str,
            action: &str,
            availability: Availability,
        ) -> Self {
            Self(ModuleDescriptor {
                id: id.into(),
                title: "Test".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: effect.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                }],
                actions: vec![ActionDescriptor {
                    id: action.into(),
                    title: "Test action".into(),
                    notes: "test".into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability,
                ..ModuleDescriptor::default()
            })
        }
        /// A module whose descriptor is written by the test itself.
        pub(crate) fn from_descriptor(descriptor: ModuleDescriptor) -> Arc<dyn ToolModule> {
            Arc::new(Self(descriptor))
        }
        pub(crate) fn shared(
            id: &str,
            effect: &str,
            action: &str,
            availability: Availability,
        ) -> Arc<dyn ToolModule> {
            Arc::new(Self::new(id, effect, action, availability))
        }
    }

    impl ToolModule for TestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("test layer of {effect_id}"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Err(Error::new(ErrorKind::Internal, "test module never renders"))
        }
    }

    pub(crate) const PATCH_MODULE: &str = "test.patch";
    pub(crate) const PATCH_EFFECT: &str = "test.patch.effect";
    pub(crate) const PATCH_ACTION: &str = "set-patch";

    /// A module whose one action is a field patch, the shape Basic's sliders will take: the host
    /// hands it only the fields the caller named, it merges them over the layer it already has, and
    /// it reports an unchanged result as a no-op. Its layer replaces one pixel, so a preview, a
    /// sample and a rendered frame all show which fields are in effect.
    pub(crate) struct PatchModule(ModuleDescriptor);

    impl PatchModule {
        pub(crate) fn shared() -> Arc<dyn ToolModule> {
            let channel = |name: &str| {
                crate::ParameterDescriptor::number(name, 0.0, 255.0)
                    .default(json!(0.0))
                    .unit("code")
                    .step(1.0)
                    .precision(0)
                    .notes(format!("the {name} channel of the replaced pixel"))
            };
            Arc::new(Self(ModuleDescriptor {
                id: PATCH_MODULE.into(),
                title: "Patch".into(),
                hint: Some("A patched pixel".into()),
                effects: vec![EffectDescriptor {
                    id: PATCH_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Pixel,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                }],
                actions: vec![ActionDescriptor {
                    id: PATCH_ACTION.into(),
                    title: "Set patch".into(),
                    notes: "merges the named channels into the one patch layer".into(),
                    summary: Some("Patch {red} {green}".into()),
                    patch: true,
                    parameters: vec![channel("red"), channel("green")],
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            }))
        }

        /// The channels a payload holds; a missing channel is neutral.
        pub(crate) fn channels(payload: &Value) -> [f64; 2] {
            let channel = |name: &str| payload.get(name).and_then(Value::as_f64).unwrap_or(0.0);
            [channel("red"), channel("green")]
        }

        fn merged(payload: &Value, fields: &Map<String, Value>) -> Value {
            let [red, green] = Self::channels(payload);
            let field = |name: &str, current: f64| {
                fields.get(name).and_then(Value::as_f64).unwrap_or(current)
            };
            json!({"red": field("red", red), "green": field("green", green)})
        }
    }

    impl ToolModule for PatchModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(
            &self,
            action_id: &str,
            parameters: &Map<String, Value>,
        ) -> Result<ActionInput, Error> {
            // Exactly the fields the host checked: a patch stores what was sent, not the merge.
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: parameters.clone(),
            })
        }
        fn plan(
            &self,
            input: &ActionInput,
            context: &StageContext<'_>,
        ) -> Result<ActionPlan, Error> {
            let existing = context
                .layers
                .iter()
                .find(|layer| layer.effect_id == PATCH_EFFECT);
            let current = existing.map(|layer| layer.payload.clone());
            let payload = Self::merged(current.as_ref().unwrap_or(&json!({})), &input.parameters);
            match (existing, current) {
                (Some(_), Some(current))
                    if Self::channels(&current) == Self::channels(&payload) =>
                {
                    Ok(ActionPlan::NoOp)
                }
                (Some(layer), _) => Ok(ActionPlan::Update(crate::LayerUpdate::new(
                    layer.id.clone(),
                    payload,
                ))),
                (None, _) if Self::channels(&payload) == [0.0, 0.0] => Ok(ActionPlan::NoOp),
                (None, _) => Ok(ActionPlan::Commit(crate::NewLayer::new(
                    PATCH_EFFECT,
                    payload,
                ))),
            }
        }
        /// One changed field names itself, so a slider's history row says what moved.
        fn label(&self, input: &ActionInput) -> Option<String> {
            match input.parameters.len() {
                1 => input.parameters.iter().next().map(|(name, value)| {
                    format!("Patch {name} {}", value.as_f64().unwrap_or_default())
                }),
                _ => None,
            }
        }
        fn validate_payload(&self, _: &str, format: u32, payload: &Value) -> Result<(), Error> {
            if format != EFFECT_FORMAT {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!("unsupported effect format {format}"),
                ));
            }
            let object = payload
                .as_object()
                .ok_or_else(|| validation("patch payload must be an object"))?;
            for (name, value) in object {
                if !["red", "green"].contains(&name.as_str())
                    || !value
                        .as_f64()
                        .is_some_and(|value| (0.0..=255.0).contains(&value))
                {
                    return Err(validation(format!("invalid patch field {name}")));
                }
            }
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
            let [red, green] = Self::channels(payload);
            Ok(format!("Patch {red}, {green}"))
        }
        fn values(&self, _: &str, _: u32, payload: &Value) -> Result<Map<String, Value>, Error> {
            let [red, green] = Self::channels(payload);
            Ok(json!({"red": red, "green": green})
                .as_object()
                .expect("an object")
                .clone())
        }
        fn compile(&self, _: &str, _: u32, payload: &Value, _: Stage) -> Result<Processing, Error> {
            let [red, green] = Self::channels(payload);
            Ok(Processing::PointReplace {
                x: 0,
                y: 0,
                rgb: [red as u8, green as u8, 0],
            })
        }
    }

    pub(crate) const STAGE_EFFECT: &str = "test.stage.effect";
    pub(crate) const STAGE_ACTION: &str = "set-stage";

    /// A module whose one effect declares any stage and any order and compiles to an identity
    /// colour operation. Placement, the order within a stage and the one refused order are
    /// properties of the host, so they are proved with this rather than with a real tool: a spatial
    /// or finish effect has no processing primitive of its own yet.
    pub(crate) struct StageModule(ModuleDescriptor);

    impl StageModule {
        pub(crate) fn shared(
            id: &str,
            effect: &str,
            action: &str,
            stage: EffectStage,
            order: u16,
        ) -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: id.into(),
                title: "Stage".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: effect.into(),
                    format: EFFECT_FORMAT,
                    stage,
                    order,
                    maskable: false,
                    artifacts: false,
                    single: false,
                }],
                actions: vec![ActionDescriptor {
                    id: action.into(),
                    title: "Set stage".into(),
                    notes: "commits one layer of this module's effect".into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            }))
        }
    }

    impl ToolModule for StageModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::Commit(crate::NewLayer::new(
                self.0.effects[0].id.clone(),
                json!({}),
            )))
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok(format!("stage layer of {effect_id}"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::neutral()))
        }
    }

    pub(crate) const HELD_EFFECT: &str = "test.held.effect";
    pub(crate) const HELD_ACTION: &str = "hold-render";

    /// A gate a test shuts to hold every render that reaches it. It is a pointwise colour unit that
    /// leaves its pixels exactly as it found them, so a stack carrying one renders the image it
    /// would render without it; all it changes is *when* that render finishes.
    ///
    /// Shut it only while nothing samples a stack that holds the layer: a point sample evaluates
    /// the same unit on the calling thread, so the caller would wait with it.
    pub(crate) struct RenderGate {
        shut: std::sync::Mutex<bool>,
        opened: std::sync::Condvar,
        /// How many times anything has reached the gate: one per row a render evaluates through it.
        reached: std::sync::atomic::AtomicU64,
    }

    impl RenderGate {
        /// A gate that is open, which is how a test builds the stack before it holds anything.
        pub(crate) fn open_gate() -> Arc<Self> {
            Arc::new(Self {
                shut: std::sync::Mutex::new(false),
                opened: std::sync::Condvar::new(),
                reached: std::sync::atomic::AtomicU64::new(0),
            })
        }
        /// How many times anything has reached the gate, held or not. A render reaches it once per
        /// row, so this counts the rows a render has evaluated through the held layer.
        pub(crate) fn reached(&self) -> u64 {
            self.reached.load(std::sync::atomic::Ordering::SeqCst)
        }
        /// Hold every render that reaches this gate from now on.
        pub(crate) fn shut(&self) {
            *self.shut.lock().expect("the render gate") = true;
        }
        /// Release whatever is waiting and let every later render through.
        pub(crate) fn open(&self) {
            *self.shut.lock().expect("the render gate") = false;
            self.opened.notify_all();
        }
        /// Wait here while the gate is shut. A render reaches it through its colour unit; a test
        /// that holds other work, such as a source preparation, calls it from a hook in that work.
        pub(crate) fn pass(&self) {
            self.reached
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut shut = self.shut.lock().expect("the render gate");
            while *shut {
                shut = self.opened.wait(shut).expect("the render gate");
            }
        }
    }

    impl crate::PointwiseColor for RenderGate {
        fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {
            self.pass();
        }
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "held render".into()
        }
    }

    /// A module whose one colour effect compiles to a [`RenderGate`]. A test that is about the
    /// analysis worker's slots commits one of these layers and shuts the gate: the job on the
    /// worker then stays there until the test opens it, so what the single pending slot does is
    /// decided by the queue's rule and never by how fast this machine renders a frame.
    pub(crate) struct HeldModule {
        descriptor: ModuleDescriptor,
        gate: Arc<RenderGate>,
    }

    impl HeldModule {
        pub(crate) fn shared(gate: Arc<RenderGate>) -> Arc<dyn ToolModule> {
            Arc::new(Self {
                descriptor: ModuleDescriptor {
                    id: "test.held".into(),
                    title: "Held".into(),
                    hint: None,
                    effects: vec![EffectDescriptor {
                        id: HELD_EFFECT.into(),
                        format: EFFECT_FORMAT,
                        stage: EffectStage::Color,
                        order: 0,
                        artifacts: false,
                        single: false,
                        maskable: false,
                    }],
                    actions: vec![ActionDescriptor {
                        id: HELD_ACTION.into(),
                        title: "Hold render".into(),
                        notes: "commits one colour layer whose render waits for the test's gate"
                            .into(),
                        summary: None,
                        patch: false,
                        parameters: Vec::new(),
                    }],
                    queries: Vec::new(),
                    controls: Vec::new(),
                    reset: None,
                    canvas: None,
                    developer: false,
                    collapsed: false,
                    layout: crate::ModuleLayout::Stacked,
                    availability: Availability::Available,
                    ..ModuleDescriptor::default()
                },
                gate,
            })
        }
    }

    impl ToolModule for HeldModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.descriptor
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::Commit(crate::NewLayer::new(
                HELD_EFFECT,
                json!({}),
            )))
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok("held render".into())
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::new(vec![
                self.gate.clone(),
            ])))
        }
    }

    pub(crate) fn test_layer(effect: &str) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: effect.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn source() -> SourceImage {
        SourceImage {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255].into(),
            fingerprint: "sha256:test".into(),
            orientation: 1,
        }
    }

    #[test]
    fn registration_rejects_duplicate_and_invalid_identities_across_modules() {
        let mut registry = ModuleRegistry::builtin();
        assert!(registry.action("set-pixel").is_some());
        assert!(registry.action("transform").is_some());
        assert!(registry.effect(PIXEL_EFFECT).is_some());
        assert!(registry.effect(ORIENTATION_EFFECT).is_some());
        assert!(registry.action("crop").is_some());
        assert!(registry.effect(CROP_EFFECT).is_some());
        assert!(registry.action("set-basic").is_some());
        assert!(registry.action("reset-basic").is_some());
        assert!(registry.effect(BASIC_EFFECT).is_some());
        assert!(registry.action("set-mixer").is_some());
        assert!(registry.action("reset-mixer").is_some());
        assert!(registry.effect(crate::MIXER_EFFECT).is_some());
        assert!(registry.action("set-raw-exposure").is_some());
        assert!(registry.action("reset-raw").is_some());
        assert!(registry.effect(RAW_EFFECT).is_some());
        assert!(registry.action("set-vignette").is_some());
        assert!(registry.action("reset-vignette").is_some());
        assert!(registry.effect(crate::VIGNETTE_EFFECT).is_some());
        assert!(registry.action("set-presence").is_some());
        assert!(registry.action("reset-presence").is_some());
        assert!(registry.effect(crate::PRESENCE_EFFECT).is_some());
        assert!(registry.action("apply-preset").is_some());
        assert_eq!(registry.descriptors().len(), 9);
        assert!(registry.action("edit.set-pixel").is_none());

        for (case, module) in [
            (
                "duplicate module",
                TestModule::shared(
                    "lightwell.pixel",
                    "test.other",
                    "test-other",
                    Availability::Available,
                ),
            ),
            (
                "duplicate effect",
                TestModule::shared(
                    "test.module",
                    PIXEL_EFFECT,
                    "test-other",
                    Availability::Available,
                ),
            ),
            (
                "duplicate action",
                TestModule::shared(
                    "test.module",
                    "test.effect",
                    "set-pixel",
                    Availability::Available,
                ),
            ),
            (
                "invalid module identity",
                TestModule::shared(
                    "Test Module",
                    "test.effect",
                    "test-other",
                    Availability::Available,
                ),
            ),
        ] {
            let error = registry.register(module).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        }
        assert_eq!(
            registry.descriptors().len(),
            9,
            "nothing was half-registered"
        );
        assert!(
            registry
                .register(TestModule::shared(
                    "test.module",
                    "test.effect",
                    "test-action",
                    Availability::Available
                ))
                .is_ok()
        );
        assert_eq!(registry.descriptors().len(), 10);
    }

    /// A module that declares no effects owns no layer and claims no effect identity, so it
    /// registers like any other and its actions dispatch. The presets module is one.
    #[test]
    fn a_module_that_declares_no_effects_registers() {
        let mut registry = ModuleRegistry::builtin();
        let (presets, _) = registry.action("apply-preset").expect("the presets module");
        assert!(presets.descriptor().effects.is_empty());
        let mut descriptor = TestModule::new(
            "test.effectless",
            "test.unused",
            "test-effectless",
            Availability::Available,
        )
        .0;
        descriptor.effects.clear();
        registry
            .register(TestModule::from_descriptor(descriptor))
            .expect("a module without effects registers");
        let (module, _) = registry
            .action("test-effectless")
            .expect("its action is dispatched");
        assert_eq!(module.descriptor().id, "test.effectless");
        assert!(registry.effect("test.unused").is_none());
    }

    /// A module whose canvas claims one mode-strip letter.
    fn shortcut_module(id: &str, effect: &str, action: &str, letter: &str) -> Arc<dyn ToolModule> {
        let coordinate = |name: &str| {
            crate::ParameterDescriptor::integer(name, 0, 100)
                .required(true)
                .notes("test")
        };
        let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
        descriptor.actions[0].parameters = vec![coordinate("x"), coordinate("y")];
        descriptor.canvas = Some(crate::CanvasInteraction::PointPick {
            action: action.into(),
            x: "x".into(),
            y: "y".into(),
            title: "Test mode".into(),
            shortcut: Some(letter.into()),
        });
        // A pick canvas is reached from the panel, so it declares its picker control.
        descriptor.controls = vec![crate::Control::Picker {
            label: "Test mode".into(),
        }];
        TestModule::from_descriptor(descriptor)
    }

    #[test]
    fn one_canvas_shortcut_letter_selects_one_mode_across_the_registry() {
        let mut registry = ModuleRegistry::builtin();
        assert_eq!(
            registry
                .effect(CROP_EFFECT)
                .expect("the crop module")
                .0
                .descriptor()
                .canvas
                .as_ref()
                .and_then(crate::CanvasInteraction::shortcut),
            Some("R"),
            "the built-in crop mode claims R"
        );
        let error = registry
            .register(shortcut_module(
                "test.one",
                "test.one.effect",
                "test-one",
                "R",
            ))
            .expect_err("R is already claimed by the crop module");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("canvas shortcut R is already claimed"),
            "{error}"
        );
        registry
            .register(shortcut_module(
                "test.two",
                "test.two.effect",
                "test-two",
                "K",
            ))
            .expect("a free letter registers");
        let error = registry
            .register(shortcut_module(
                "test.three",
                "test.three.effect",
                "test-three",
                "K",
            ))
            .expect_err("K is now claimed too");
        assert_eq!(error.kind, ErrorKind::Validation);
    }

    /// A query identity is unique across the whole registry, so `query.<id>` can never resolve to
    /// two providers; it is its own namespace, so it does not collide with an action of that name.
    #[test]
    fn one_query_identity_resolves_to_one_provider_across_the_registry() {
        let mut registry = ModuleRegistry::builtin();
        let (module, query) = registry
            .query("neutral-sample")
            .expect("the Basic module declares the neutral picker");
        assert_eq!(module.descriptor().id, "lightwell.basic");
        assert_eq!(query.id, "neutral-sample");
        assert!(!query.patch);
        assert!(
            registry.query("set-basic").is_none(),
            "queries are separate"
        );
        assert!(registry.action("neutral-sample").is_none());

        let with_query = |id: &str, effect: &str, action: &str, query: &str| {
            let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
            descriptor.queries = vec![ActionDescriptor {
                id: query.into(),
                title: "Test query".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: Vec::new(),
            }];
            TestModule::from_descriptor(descriptor)
        };
        let error = registry
            .register(with_query(
                "test.one",
                "test.one.effect",
                "test-one",
                "neutral-sample",
            ))
            .expect_err("the Basic module already provides that query");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("query neutral-sample is already provided by lightwell.basic"),
            "{error}"
        );
        // An action may still be named after a query of another module: different namespaces.
        registry
            .register(with_query(
                "test.two",
                "test.two.effect",
                "neutral-sample",
                "test-query",
            ))
            .expect("an action named after another module's query is free");
        assert_eq!(
            registry
                .query("test-query")
                .expect("the new query")
                .0
                .descriptor()
                .id,
            "test.two"
        );
        assert_eq!(
            registry
                .query("neutral-sample")
                .expect("still the Basic module's")
                .0
                .descriptor()
                .id,
            "lightwell.basic"
        );
    }

    /// A descriptor carrying queries and a sample-apply canvas round-trips through JSON, so a
    /// client reads exactly what the registry validated.
    #[test]
    fn queries_and_the_sample_apply_canvas_survive_a_json_round_trip() {
        let registry = ModuleRegistry::builtin();
        let basic = registry
            .effect(BASIC_EFFECT)
            .expect("the Basic module")
            .0
            .descriptor();
        let encoded = serde_json::to_value(basic).expect("a serializable descriptor");
        assert_eq!(encoded["queries"][0]["id"], json!("neutral-sample"));
        assert_eq!(
            encoded["canvas"],
            json!({
                "kind": "sample-apply",
                "query": "neutral-sample",
                "x": "x",
                "y": "y",
                "action": "set-basic",
                "title": "Neutral picker",
                "shortcut": "W",
            })
        );
        assert_eq!(
            &ModuleDescriptor::parse(&encoded).expect("a valid descriptor"),
            basic
        );
        // A descriptor written before queries existed still reads, with none declared.
        let mut without = encoded.clone();
        let object = without.as_object_mut().expect("an object");
        object.remove("queries");
        object.insert("canvas".into(), Value::Null);
        assert!(
            serde_json::from_value::<ModuleDescriptor>(without)
                .expect("queries are optional")
                .queries
                .is_empty()
        );
    }

    /// The generic check in front of a patch action: the module is handed exactly the fields the
    /// caller named, with no declared default filled in and no required parameter demanded, and it
    /// merges them over the state it already holds.
    #[test]
    fn a_patch_action_is_checked_field_by_field_and_fills_no_defaults() {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(PatchModule::shared())
            .expect("a patch descriptor is valid");
        let (module, action) = registry.action(PATCH_ACTION).expect("the patch action");
        assert!(action.patch);
        let checked = crate::check_parameters(action, &json!({"red": 12})).expect("one field");
        assert_eq!(
            checked,
            json!({"red": 12}).as_object().unwrap().clone(),
            "only the field that was sent, exactly as it was sent"
        );
        assert_eq!(
            crate::check_parameters(action, &json!({}))
                .expect("an empty patch")
                .len(),
            0,
            "a patch fills no declared default"
        );
        for (case, sent, fragment) in [
            (
                "unknown field",
                json!({"blue": 1}),
                "unknown parameter blue",
            ),
            (
                "out of range",
                json!({"red": 300}),
                "parameter red must be a number within 0..=255",
            ),
            (
                "wrong kind",
                json!({"red": "12"}),
                "parameter red must be a number",
            ),
        ] {
            let error = crate::check_parameters(action, &sent).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        // The module merges what it was handed over what it already stores.
        let input = ActionInput {
            action_id: PATCH_ACTION.into(),
            parameters: checked,
        };
        let parsed = module.parse(PATCH_ACTION, &input.parameters).unwrap();
        assert_eq!(parsed.parameters, input.parameters);
        assert_eq!(
            module
                .values(PATCH_EFFECT, EFFECT_FORMAT, &json!({"red": 12.0}))
                .unwrap(),
            json!({"red": 12.0, "green": 0.0})
                .as_object()
                .unwrap()
                .clone(),
            "a stored layer reports every parameter it represents, neutral fields included"
        );
        assert_eq!(
            module.label(&parsed).as_deref(),
            Some("Patch red 12"),
            "one changed field labels its own entry"
        );
        assert_eq!(
            module.label(&ActionInput {
                action_id: PATCH_ACTION.into(),
                parameters: json!({"red": 1, "green": 2}).as_object().unwrap().clone(),
            }),
            None,
            "a module that has nothing to add leaves the label to the host"
        );
    }

    #[test]
    fn built_in_modules_describe_their_stored_layers() {
        let registry = ModuleRegistry::builtin();
        let described = |layer: &Layer| -> String {
            let (module, _) = registry.effect(&layer.effect_id).expect("a provider");
            module
                .describe_layer(&layer.effect_id, layer.effect_format, &layer.payload)
                .expect("a stored payload")
        };
        assert_eq!(
            described(&Layer::pixel(3, 4, [1, 2, 3])),
            "Pixel 3, 4 → 1,2,3"
        );
        // An orientation layer holds a composed state, so its row names the orientation it is in,
        // not the actions that reached it. All eight are named and the neutral one says so.
        let orientation = |mirror: bool, turns: u8| -> String {
            described(&Layer::orientation(Orientation { mirror, turns }))
        };
        assert_eq!(orientation(false, 0), "Upright");
        assert_eq!(orientation(false, 1), "Rotate right");
        assert_eq!(orientation(false, 2), "Rotate 180°");
        assert_eq!(orientation(false, 3), "Rotate left");
        assert_eq!(orientation(true, 0), "Mirror horizontal");
        assert_eq!(orientation(true, 1), "Mirror horizontal · Rotate right");
        assert_eq!(orientation(true, 2), "Flip vertical");
        assert_eq!(orientation(true, 3), "Mirror horizontal · Rotate left");
        assert_eq!(
            described(&Layer::crop(crate::CropPayload::NEUTRAL)),
            "Whole image"
        );
    }

    /// Whether a stored layer changes nothing is its module's answer, for every module with a
    /// neutral form: a field patch at its neutral values however they are spelled (the vignette's
    /// is any shape at amount 0), a whole-image crop, the identity orientation and a RAW
    /// development at As shot and 0 EV. A pixel replacement has no neutral form, and a layer whose
    /// provider is missing or unavailable, or whose payload cannot be read, is never neutral.
    #[test]
    fn a_layer_is_neutral_by_its_own_modules_rule() {
        let registry = ModuleRegistry::builtin();
        let layer = |effect: &str, payload: Value| Layer::new(effect, payload);
        let as_shot = crate::RawPayload::for_as_shot([2.0, 1.0, 1.5], [[0.5; 3]; 4]).unwrap();
        let cases = [
            (layer(BASIC_EFFECT, json!({})), true),
            (
                layer(BASIC_EFFECT, json!({"exposure": 0.0, "tint": 0})),
                true,
            ),
            (layer(BASIC_EFFECT, json!({"exposure": 0.5})), false),
            (layer(crate::PRESENCE_EFFECT, json!({"texture": 0})), true),
            (layer(crate::PRESENCE_EFFECT, json!({"dehaze": -3})), false),
            (layer(crate::MIXER_EFFECT, json!({"red-hue": 0})), true),
            (
                layer(crate::MIXER_EFFECT, json!({"aqua-luminance": 12})),
                false,
            ),
            (layer(crate::VIGNETTE_EFFECT, json!({})), true),
            (
                layer(
                    crate::VIGNETTE_EFFECT,
                    json!({"midpoint": 60, "feather": 0}),
                ),
                true,
            ),
            (layer(crate::VIGNETTE_EFFECT, json!({"amount": -10})), false),
            (Layer::crop(CropPayload::NEUTRAL), true),
            (
                Layer::crop(CropPayload {
                    angle: 0.0,
                    x: 0.1,
                    y: 0.1,
                    width: 0.5,
                    height: 0.5,
                }),
                false,
            ),
            (
                Layer::crop(CropPayload {
                    angle: 2.0,
                    ..CropPayload::NEUTRAL
                }),
                false,
            ),
            (Layer::orientation(Orientation::NEUTRAL), true),
            (
                Layer::orientation(Orientation {
                    mirror: true,
                    turns: 0,
                }),
                false,
            ),
            (as_shot.layer(LayerId::new()), true),
            (
                crate::RawPayload {
                    exposure_ev: 0.25,
                    ..as_shot.clone()
                }
                .layer(LayerId::new()),
                false,
            ),
            (Layer::pixel(0, 0, [1, 2, 3]), false),
            (layer(BASIC_EFFECT, json!({"gamma": 1})), false),
            (layer("test.nobody", json!({})), false),
        ];
        for (layer, neutral) in cases {
            assert_eq!(
                registry.layer_neutral(&layer),
                neutral,
                "{} {}",
                layer.effect_id,
                layer.payload
            );
        }
        let mut unavailable = ModuleRegistry::new();
        unavailable
            .register_unavailable(Arc::new(super::BasicModule::new()), "switched off")
            .unwrap();
        assert!(!unavailable.layer_neutral(&layer(BASIC_EFFECT, json!({}))));
    }

    /// One list of built-in modules serves every registry, and registering one of them unavailable
    /// keeps everything it declares, with the reason on its availability: its action is refused by
    /// name and a stack holding its effect is reported rather than rendered without it.
    #[test]
    fn a_built_in_registered_unavailable_keeps_its_declarations_and_reports_why() {
        let ids = |descriptors: Vec<&ModuleDescriptor>| {
            descriptors
                .iter()
                .map(|descriptor| descriptor.id.clone())
                .collect::<Vec<_>>()
        };
        let listed: Vec<Arc<dyn ToolModule>> = builtin_modules();
        assert_eq!(
            ids(ModuleRegistry::builtin().descriptors()),
            ids(listed.iter().map(|module| module.descriptor()).collect()),
        );

        let mut registry = ModuleRegistry::new();
        for module in builtin_modules() {
            if module.descriptor().id == "lightwell.basic" {
                registry.register_unavailable(module, "switched off")
            } else {
                registry.register(module)
            }
            .unwrap();
        }
        let (basic, _) = registry
            .action("set-basic")
            .expect("the action stays declared");
        assert_eq!(
            basic.descriptor().availability,
            Availability::Unavailable {
                reason: "switched off".into()
            }
        );
        let mut expected = serde_json::to_value(super::BasicModule::new().descriptor()).unwrap();
        expected["availability"] = json!({"kind": "unavailable", "reason": "switched off"});
        assert_eq!(
            serde_json::to_value(basic.descriptor()).unwrap(),
            expected,
            "every declaration but availability is the module's own"
        );
        let error = registry
            .compile(
                64,
                48,
                &Recipe {
                    layers: vec![basic_layer()],
                    ..Recipe::default()
                },
            )
            .err()
            .expect("an unavailable effect never compiles");
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(
            error
                .detail
                .starts_with("unavailable effect lightwell.basic.adjust"),
            "{error}"
        );
    }

    #[test]
    fn an_unavailable_provider_keeps_its_identity_and_fails_evaluation_with_its_layers() {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(TestModule::shared(
                "test.module",
                "test.effect",
                "test-action",
                Availability::Unavailable {
                    reason: "not built in this configuration".into(),
                },
            ))
            .unwrap();
        assert!(
            registry.effect("test.effect").is_some(),
            "an unavailable provider keeps its effect identity"
        );
        let first = test_layer("test.effect");
        let second = test_layer("test.effect");
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![
                first.clone(),
                Layer::orientation(Orientation {
                    mirror: false,
                    turns: 1,
                }),
                second.clone(),
            ],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let expected = format!(
            "unavailable effect test.effect (layers {}, {})",
            first.id, second.id
        );
        for error in [
            registry.validate_recipe(&recipe).unwrap_err(),
            registry.validate_layer(&first).unwrap_err(),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
        }
        assert_eq!(
            registry.validate_recipe(&recipe).unwrap_err().detail,
            expected
        );
        assert_eq!(
            render(&registry, &source(), SnapshotId::new(), &recipe)
                .unwrap_err()
                .detail,
            expected
        );
        let missing = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![test_layer("test.absent")],
            masks: Vec::new(),
            ..Recipe::default()
        };
        assert_eq!(
            registry.validate_recipe(&missing).unwrap_err().detail,
            format!(
                "unavailable effect test.absent (layers {})",
                missing.layers[0].id
            )
        );
        let _ = AssetId::new();
    }

    /// Current shapes only: the retired per-action transform effect has no provider, so a stack
    /// holding it is refused exactly like any other unavailable effect. Nothing rewrites it, so
    /// the data survives the refusal and the owner can open it with a build that provides it.
    #[test]
    fn a_stack_holding_the_retired_transform_effect_is_refused_without_being_rewritten() {
        let registry = ModuleRegistry::builtin();
        let retired = Layer {
            id: LayerId::new(),
            effect_id: "lightwell.geometry.transform".into(),
            effect_format: EFFECT_FORMAT,
            payload: json!("rotate-right"),
            mask: None,
            artifacts: Vec::new(),
        };
        assert!(registry.effect("lightwell.geometry.transform").is_none());
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![retired.clone()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let expected = format!(
            "unavailable effect lightwell.geometry.transform (layers {})",
            retired.id
        );
        for error in [
            registry.validate_recipe(&recipe).unwrap_err(),
            registry.validate_layer(&retired).unwrap_err(),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, expected);
        }
        assert_eq!(recipe.layers, vec![retired], "the refused stack is kept");
    }

    /// A mask reaches a layer only where the layer's input is the content stage the mask is stored
    /// in. The stage comes from the effect's provider, so the rule is the registry's and it holds
    /// wherever a recipe is validated or compiled.
    #[test]
    fn a_mask_may_only_reach_a_layer_before_the_geometry_tail() {
        let registry = staged_registry();
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
        ));
        let masked = |layer: Layer| Layer {
            mask: Some(mask.id.clone()),
            ..layer
        };
        let recipe = |layer: Layer| Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        // A colour-stage layer addresses the content stage, so it may carry one.
        let colour = recipe(masked(test_layer(MIXER_EFFECT)));
        registry.validate_recipe(&colour).unwrap();
        registry.compile(2, 1, &colour).unwrap();
        for (layer, stage) in [
            (Layer::orientation(Orientation::NEUTRAL), "geometry"),
            (test_layer(FINISH_EFFECT), "finish"),
        ] {
            let refused = recipe(masked(layer.clone()));
            let expected = format!(
                "layer {} carries a mask, which a {stage} effect cannot: a mask is stored in \
                 content-stage coordinates",
                refused.layers[0].id
            );
            for error in [
                registry.validate_recipe(&refused).unwrap_err(),
                registry
                    .compile(2, 1, &refused)
                    .err()
                    .expect("a masked layer at the geometry tail never compiles"),
            ] {
                assert_eq!(error.kind, ErrorKind::Validation);
                assert_eq!(error.detail, expected);
            }
            // Unmasked, the same layer is the ordinary stack it always was.
            registry.validate_recipe(&recipe(layer)).unwrap();
        }
    }

    /// One `add` linear gradient at full amount, over the whole frame: the mask every test below
    /// attaches to a layer.
    fn gradient_mask(name: &str) -> Mask {
        let mut mask = Mask::new(name);
        let component = mask.next_component_name("linear");
        mask.components.push(Component::new(
            component,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
        ));
        mask
    }

    fn basic_layer() -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": 1.0}),
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn bound(layer: Layer, mask: &Mask) -> Layer {
        Layer {
            mask: Some(mask.id.clone()),
            ..layer
        }
    }

    /// A mask is stored in content-stage coordinates, so an effect whose input is not that stage
    /// cannot declare itself maskable: the descriptor is refused at registration, by name, rather
    /// than carrying a flag nothing could honour.
    #[test]
    fn a_maskable_effect_is_refused_at_the_geometry_and_finish_stages() {
        let descriptor = |stage: EffectStage| ModuleDescriptor {
            id: "test.maskable".into(),
            title: "Maskable".into(),
            hint: None,
            effects: vec![EffectDescriptor {
                id: "test.maskable.effect".into(),
                format: EFFECT_FORMAT,
                stage,
                order: 0,
                maskable: true,
                artifacts: false,
                single: false,
            }],
            actions: Vec::new(),
            queries: Vec::new(),
            controls: Vec::new(),
            reset: None,
            canvas: None,
            developer: false,
            collapsed: false,
            layout: crate::ModuleLayout::Stacked,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        };
        for stage in [EffectStage::Geometry, EffectStage::Finish] {
            let error = ModuleRegistry::new()
                .register(TestModule::from_descriptor(descriptor(stage)))
                .expect_err("a maskable effect at the geometry tail");
            assert_eq!(error.kind, ErrorKind::Validation);
            assert_eq!(
                error.detail,
                format!(
                    "effect test.maskable.effect declares maskable at the {} stage, which a mask \
                     stored in content-stage coordinates cannot reach",
                    stage.as_str()
                )
            );
        }
        for stage in [
            EffectStage::Source,
            EffectStage::Pixel,
            EffectStage::Color,
            EffectStage::Spatial,
        ] {
            ModuleRegistry::new()
                .register(TestModule::from_descriptor(descriptor(stage)))
                .expect("every content-stage effect may declare it");
        }
    }

    /// The three effects the design names declare the flag and nothing else does, and the field
    /// follows the module that declares one: `edit.set-basic` accepts a target, `edit.set-crop` does
    /// not, and a client reads both from the registry rather than from a hand-maintained list.
    #[test]
    fn exactly_the_three_delivered_effects_are_maskable() {
        let registry = ModuleRegistry::builtin();
        for effect in [BASIC_EFFECT, crate::MIXER_EFFECT, crate::PRESENCE_EFFECT] {
            assert!(registry.effect_maskable(effect), "{effect}");
        }
        for effect in [
            PIXEL_EFFECT,
            RAW_EFFECT,
            CROP_EFFECT,
            ORIENTATION_EFFECT,
            crate::VIGNETTE_EFFECT,
            "test.absent",
        ] {
            assert!(!registry.effect_maskable(effect), "{effect}");
        }
        for action in [
            "set-basic",
            "reset-basic",
            "set-mixer",
            "reset-mixer",
            "set-presence",
            "reset-presence",
        ] {
            assert!(registry.action_accepts_mask(action), "{action}");
        }
        for action in [
            "set-pixel",
            "set-crop",
            "rotate-left",
            "set-vignette",
            "set-raw",
            "no-such-action",
        ] {
            assert!(!registry.action_accepts_mask(action), "{action}");
        }
    }

    /// A declared `single` effect is one layer per target: the global layer and each mask are
    /// distinct targets, so one effect may hold one layer in each and two layers with the *same*
    /// target are still the ambiguity the host refuses without rewriting anything.
    #[test]
    fn one_layer_per_target_is_what_single_layer_means() {
        let registry = ModuleRegistry::builtin();
        let first = gradient_mask("Mask 1");
        let second = gradient_mask("Mask 2");
        let recipe = |layers: Vec<Layer>| Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks: vec![first.clone(), second.clone()],
            ..Recipe::default()
        };
        // The global layer and one layer per mask: three layers of one single-layer effect, legal.
        let legal = recipe(vec![
            basic_layer(),
            bound(basic_layer(), &first),
            bound(basic_layer(), &second),
        ]);
        registry.compile(64, 48, &legal).expect("one per target");
        // Two layers of one effect with the same target, global or masked, is the old refusal.
        for (case, layers) in [
            ("two global layers", vec![basic_layer(), basic_layer()]),
            (
                "two layers in one mask",
                vec![bound(basic_layer(), &first), bound(basic_layer(), &first)],
            ),
        ] {
            let error = registry
                .compile(64, 48, &recipe(layers))
                .err()
                .expect("ambiguous layers");
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert_eq!(error.detail, "ambiguous Basic layers", "{case}");
        }
    }

    /// The one lookup of a module's own layer answers per target exactly as the compile refuses per
    /// target: a masked layer of a maskable effect belongs only to its mask, a layer of an effect
    /// that is not maskable belongs to every target, and two layers for one target are refused with
    /// the compile's own words, whatever else the stack holds.
    #[test]
    fn a_modules_own_layer_is_found_for_its_target_only() {
        let registry = ModuleRegistry::builtin();
        let first = gradient_mask("Mask 1");
        let second = gradient_mask("Mask 2");
        let global = basic_layer();
        let in_first = bound(basic_layer(), &first);
        let crop = Layer::crop(CropPayload::NEUTRAL);
        let stack = [
            Layer::pixel(0, 0, [1, 2, 3]),
            global.clone(),
            in_first.clone(),
            crop.clone(),
        ];
        let found = |target: Option<&MaskId>, effect: &str| {
            registry
                .own_layer(&stack, effect, target)
                .unwrap()
                .map(|(index, layer)| (index, layer.id.clone()))
        };
        assert_eq!(found(None, BASIC_EFFECT), Some((1, global.id.clone())));
        assert_eq!(
            found(Some(&first.id), BASIC_EFFECT),
            Some((2, in_first.id.clone()))
        );
        assert_eq!(found(Some(&second.id), BASIC_EFFECT), None);
        for target in [None, Some(&first.id)] {
            assert_eq!(
                found(target, CROP_EFFECT),
                Some((3, crop.id.clone())),
                "an effect that is not maskable belongs to every target"
            );
        }
        let ambiguous = registry
            .own_layer(&[global.clone(), basic_layer()], BASIC_EFFECT, None)
            .expect_err("two global layers");
        assert_eq!(ambiguous.kind, ErrorKind::Validation);
        assert_eq!(ambiguous.detail, "ambiguous Basic layers");
        assert!(
            registry
                .own_layer(&[global, in_first], BASIC_EFFECT, None)
                .is_ok(),
            "a global and a masked layer are two targets, not an ambiguity"
        );
        // Every built-in module that owns one layer declares it; the transform's orientation does
        // not, because a fold leaves a neutral orientation stored after the crop beside the one ahead.
        for effect in [
            BASIC_EFFECT,
            crate::MIXER_EFFECT,
            crate::PRESENCE_EFFECT,
            crate::VIGNETTE_EFFECT,
            CROP_EFFECT,
        ] {
            assert!(registry.effect_single(effect), "{effect}");
        }
        for effect in [crate::PIXEL_EFFECT, ORIENTATION_EFFECT, crate::RAW_EFFECT] {
            assert!(!registry.effect_single(effect), "{effect}");
        }
    }

    /// The order a person can see: a masked layer of an effect follows the global layer of that
    /// effect and the masked layers of earlier masks, and nothing else moves.
    #[test]
    fn a_masked_layer_is_placed_after_the_global_layer_and_by_its_masks_index() {
        let registry = ModuleRegistry::builtin();
        let first = gradient_mask("Mask 1");
        let second = gradient_mask("Mask 2");
        let third = gradient_mask("Mask 3");
        let masks = vec![first.clone(), second.clone(), third.clone()];
        let global = basic_layer();
        let in_first = bound(basic_layer(), &first);
        let in_third = bound(basic_layer(), &third);
        let crop = Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        for (case, layers, target, expected) in [
            ("an empty stack", vec![], Some(&first), 0),
            ("a global layer only", vec![global.clone()], Some(&first), 1),
            (
                "after the global layer and before the tail",
                vec![global.clone(), crop.clone()],
                Some(&second),
                1,
            ),
            (
                "between two masks",
                vec![global.clone(), in_first.clone(), in_third.clone()],
                Some(&second),
                2,
            ),
            (
                "before a later mask",
                vec![global.clone(), in_third.clone()],
                Some(&first),
                1,
            ),
            (
                "after an earlier mask",
                vec![global.clone(), in_first.clone()],
                Some(&third),
                2,
            ),
            (
                // The global layer keeps its own placement, and never lands after a masked layer of
                // its own effect.
                "the global layer under existing masked layers",
                vec![in_first.clone(), in_third.clone()],
                None,
                0,
            ),
            (
                "the global layer of an effect with none",
                vec![crop.clone()],
                None,
                0,
            ),
        ] {
            assert_eq!(
                registry.insertion_index_for_target(
                    &layers,
                    BASIC_EFFECT,
                    target.map(|mask| &mask.id),
                    &masks,
                ),
                expected,
                "{case}"
            );
        }
        // An unmasked target is placed exactly where the stage rule alone places it, so nothing about
        // masking moves an ordinary layer.
        for layers in [
            vec![],
            vec![crop.clone()],
            vec![global.clone(), crop.clone()],
        ] {
            assert_eq!(
                registry.insertion_index_for_target(&layers, BASIC_EFFECT, None, &masks),
                registry.insertion_index_for(&layers, BASIC_EFFECT),
                "the global target follows the delivered placement rule"
            );
        }
    }

    /// Reordering the mask list re-sorts exactly the masked layers of each effect, in the positions
    /// they already occupy, and moves nothing else.
    #[test]
    fn sorting_masked_layers_moves_only_them() {
        let registry = ModuleRegistry::builtin();
        let first = gradient_mask("Mask 1");
        let second = gradient_mask("Mask 2");
        let crop = Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        let global = basic_layer();
        let in_first = bound(basic_layer(), &first);
        let in_second = bound(basic_layer(), &second);
        let mut layers = vec![
            Layer::pixel(0, 0, [1, 2, 3]),
            global.clone(),
            in_first.clone(),
            in_second.clone(),
            crop.clone(),
        ];
        let identities = |layers: &[Layer]| {
            layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>()
        };
        let before = identities(&layers);
        // The mask list as it stands: the stack is already sorted, so nothing moves.
        registry.sort_masked_layers(&mut layers, &[first.clone(), second.clone()]);
        assert_eq!(identities(&layers), before, "an already ordered stack");
        // The masks swap places, and with them exactly the two masked layers.
        registry.sort_masked_layers(&mut layers, &[second.clone(), first.clone()]);
        assert_eq!(
            identities(&layers),
            vec![
                before[0].clone(),
                global.id.clone(),
                in_second.id.clone(),
                in_first.id.clone(),
                crop.id.clone(),
            ],
            "only the masked layers moved"
        );
        // A stack with no masked layers is untouched, and so is a stack whose masks are unchanged.
        let mut unmasked = vec![Layer::pixel(0, 0, [1, 2, 3]), global.clone(), crop.clone()];
        let before = identities(&unmasked);
        registry.sort_masked_layers(&mut unmasked, &[first, second]);
        assert_eq!(identities(&unmasked), before);
    }

    /// A mask reaches a colour operation and a spatial operation, so a layer that carries one and
    /// compiles into anything else is refused by name on every path that would have to draw it.
    /// Refusing is what keeps such a layer from being committed at all — the host compiles a stack
    /// before it persists one — and is the alternative to the silent omission of rendering it as if it
    /// applied everywhere.
    ///
    /// The live case is a point replacement: no delivered pixel effect declares itself maskable, and
    /// the recipe model lets a stored layer of one carry a mask, so this refusal is what a stack like
    /// that meets. A geometry or finish layer is refused for the earlier reason, that it has no
    /// content stage to read a mask in, and that refusal is asserted below too so the two cannot both
    /// be removed by accident.
    #[test]
    fn a_mask_this_build_cannot_evaluate_is_refused_by_name() {
        let registry = ModuleRegistry::builtin();
        let mask = gradient_mask("Mask 1");
        let recipe = |layer: Layer| Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let layer = Layer::pixel(1, 1, [9, 9, 9]);
        // Unmasked, the same layer compiles as it always has.
        registry
            .compile(256, 256, &recipe(layer.clone()))
            .unwrap_or_else(|error| panic!("an unmasked pixel layer: {error:?}"));
        let refused = recipe(bound(layer, &mask));
        let error = registry
            .compile(256, 256, &refused)
            .err()
            .expect("a masked pixel layer");
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            format!(
                "layer {} carries a mask on the pixel effect {}, and this build evaluates a \
                 mask only on a colour-stage or spatial-stage effect",
                refused.layers[0].id, refused.layers[0].effect_id
            )
        );
        // The refusal reads the stack; it rewrites nothing.
        assert!(refused.layers[0].mask.is_some());

        // The stage rule still refuses the two stages that have no content stage to read a mask in.
        for (stage, layer) in [
            (
                "geometry",
                Layer::orientation(Orientation {
                    mirror: false,
                    turns: 1,
                }),
            ),
            (
                "finish",
                Layer {
                    id: LayerId::new(),
                    effect_id: crate::VIGNETTE_EFFECT.into(),
                    effect_format: EFFECT_FORMAT,
                    payload: json!({"amount": -40.0}),
                    mask: None,
                    artifacts: Vec::new(),
                },
            ),
        ] {
            let refused = recipe(bound(layer, &mask));
            let error = registry
                .compile(256, 256, &refused)
                .err()
                .unwrap_or_else(|| panic!("a masked {stage} layer compiled"));
            assert_eq!(error.kind, ErrorKind::Validation, "{stage}");
            assert_eq!(
                error.detail,
                format!(
                    "layer {} carries a mask, which a {stage} effect cannot: a mask is stored in \
                     content-stage coordinates",
                    refused.layers[0].id
                ),
                "{stage}"
            );
            assert!(refused.layers[0].mask.is_some(), "{stage}");
        }
    }

    /// A masked spatial layer compiles, because the masked spatial primitive is delivered: the mask
    /// is attached to the operation the module returned, against the stage the layer receives, which
    /// for a stage boundary is also the frame it reads and writes.
    #[test]
    fn a_masked_spatial_layer_compiles_with_its_mask_attached() {
        let registry = ModuleRegistry::builtin();
        let mask = gradient_mask("Mask 1");
        let presence = Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"clarity": 40.0}),
            mask: None,
            artifacts: Vec::new(),
        };
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![bound(presence, &mask)],
            masks: vec![mask],
            ..Recipe::default()
        };
        let compiled = registry
            .compile(256, 200, &recipe)
            .unwrap_or_else(|error| panic!("a masked spatial layer: {error:?}"));
        let entry = compiled.segments[1]
            .entry
            .as_ref()
            .expect("a spatial entry");
        let Entry::Spatial { operation, .. } = entry else {
            panic!("a spatial entry");
        };
        let attached = operation.mask().expect("the mask is attached");
        // The mask is compiled against the stage the layer receives, which is the frame this
        // operation reads and writes: the tile loop needs no mapping at all.
        assert_eq!(
            attached.stage(),
            Stage {
                width: 256,
                height: 200
            }
        );
        assert_eq!(compiled.segments[0].width, 256);
    }

    /// Each masked spatial layer is a stage boundary and therefore a sequential full frame, so the
    /// design caps them at four. The fifth is a `resource-limit` error naming the limit; nothing is
    /// dropped, reordered or rendered as if it applied everywhere.
    #[test]
    fn a_fifth_masked_spatial_layer_is_a_resource_limit() {
        let registry = ModuleRegistry::builtin();
        let masks: Vec<Mask> = (0..MAX_MASKED_SPATIAL_LAYERS + 1)
            .map(|index| gradient_mask(&format!("Mask {index}")))
            .collect();
        let presence = |mask: &Mask| Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"clarity": 40.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        };
        let recipe = |count: usize| Recipe {
            format: RECIPE_FORMAT,
            layers: masks[..count].iter().map(presence).collect(),
            masks: masks.clone(),
            ..Recipe::default()
        };
        registry
            .compile(128, 128, &recipe(MAX_MASKED_SPATIAL_LAYERS))
            .unwrap_or_else(|error| panic!("four masked spatial layers: {error:?}"));
        let error = registry
            .compile(128, 128, &recipe(MAX_MASKED_SPATIAL_LAYERS + 1))
            .err()
            .expect("five masked spatial layers");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            "this recipe holds 5 masked spatial layers, more than the 4 the host evaluates: each \
             one is a stage boundary and therefore a sequential full frame"
        );
    }

    /// The missing mask is refused wherever a recipe is evaluated, because compiling checks it and
    /// every render, sample and plan compiles.
    #[test]
    fn a_layer_naming_a_mask_the_recipe_does_not_carry_is_refused_by_every_compile() {
        let registry = ModuleRegistry::builtin();
        let mask = Mask::new("Mask 1");
        let layer = Layer {
            mask: Some(mask.id.clone()),
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer.clone()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let expected = format!(
            "layer {} references mask {}, which this recipe does not carry",
            layer.id, mask.id
        );
        for error in [
            registry.validate_recipe(&recipe).unwrap_err(),
            registry
                .compile(2, 1, &recipe)
                .err()
                .expect("a missing mask never compiles"),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
            crate::render::extents(&registry, &source(), &recipe).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, expected);
        }
        assert_eq!(recipe.layers, vec![layer], "the refused stack is kept");
    }

    /// The mask table is checked once, where a recipe enters the service, and compiling trusts it:
    /// a table past a per-recipe limit and a component this build cannot read are refused by the
    /// admission check, while compiling the same stack — whose masks no layer draws — reads none of
    /// the table and answers.
    #[test]
    fn the_mask_table_is_checked_on_admission_and_not_by_compile() {
        let registry = ModuleRegistry::builtin();
        let mut too_many = Recipe::default();
        for index in 0..=crate::MASKS_PER_RECIPE {
            too_many.masks.push(Mask::new(format!("Mask {index}")));
        }
        let mut unreadable = Mask::new("Mask 1");
        let name = unreadable.next_component_name("future-kind");
        unreadable.components.push(Component::new(
            name,
            ComponentMode::Add,
            "future-kind",
            json!({}),
        ));
        let unknown = Recipe {
            masks: vec![unreadable],
            ..Recipe::default()
        };
        for (recipe, kind, detail) in [
            (
                &too_many,
                ErrorKind::ResourceLimit,
                format!(
                    "recipe has {} masks; the limit is {} masks per recipe",
                    crate::MASKS_PER_RECIPE + 1,
                    crate::MASKS_PER_RECIPE
                ),
            ),
            (
                &unknown,
                ErrorKind::Incompatible,
                "unknown mask component future-kind".to_owned(),
            ),
        ] {
            let refused = registry.validate_recipe(recipe).unwrap_err();
            assert_eq!((refused.kind, &refused.detail), (kind, &detail));
            assert_eq!(
                recipe.validate_mask_table().unwrap_err().detail,
                detail,
                "admission's refusal is the table's own"
            );
            registry
                .compile(4, 4, recipe)
                .expect("compiling does not check the table again");
        }
    }

    /// A component of a kind this build cannot evaluate is refused by name where a recipe enters the
    /// service and wherever the mask would have to be drawn, and nothing about the stored mask is
    /// rewritten: the host keeps every byte and says what it could not draw, rather than rendering
    /// the layer unmasked or dropping the component. The model's structural check never asks what a
    /// kind means, so the stack still reads and round-trips.
    #[test]
    fn a_component_kind_this_build_does_not_know_is_refused_by_every_compile() {
        let registry = ModuleRegistry::builtin();
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("future-kind");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "future-kind",
            json!({"nested": {"points": [[0.25, 0.5], [0.75, 0.5]]}, "flag": true, "n": 3.5}),
        ));
        // A layer that draws the mask: a non-neutral colour layer bound to it.
        let layer = Layer {
            mask: Some(mask.id.clone()),
            ..Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.5}))
        };
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers: vec![layer],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        // Admission refuses it once, with the kind table's own words.
        let admitted = registry.validate_recipe(&recipe).unwrap_err();
        assert_eq!(admitted.kind, ErrorKind::Incompatible);
        assert_eq!(admitted.detail, "unknown mask component future-kind");
        for error in [
            registry
                .compile(2, 1, &recipe)
                .err()
                .expect("an unknown component kind never compiles"),
            render(&registry, &source(), SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source(), &recipe, 0, 0).unwrap_err(),
            crate::render::extents(&registry, &source(), &recipe).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, "unknown mask component future-kind");
        }
        // The structural model never asks what a kind means, so the refused stack still reads back
        // byte for byte through the persisted shape.
        recipe.validate().unwrap();
        let reopened: Recipe =
            serde_json::from_slice(&serde_json::to_vec(&recipe).unwrap()).unwrap();
        assert_eq!(reopened, recipe, "the refused stack is kept");
        assert_eq!(reopened.masks[0].components[0].kind, "future-kind");
    }

    #[test]
    fn payload_format_and_shape_are_validated_by_the_providing_module() {
        let registry = ModuleRegistry::builtin();
        let wrong_format = Layer {
            effect_format: 99,
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        assert_eq!(
            registry.validate_layer(&wrong_format).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let wrong_payload = Layer {
            payload: json!({"x": 1}),
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        assert_eq!(
            registry.validate_layer(&wrong_payload).unwrap_err().kind,
            ErrorKind::Validation
        );
        for wrong_orientation in [
            json!("rotate-sideways"),
            json!({"mirror": false, "turns": 4}),
            json!({"mirror": false, "turns": 0, "flip": true}),
        ] {
            let layer = Layer {
                payload: wrong_orientation.clone(),
                ..Layer::orientation(Orientation::NEUTRAL)
            };
            assert_eq!(
                registry.validate_layer(&layer).unwrap_err().kind,
                ErrorKind::Validation,
                "{wrong_orientation}"
            );
        }
        assert!(
            registry
                .validate_layer(&Layer::orientation(Orientation::NEUTRAL))
                .is_ok()
        );
        assert!(
            registry
                .validate_layer(&Layer::pixel(0, 0, [1, 2, 3]))
                .is_ok()
        );
    }

    #[test]
    fn a_pixel_layer_joins_the_stack_before_the_first_geometry_layer() {
        let registry = ModuleRegistry::builtin();
        let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
        let turn = || {
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            })
        };
        let crop = || {
            Layer::crop(CropPayload {
                angle: 0.0,
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            })
        };
        // `geometry` is where a default-order geometry layer such as the orientation goes: ahead
        // of the crop, whose own order is later, and otherwise at the end of the tail.
        for (case, layers, expected, geometry) in [
            ("an empty stack", vec![], 0, 0),
            ("geometry only", vec![turn(), crop()], 0, 1),
            ("pixels only", vec![pixel(), pixel()], 2, 2),
            // A stored stack with a turn after the crop keeps its order.
            (
                "a pixel before the tail",
                vec![pixel(), crop(), turn()],
                1,
                1,
            ),
            (
                // Such a stack renders as it always did; a new edit still joins the content stage.
                "an interleaved pixel after geometry",
                vec![pixel(), turn(), pixel(), crop()],
                1,
                3,
            ),
            (
                "a layer no provider declares does not open the tail",
                vec![test_layer("test.absent"), turn()],
                1,
                2,
            ),
        ] {
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Pixel, 0),
                expected,
                "{case}"
            );
            // A colour-stage layer joins the stack by the same rule, so a Basic layer lands before
            // the quarter-turns, reflections and crop that carry it.
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Color, 0),
                expected,
                "{case}: colour joins where a pixel edit does"
            );
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Geometry, 0),
                geometry,
                "{case}: the orientation goes ahead of the crop"
            );
            assert_eq!(
                registry.insertion_index_for(&layers, ORIENTATION_EFFECT),
                geometry,
                "{case}: the orientation effect's own placement"
            );
            assert_eq!(
                registry.insertion_index_for(&layers, CROP_EFFECT),
                layers.len(),
                "{case}: a crop ends the tail"
            );
            // A client reading the same rule from its module list places every effect alike.
            let listed: Vec<ModuleDescriptor> =
                registry.descriptors().into_iter().cloned().collect();
            for effect in [
                PIXEL_EFFECT,
                ORIENTATION_EFFECT,
                CROP_EFFECT,
                crate::VIGNETTE_EFFECT,
                "test.absent",
            ] {
                assert_eq!(
                    insertion_index_among(&listed, &layers, effect),
                    registry.insertion_index_for(&layers, effect),
                    "{case}: {effect} from the module list"
                );
            }
            // The host reads the same rule from an effect's own descriptor.
            assert_eq!(
                registry.insertion_index_for(&layers, PIXEL_EFFECT),
                expected,
                "{case}: the pixel effect's own placement"
            );
        }
        assert_eq!(
            registry.effect_stage(PIXEL_EFFECT),
            Some(EffectStage::Pixel)
        );
        assert_eq!(
            registry.effect_stage(CROP_EFFECT),
            Some(EffectStage::Geometry)
        );
        assert_eq!(registry.effect_stage("test.absent"), None);
    }

    pub(crate) const MIXER_EFFECT: &str = "test.mixer.effect";
    pub(crate) const SPATIAL_EFFECT: &str = "test.spatial.effect";
    pub(crate) const FINISH_EFFECT: &str = "test.finish.effect";

    /// The built-ins plus one colour effect of order 10, one spatial effect and one finish effect,
    /// which is every stage and two orders within the colour stage.
    pub(crate) fn staged_registry() -> ModuleRegistry {
        let mut registry = ModuleRegistry::builtin();
        for (id, effect, action, stage, order) in [
            (
                "test.mixer",
                MIXER_EFFECT,
                // Distinct from the real mixer module's own "set-mixer" action, which
                // `ModuleRegistry::builtin()` now registers.
                "set-test-mixer",
                EffectStage::Color,
                10,
            ),
            (
                "test.spatial",
                SPATIAL_EFFECT,
                "set-spatial",
                EffectStage::Spatial,
                0,
            ),
            (
                "test.finish",
                FINISH_EFFECT,
                "set-finish",
                EffectStage::Finish,
                0,
            ),
        ] {
            registry
                .register(StageModule::shared(id, effect, action, stage, order))
                .expect("a valid test module");
        }
        registry
    }

    /// Every stage's region, over stacks that mix them all: a pixel or colour layer joins the
    /// content region before the first spatial, geometry or finish layer, a spatial layer follows
    /// the pointwise work and precedes the tail, a geometry layer precedes the first finish layer
    /// and a finish layer goes last, with a leading source layer always keeping index zero.
    #[test]
    fn every_stage_joins_the_region_the_placement_table_names() {
        let registry = staged_registry();
        let source = || test_layer(RAW_EFFECT);
        let pixel = || Layer::pixel(0, 0, [1, 2, 3]);
        let basic = || test_layer(BASIC_EFFECT);
        let mixer = || test_layer(MIXER_EFFECT);
        let spatial = || test_layer(SPATIAL_EFFECT);
        let turn = || {
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            })
        };
        let finish = || test_layer(FINISH_EFFECT);
        // (case, stack, pixel, colour order 0, colour order 10, spatial, geometry, finish)
        for (case, layers, expected) in [
            ("an empty stack", vec![], [0, 0, 0, 0, 0, 0]),
            (
                "the canonical stack of every stage",
                vec![
                    source(),
                    pixel(),
                    basic(),
                    mixer(),
                    spatial(),
                    turn(),
                    finish(),
                ],
                [4, 3, 4, 5, 6, 7],
            ),
            ("a source layer alone", vec![source()], [1, 1, 1, 1, 1, 1]),
            (
                "the geometry tail only",
                vec![turn(), turn()],
                [0, 0, 0, 0, 2, 2],
            ),
            (
                "a finish layer over the tail",
                vec![turn(), finish()],
                [0, 0, 0, 0, 1, 2],
            ),
            (
                "a spatial layer before the tail",
                vec![basic(), spatial(), turn()],
                [1, 1, 1, 2, 3, 3],
            ),
            // The colour region is read in order: an order-0 layer goes before an order-10 one
            // whichever was committed first, and neither existing layer moves.
            (
                "one colour layer of order 10",
                vec![mixer()],
                [1, 0, 1, 1, 1, 1],
            ),
            (
                "one colour layer of order 0",
                vec![basic()],
                [1, 1, 1, 1, 1, 1],
            ),
            (
                "both colour orders before the tail",
                vec![basic(), mixer(), turn()],
                [2, 1, 2, 2, 3, 3],
            ),
            (
                // A stored stack the host did not build keeps every layer where it is; the region
                // still ends at the first layer of a later stage.
                "a pixel layer stored after the tail",
                vec![turn(), pixel()],
                [0, 0, 0, 0, 2, 2],
            ),
        ] {
            let placement = [
                registry.insertion_index(&layers, EffectStage::Pixel, 0),
                registry.insertion_index(&layers, EffectStage::Color, 0),
                registry.insertion_index(&layers, EffectStage::Color, 10),
                registry.insertion_index(&layers, EffectStage::Spatial, 0),
                registry.insertion_index(&layers, EffectStage::Geometry, 0),
                registry.insertion_index(&layers, EffectStage::Finish, 0),
            ];
            assert_eq!(placement, expected, "{case}");
            assert_eq!(
                registry.insertion_index(&layers, EffectStage::Source, 0),
                0,
                "{case}: a source layer prepares the content stage"
            );
            // The same answers through the effects' own descriptors.
            assert_eq!(
                [
                    registry.insertion_index_for(&layers, PIXEL_EFFECT),
                    registry.insertion_index_for(&layers, BASIC_EFFECT),
                    registry.insertion_index_for(&layers, MIXER_EFFECT),
                    registry.insertion_index_for(&layers, SPATIAL_EFFECT),
                    registry.insertion_index_for(&layers, CROP_EFFECT),
                    registry.insertion_index_for(&layers, FINISH_EFFECT),
                ],
                expected,
                "{case}: read from each effect's descriptor"
            );
        }

        // Committing the two colour orders in either sequence leaves the same stack.
        let mut committed_low_first = vec![basic()];
        committed_low_first.insert(
            registry.insertion_index_for(&committed_low_first, MIXER_EFFECT),
            mixer(),
        );
        let mut committed_high_first = vec![mixer()];
        committed_high_first.insert(
            registry.insertion_index_for(&committed_high_first, BASIC_EFFECT),
            basic(),
        );
        for (case, stack) in [
            ("order 0 first", committed_low_first),
            ("order 10 first", committed_high_first),
        ] {
            assert_eq!(
                stack
                    .iter()
                    .map(|layer| layer.effect_id.as_str())
                    .collect::<Vec<_>>(),
                vec![BASIC_EFFECT, MIXER_EFFECT],
                "{case}: the declared order decides, not the commit sequence"
            );
        }

        // `module.list` reports the stage and the order of every effect.
        let descriptors = serde_json::to_value(registry.descriptors()).expect("descriptor JSON");
        let effect_of = |module: &str| {
            descriptors
                .as_array()
                .expect("an array")
                .iter()
                .find(|descriptor| descriptor["id"] == json!(module))
                .expect("a registered module")["effects"][0]
                .clone()
        };
        assert_eq!(
            effect_of("test.mixer"),
            json!({"id": MIXER_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "order": 10})
        );
        assert_eq!(
            effect_of("test.spatial"),
            json!({"id": SPATIAL_EFFECT, "format": EFFECT_FORMAT, "stage": "spatial", "order": 0})
        );
        assert_eq!(
            effect_of("test.finish"),
            json!({"id": FINISH_EFFECT, "format": EFFECT_FORMAT, "stage": "finish", "order": 0})
        );
    }

    /// The one order the host cannot evaluate: a finish layer is defined in the output coordinates
    /// the geometry tail produced, so a geometry layer after it has no stage to address. The stack
    /// is refused as it stands, and nothing is rewritten, reordered or dropped.
    #[test]
    fn a_finish_layer_before_a_geometry_layer_is_refused_by_compilation() {
        let registry = staged_registry();
        let finish = test_layer(FINISH_EFFECT);
        let turn = Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        });
        let refused = vec![finish.clone(), turn.clone()];
        let error = registry
            .compile_layers(
                2,
                1,
                &refused,
                &[],
                &crate::path::StrokeTable::default(),
                &ArtifactTable::default(),
            )
            .err()
            .expect("a finish layer before geometry never compiles");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.starts_with("finish layer precedes geometry"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains(finish.id.as_str()));
        assert!(error.detail.contains(turn.id.as_str()));
        assert_eq!(refused.len(), 2, "the refused stack is kept as it stands");
        // The order the host does build compiles, and so does a stack with no geometry at all.
        assert!(
            registry
                .compile_layers(
                    2,
                    1,
                    &[turn, finish.clone()],
                    &[],
                    &crate::path::StrokeTable::default(),
                    &ArtifactTable::default(),
                )
                .is_ok()
        );
        assert!(
            registry
                .compile_layers(
                    2,
                    1,
                    &[finish],
                    &[],
                    &crate::path::StrokeTable::default(),
                    &ArtifactTable::default(),
                )
                .is_ok()
        );
    }

    const BOUND_EFFECT: &str = "test.bound.effect";

    /// An identity colour unit that names the artifact it was compiled with, so a compiled stack
    /// shows which artifacts its module received and in what order.
    struct Named(String);

    impl crate::PointwiseColor for Named {
        fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {}
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            self.0.clone()
        }
    }

    /// A colour effect that declares artifacts and compiles one named unit per bound artifact.
    struct BoundModule(ModuleDescriptor);

    impl ToolModule for BoundModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok("bound".into())
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::neutral()))
        }
        fn compile_bound(
            &self,
            _: &str,
            _: u32,
            _: &Value,
            _: Stage,
            artifacts: &[Arc<crate::artifacts::PreparedArtifact>],
        ) -> Result<Processing, Error> {
            Ok(Processing::Color(crate::ColorOperation::new(
                artifacts
                    .iter()
                    .map(|artifact| {
                        Arc::new(Named(artifact.id.to_string())) as Arc<dyn crate::PointwiseColor>
                    })
                    .collect(),
            )))
        }
    }

    /// The built-in providers and [`BoundModule`].
    fn bound_registry() -> ModuleRegistry {
        let descriptor = ModuleDescriptor::parse(&json!({
            "id": "test.bound",
            "title": "Bound",
            "effects": [{"id": BOUND_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "artifacts": true}],
            "actions": [],
            "controls": [],
            "availability": {"kind": "available"},
        }))
        .unwrap();
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(BoundModule(descriptor)))
            .unwrap();
        registry
    }

    /// One byte of verified artifact under an identity that starts with `digit`.
    fn bound_artifact(digit: &str) -> Arc<crate::artifacts::PreparedArtifact> {
        let id = crate::ArtifactId::for_hash(&format!("{digit}{}", "d".repeat(63))).unwrap();
        let meta = crate::artifacts::ArtifactMeta {
            kind: "test".into(),
            width: None,
            height: None,
            colour: None,
        };
        Arc::new(crate::artifacts::PreparedArtifact::new(
            id,
            &meta,
            vec![0].into(),
        ))
    }

    #[test]
    fn compile_binds_artifacts_in_listed_order_and_refuses_unbound_ones() {
        let registry = bound_registry();
        let (first, second) = (bound_artifact("1"), bound_artifact("2"));
        let bound: ArtifactTable = [first.clone(), second.clone()].into_iter().collect();
        let layer = Layer {
            artifacts: vec![second.id.clone(), first.id.clone()],
            ..test_layer(BOUND_EFFECT)
        };
        let compiled = registry
            .compile_layers(
                2,
                1,
                std::slice::from_ref(&layer),
                &[],
                &Default::default(),
                &bound,
            )
            .unwrap();
        let Processing::Color(operation) = &compiled.segments[0].operations[0] else {
            panic!("a colour operation");
        };
        let named: Vec<String> = operation
            .units()
            .iter()
            .map(|unit| unit.describe())
            .collect();
        assert_eq!(
            named,
            [second.id.to_string(), first.id.to_string()],
            "the module receives the layer's order"
        );
        // A layer without artifacts is compiled exactly as before, through `compile`.
        let plain = registry
            .compile_layers(
                2,
                1,
                &[test_layer(BOUND_EFFECT)],
                &[],
                &Default::default(),
                &Default::default(),
            )
            .unwrap();
        assert!(plain.segments[0].operations.is_empty());
        // Bytes the table was not bound with are refused rather than evaluated without them,
        // whatever else in the process holds them.
        let partial: ArtifactTable = std::iter::once(first.clone()).collect();
        let error = registry
            .compile_layers(
                2,
                1,
                std::slice::from_ref(&layer),
                &[],
                &Default::default(),
                &partial,
            )
            .err()
            .expect("an unbound artifact never compiles");
        assert_eq!(error.kind, ErrorKind::SourceUnavailable);
        assert_eq!(
            error.detail,
            format!(
                "artifact {} of layer {} is not bound to this recipe",
                second.id, layer.id
            )
        );
        // An effect that does not declare artifacts cannot be compiled with any.
        let pixel = Layer {
            artifacts: vec![first.id.clone()],
            ..Layer::pixel(0, 0, [1, 2, 3])
        };
        let error = registry
            .compile_layers(2, 1, &[pixel], &[], &Default::default(), &bound)
            .err()
            .expect("a pixel layer never binds an artifact");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("which its effect does not declare"));
    }

    /// Compiling a recipe reads its artifacts from the table the recipe carries and nowhere else:
    /// a listed artifact the table lacks refuses the whole recipe by name, even while the bytes
    /// are alive elsewhere, and the table is neither stored nor part of the recipe's equality.
    #[test]
    fn a_recipe_whose_bound_table_lacks_a_listed_artifact_refuses_to_compile_naming_it() {
        let registry = bound_registry();
        let (first, second) = (bound_artifact("3"), bound_artifact("4"));
        let layer = Layer {
            artifacts: vec![first.id.clone(), second.id.clone()],
            ..test_layer(BOUND_EFFECT)
        };
        let unbound = Recipe {
            layers: vec![layer.clone()],
            ..Recipe::default()
        };
        let partly = Recipe {
            artifacts: std::iter::once(first.clone()).collect(),
            ..unbound.clone()
        };
        for (recipe, missing) in [(&unbound, &first), (&partly, &second)] {
            let error = registry
                .compile(2, 1, recipe)
                .err()
                .expect("a recipe missing a bound artifact never compiles");
            assert_eq!(error.kind, ErrorKind::SourceUnavailable);
            assert_eq!(
                error.detail,
                format!(
                    "artifact {} of layer {} is not bound to this recipe",
                    missing.id, layer.id
                )
            );
        }
        // Bound with both, the same recipe compiles, and a clone shares the one table.
        let bound = Recipe {
            artifacts: [first.clone(), second.clone()].into_iter().collect(),
            ..unbound.clone()
        };
        assert!(registry.compile(2, 1, &bound).is_ok());
        let clone = bound.clone();
        assert!(clone.artifacts.shares(&bound.artifacts));
        assert!(registry.compile(2, 1, &clone).is_ok());
        // The bytes are never stored and never make two recipes differ.
        assert_eq!(bound, unbound);
        assert_eq!(
            serde_json::to_value(&bound).unwrap(),
            serde_json::to_value(&unbound).unwrap()
        );
        let read: Recipe = serde_json::from_value(serde_json::to_value(&bound).unwrap()).unwrap();
        assert!(read.artifacts.is_empty());
    }
}
