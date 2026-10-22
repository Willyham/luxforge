//! Worker queries: a module's declared query that runs a model, such as `luxforge.ai`'s
//! `query.pick-object`, which the catalog owner checks and binds and the host's **query worker**
//! answers (`docs/design/ai-editing.md`, "Picking: hover, click, box and brush").
//!
//! # Shape
//!
//! The owner resolves the query, checks its parameters and reads the repair stage identity of the
//! asset's current entry (`O(layers)`, no pixel): a worker query answers about the photograph as it
//! is now, the stack a module task's host-planned read reads, so an entry the request names or the
//! session previews that is not current is refused `validation`. Then the host checks every
//! requirement the module names for the request ([`crate::CapabilityModule::query_needs`]: its
//! flags, the runtime and each used model's installed resource, from the install memo, so a hover
//! reads no file) and refuses `not-ready` with `data.requirements` before anything is queued. A ready request becomes one job of the query
//! worker with a [`ModuleContext`] holding the models it uses, the analysis cache and the identity,
//! and the module answers it there ([`crate::CapabilityModule::run_query`]); the owner's call is
//! answered from the worker.
//!
//! # Bounds
//!
//! One thread for every module's worker queries, started by the first and stopped when a model of a
//! module it served is released, however it is released (`module.runtime.unload`, a resource's
//! removal, the runtime's idle unload, the memory signal), or the host stops; one running query and
//! one waiting ([`crate::latest`]). A newer query takes the waiting slot and the query it replaced
//! is answered `cancelled` at once, so a burst of hover moves decodes once; a running query, a
//! decode of a few milliseconds, is not cancellable: it finishes and is answered. A query never
//! waits for the module lane, and the lane never waits for it.
use super::{CapabilityHost, Requirement, RequirementKind};
use crate::{
    AssetId, EditorService, Error,
    capabilities::{
        context::ModuleContext, descriptor::CapabilityKind, read::StageIdentity,
        runtime::RuntimeGrant,
    },
    jobs::{JobControl, Jobs},
    latest::Latest,
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, PoisonError},
};

/// A worker query the owner resolved and checked, and the identity of the stage it asks about.
#[derive(Clone, Debug)]
pub(crate) struct WorkerQuery {
    pub(crate) module_id: String,
    pub(crate) query_id: String,
    pub(crate) parameters: Map<String, Value>,
    pub(crate) identity: StageIdentity,
}

/// Where a worker query's answer goes.
pub(crate) type Deliver = Box<dyn FnOnce(Result<Value, Error>) + Send>;

/// One query for the worker: what to run and where its answer goes. A query that is dropped
/// before it runs, replaced or left waiting when the worker stops, is answered `cancelled`.
struct QueryJob {
    run: Option<Box<dyn FnOnce() -> Result<Value, Error> + Send>>,
    deliver: Option<Deliver>,
}

impl QueryJob {
    fn answer(&mut self, result: Result<Value, Error>) {
        if let Some(deliver) = self.deliver.take() {
            deliver(result);
        }
    }
}

impl Drop for QueryJob {
    fn drop(&mut self) {
        self.answer(Err(Error::cancelled(
            "a newer query replaced this one before it ran",
        )));
    }
}

#[derive(Default)]
struct WorkerState {
    latest: Option<Latest<QueryJob, ()>>,
    /// The modules whose queries the running worker has answered.
    served: BTreeSet<String>,
}

/// The host's query worker, shared with the runtime's release listener so a release on any thread
/// stops it. See the [module documentation](self).
#[derive(Clone, Default)]
pub(crate) struct QueryWorker(Arc<Mutex<WorkerState>>);

impl QueryWorker {
    fn lock(&self) -> std::sync::MutexGuard<'_, WorkerState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hand `module_id`'s `job` to the worker, starting it when it is stopped; the waiting query it
    /// replaces is answered `cancelled`.
    fn submit(&self, module_id: &str, job: QueryJob) {
        let replaced = {
            let mut state = self.lock();
            state.served.insert(module_id.to_owned());
            let latest = state.latest.get_or_insert_with(|| {
                Latest::new("luxforge-queries", |mut job: QueryJob, _running| {
                    if let Some(run) = job.run.take() {
                        let result = run();
                        job.answer(result);
                    }
                    None
                })
            });
            latest.request(job).replaced
        };
        drop(replaced);
    }

    /// Stop the worker: the waiting query is answered `cancelled` and the running one finishes on
    /// its own thread, which then ends.
    pub(crate) fn stop(&self) {
        let stopped = {
            let mut state = self.lock();
            state.served.clear();
            state.latest.take()
        };
        drop(stopped);
    }

    /// Models of `modules` were released: stop the worker when it served any of them.
    pub(crate) fn released(&self, modules: &[String]) {
        let served = {
            let state = self.lock();
            modules.iter().any(|module| state.served.contains(module))
        };
        if served {
            self.stop();
        }
    }

    /// Whether the worker is started.
    pub(crate) fn running(&self) -> bool {
        self.lock().latest.is_some()
    }
}

impl EditorService {
    /// The worker query `query_id` of the request, checked, with the identity of the repair stage
    /// of the entry it names; `None` when the query is not a worker query, which the owner then
    /// answers as any other. `O(layers)`; reads no pixel.
    pub(crate) fn worker_query(
        &self,
        asset_id: &AssetId,
        entry_id: &crate::EntryId,
        query_id: &str,
        parameters: &Value,
    ) -> Result<Option<WorkerQuery>, Error> {
        let registry = self.registry();
        let Some(crate::QueryRef::Module(module, descriptor)) = registry.resolve_query(query_id)
        else {
            return Ok(None);
        };
        let module_id = module.descriptor().id.clone();
        if !module
            .capabilities()
            .is_some_and(|hooks| hooks.worker_query(query_id))
        {
            return Ok(None);
        }
        let checked = crate::modules::check_parameters(descriptor, parameters)?;
        // A worker query answers about the photograph as it is now: the stack a module task's
        // host-planned read reads, so the analysis it answers from is one a task can prepare. An
        // entry the client names or previews that is not current is refused, as an edit is.
        let current = self.current_entry_id(asset_id)?;
        if entry_id != &current {
            return Err(Error::validation(format!(
                "{query_id} answers about the current entry {current}, the one module tasks \
                 read; entry {entry_id} is not current"
            ))
            .with_data(json!({"entry_id": entry_id, "current_entry_id": current})));
        }
        let identity = self.repair_identity(asset_id, &current)?;
        Ok(Some(WorkerQuery {
            module_id,
            query_id: query_id.to_owned(),
            parameters: checked,
            identity,
        }))
    }
}

impl CapabilityHost {
    /// Check a worker query's requirements and hand it to the query worker, which answers
    /// `deliver`; a refusal is delivered at once.
    pub(crate) fn worker_query(
        &mut self,
        jobs: &Jobs,
        service: &EditorService,
        query: WorkerQuery,
        deliver: Deliver,
    ) {
        match self.bind_query(jobs, service, &query) {
            Ok(context) => {
                let registry = service.registry().clone();
                let WorkerQuery {
                    module_id,
                    query_id,
                    parameters,
                    ..
                } = query;
                let served = module_id.clone();
                self.queries.submit(
                    &served,
                    QueryJob {
                        run: Some(Box::new(move || {
                            let hooks = registry.capabilities(&module_id).ok_or_else(|| {
                                Error::internal(format!("module {module_id} is not registered"))
                            })?;
                            hooks.run_query(&query_id, &parameters, &context)
                        })),
                        deliver: Some(deliver),
                    },
                );
            }
            Err(error) => deliver(Err(error)),
        }
    }

    /// The context a ready worker query runs with, or its unmet requirements as `not-ready`.
    fn bind_query(
        &self,
        jobs: &Jobs,
        service: &EditorService,
        query: &WorkerQuery,
    ) -> Result<ModuleContext, Error> {
        let registry = service.registry();
        let hooks = registry.capabilities(&query.module_id).ok_or_else(|| {
            Error::internal(format!("module {} is not registered", query.module_id))
        })?;
        let descriptor = hooks.descriptor();
        let needs = hooks.query_needs(&query.query_id, &query.parameters)?;
        let mut missing = Vec::new();
        for flag in &needs.flags {
            if !crate::flags::live_toggle(&self.preferences, self.launch_flags(), flag) {
                missing.push(Requirement {
                    kind: RequirementKind::Flag,
                    id: (*flag).to_owned(),
                    state: "off".into(),
                });
            }
        }
        let mut runtimes = Vec::new();
        for id in needs.uses.iter().flatten() {
            let capability = descriptor
                .capability(id)
                .filter(|capability| matches!(capability.kind, CapabilityKind::LocalRuntime { .. }))
                .ok_or_else(|| {
                    Error::internal(format!(
                        "query {} uses {id}, which is not a local-runtime capability of its module",
                        query.query_id
                    ))
                })?;
            if self.runtime.configured().is_err() {
                missing.push(Requirement {
                    kind: RequirementKind::Runtime,
                    id: id.clone(),
                    state: "unavailable".into(),
                });
            }
            match self.load_request(jobs, descriptor, capability)? {
                Ok(load) => runtimes.push((id.clone(), load)),
                Err(requirement) => missing.push(requirement),
            }
        }
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|requirement| {
                    format!(
                        "{} {} is {}",
                        requirement.kind.name(),
                        requirement.id,
                        requirement.state
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(
                Error::not_ready(format!("query {} is not ready: {list}", query.query_id))
                    .with_data(json!({"requirements": missing})),
            );
        }
        let mut context = ModuleContext::new(
            &query.module_id,
            self.config.secrets.clone(),
            JobControl::new(),
        )
        .with_analysis(self.analysis.clone())
        .with_identity(query.identity.clone());
        for (capability, load) in runtimes {
            context =
                context.with_runtime(&capability, RuntimeGrant::new(self.runtime.clone(), load));
        }
        Ok(context)
    }
}
