//! The module side of the shared capability contract: what a module that declares worker tasks,
//! managed resources or an effect evaluated with derived artifacts implements, beyond
//! [`ToolModule`]. A module that declares none of those implements only [`ToolModule`]; one that
//! does returns itself from [`ToolModule::capabilities`], and [`ModuleRegistry::register`] refuses a
//! module whose descriptor needs these hooks and does not provide them. See
//! `docs/design/module-capabilities.md`.
//!
//! [`ModuleRegistry::register`]: super::ModuleRegistry::register
use super::{ModuleDescriptor, Processing, Stage, ToolModule};
use crate::{
    Error,
    artifacts::PreparedArtifact,
    capabilities::{context::ModuleContext, resources::ResourceRow, resources::ResourceState},
};
use serde_json::{Map, Value};
use std::{path::Path, sync::Arc};

/// What one request of a task needs beyond the task's declaration, which the host checks before
/// it queues the task ([`CapabilityModule::task_needs`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskNeeds {
    /// The declared capabilities this request uses, in the task's order; `None` uses every one the
    /// task declares. A resource it uses that is not installed is an unmet requirement, as for
    /// every task, so a request that needs an optional resource asks for it here.
    pub uses: Option<Vec<String>>,
    /// Toggle flags ([`crate::flags`]) that must be on: one that is off is an unmet requirement.
    pub flags: Vec<&'static str>,
    /// The task holds its asset locked while it runs: the host refuses every edit of that asset,
    /// and another locking task on it, until the job ends, however it ends.
    pub locks_asset: bool,
}

/// What the host knows once it has bound a task's request, for the module's admission
/// ([`CapabilityModule::admit_task`]): nothing has been read or allocated yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskAdmission {
    /// The size of the stage the task reads, for a task that declares `reads`.
    pub stage: Option<Stage>,
    /// The bytes of every artifact the asset's current stack binds.
    pub artifact_bytes: u64,
}

/// The host state a module's own status fields are computed from ([`CapabilityModule::status`]):
/// read from tables the host already holds, never opening anything.
pub struct HostView<'a> {
    pub(crate) resources: &'a [ResourceRow],
    pub(crate) runtime: bool,
    pub(crate) flag: &'a dyn Fn(&str) -> bool,
}

impl HostView<'_> {
    /// The state of one of the module's resources.
    pub fn resource(&self, id: &str) -> ResourceState {
        self.resources
            .iter()
            .find(|row| row.id == id)
            .map_or(ResourceState::NotInstalled, |row| row.state)
    }

    /// Whether the host has a local inference runtime.
    pub fn runtime(&self) -> bool {
        self.runtime
    }

    /// Whether a toggle flag is on now.
    pub fn flag(&self, id: &str) -> bool {
        (self.flag)(id)
    }
}

/// The hooks the capability host and the compile of an artifact-bound layer call. Every one runs
/// only for a declaration that needs it, so each default is what a module that declares no such
/// thing would answer.
pub trait CapabilityModule: ToolModule {
    /// Compile a layer that references derived artifacts: the same as [`ToolModule::compile`], with
    /// the verified bytes of every artifact the layer lists, in the layer's order. The host calls
    /// this instead of `compile` only for a layer whose `artifacts` list is not empty, which only an
    /// effect declaring `artifacts: true` may have. The bytes are immutable and already checked
    /// against their hash; the module decides what they mean and refuses what it cannot use.
    /// `strokes` are the recipe's resolved strokes, as [`ToolModule::compile_with_strokes`]
    /// receives them. The default ignores both.
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
        artifacts: &[Arc<PreparedArtifact>],
        strokes: &crate::path::StrokeTable,
    ) -> Result<Processing, Error> {
        let _ = (artifacts, strokes);
        self.compile(effect_id, format, payload, at)
    }
    /// Check that a staged resource's bytes are the format the module declares, before the host
    /// installs it. The bytes already match the pinned length and SHA-256. Called on the transfer
    /// lane; a refusal leaves nothing installed. Read the file; never execute or load it with a
    /// general object loader.
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        let _ = (resource_id, path);
        Ok(())
    }
    /// Run one declared worker task on the capability worker's module lane and return its result
    /// value, which the host reports as the job's `result`.
    ///
    /// Before the job was queued the host checked the task's parameters (`parameters` holds them
    /// with their declared defaults), its asset and profile, that every resource it `uses` is
    /// installed and a live grant for every remote capability it `uses`, and prepared the data it
    /// may send. `context` is the only way to reach any of it: `send` for a granted
    /// `remote-image-request` whose body the host built, `resource_path` for a resource it uses,
    /// `publish_artifact` for a result the catalog records when the task succeeds, and the
    /// settings, secrets, progress and cancellation every job has. Call `context.checkpoint()`
    /// between units of work and return its error when cancelled; artifacts a task publishes
    /// before it fails or is cancelled are never recorded. Never called on the owner or UI thread.
    /// The default refuses.
    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let _ = (task_id, parameters, context);
        Err(Error::validation(format!(
            "module {} declares no tasks",
            self.descriptor().id
        )))
    }
    /// What one request of a task needs beyond its declaration: the capabilities it uses, the flags
    /// that gate it and whether it locks its asset. Called on the owner after the parameters were
    /// checked against their declarations (`parameters` holds them with their defaults), so it reads
    /// only the request; a refusal is the request's. The default uses every declared capability,
    /// needs no flag and locks nothing.
    fn task_needs(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<TaskNeeds, Error> {
        let _ = (task_id, parameters);
        Ok(TaskNeeds::default())
    }
    /// Admit one request of a task once the host has bound it, before it is queued: a request the
    /// module cannot serve within its limits is refused here, by name, before anything is read or
    /// allocated. Called on the owner; `O(request)`. The default admits everything.
    fn admit_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        admission: &TaskAdmission,
    ) -> Result<(), Error> {
        let _ = (task_id, parameters, admission);
        Ok(())
    }
    /// Fields of the module's own that `module.status` adds to its answer, such as what the module
    /// offers with what is installed now. Called on the owner from tables the host holds. The
    /// default adds none.
    fn status(&self, view: &HostView<'_>) -> Map<String, Value> {
        let _ = view;
        Map::new()
    }
}

/// The first declaration of `descriptor` that only a [`CapabilityModule`] can serve, named for the
/// registration refusal, or `None` when the module needs no capability hook: worker tasks, managed
/// resources or an effect evaluated with derived artifacts. Settings, profiles, adapters and
/// capabilities alone are the host's to hold and need no module code.
pub(super) fn needs_capabilities(descriptor: &ModuleDescriptor) -> Option<String> {
    if let Some(task) = descriptor.tasks.first() {
        Some(format!("task {}", task.id))
    } else if let Some(resource) = descriptor.resources.first() {
        Some(format!("resource {}", resource.id))
    } else {
        descriptor
            .effects
            .iter()
            .find(|effect| effect.artifacts)
            .map(|effect| format!("effect {}, which references artifacts", effect.id))
    }
}
