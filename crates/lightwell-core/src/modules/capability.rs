//! The module side of the shared capability contract: what a module that declares worker tasks,
//! an activation, managed resources or an effect evaluated with derived artifacts implements, beyond
//! [`ToolModule`]. A module that declares none of those implements only [`ToolModule`]; one that
//! does returns itself from [`ToolModule::capabilities`], and [`ModuleRegistry::register`] refuses a
//! module whose descriptor needs these hooks and does not provide them. See
//! `docs/design/module-capabilities.md`.
//!
//! [`ModuleRegistry::register`]: super::ModuleRegistry::register
use super::{ModuleDescriptor, Processing, Stage, ToolModule};
use crate::{Error, artifacts::PreparedArtifact, capabilities::context::ModuleContext};
use serde_json::{Map, Value};
use std::{path::Path, sync::Arc};

/// The hooks the capability host and the compile of an artifact-bound layer call. Every one runs
/// only for a declaration that needs it, so each default is what a module that declares no such
/// thing would answer.
pub trait CapabilityModule: ToolModule {
    /// Compile a layer that references derived artifacts: the same as [`ToolModule::compile`], with
    /// the verified bytes of every artifact the layer lists, in the layer's order. The host calls
    /// this instead of `compile` only for a layer whose `artifacts` list is not empty, which only an
    /// effect declaring `artifacts: true` may have. The bytes are immutable and already checked
    /// against their hash; the module decides what they mean and refuses what it cannot use. The
    /// default ignores them.
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: Stage,
        artifacts: &[Arc<PreparedArtifact>],
    ) -> Result<Processing, Error> {
        let _ = artifacts;
        self.compile(effect_id, format, payload, stage)
    }
    /// Load what the module's declared activation needs, on the capability worker's module lane,
    /// after the host checked every required setting and resource. `context` gives the installed
    /// resources' paths, the settings and the declared secrets; the module keeps what it loads
    /// until [`CapabilityModule::deactivate`]. Call `context.checkpoint()` between units of work and
    /// return its error when cancelled. After an activation that does not succeed, or one cancelled
    /// as it finished, the host calls `deactivate` itself, so partial state is released in one
    /// place. Never called on the owner or UI thread, and never by discovery or catalog reopen.
    fn activate(&self, context: &ModuleContext) -> Result<(), Error> {
        let _ = context;
        Ok(())
    }
    /// Release everything `activate` loaded. Called on the module lane, after any work queued
    /// before it; it must tolerate being called when nothing is loaded.
    fn deactivate(&self) {}
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
    /// with their declared defaults), its asset and profile, its activation requirement and a live
    /// grant for every capability it `uses`, and prepared the data it may send. `context` is the
    /// only way to reach any of it: `send` for a granted `remote-image-request` whose body the host
    /// built, `publish_artifact` for a result the catalog records when the task succeeds, and the
    /// settings, secrets, progress and cancellation every job has. Call `context.checkpoint()`
    /// between units of work and return its error when cancelled; artifacts a task publishes before
    /// it fails or is cancelled are never recorded. Never called on the owner or UI thread. The
    /// default refuses.
    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let _ = (task_id, parameters, context);
        Err(Error::new(
            crate::ErrorKind::Validation,
            format!("module {} declares no tasks", self.descriptor().id),
        ))
    }
}

/// The first declaration of `descriptor` that only a [`CapabilityModule`] can serve, named for the
/// registration refusal, or `None` when the module needs no capability hook: worker tasks, an
/// activation, managed resources or an effect evaluated with derived artifacts. Settings, profiles,
/// adapters and capabilities alone are the host's to hold and need no module code.
pub(super) fn needs_capabilities(descriptor: &ModuleDescriptor) -> Option<String> {
    if let Some(task) = descriptor.tasks.first() {
        Some(format!("task {}", task.id))
    } else if descriptor.activation.is_some() {
        Some("an activation".into())
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
