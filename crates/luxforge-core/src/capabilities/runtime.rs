//! The host side of the `local-runtime` capability: the resident set, the host-owned slot each
//! loaded model lives in between tasks (`docs/design/module-capabilities.md#local-runtime`).
//!
//! # Shape
//!
//! One [`Runtime`] per catalog owner, shared by the capability host on the owner, the module lane
//! that loads and uses models, and the owner's handle, which the desktop calls on a memory signal.
//! A model is opened through the host's [`InferenceRuntime`] ([`crate::HostConfig::inference`]) by
//! [`Runtime::acquire`], on the module lane only: by a `module.runtime.load` job, or by the first
//! task that uses the capability. It then stays in its slot, keyed by module and resource, until
//! `module.runtime.unload`, a removal of its resource, the idle interval, a memory signal or the
//! owner stopping releases it. Nothing here opens a model on the owner, and nothing opens one for a
//! listing, a catalog open or a launch.
//!
//! # Bounds
//!
//! Every model counts the resident bytes its capability declares, and the set holds at most the
//! host's budget ([`crate::HostConfig::runtime_budget_bytes`], [`DEFAULT_RUNTIME_BUDGET_BYTES`])
//! of resident and loading models together. A load past it first releases the least recently used
//! models no task holds, and is refused `resource-limit` when that is not enough, before anything
//! is opened. A model a task holds is never released from under it: an unload empties the slot, and
//! the task's own reference releases the model when the task ends.
//!
//! One thread, `luxforge-runtime-idle`, started when the first model becomes resident and ended
//! when the set is empty, sleeps until the earliest idle deadline ([`RUNTIME_IDLE_UNLOAD`] after a
//! model was last used, by default) and releases the models past it; it never polls.
use super::descriptor::ModelKind;
use crate::{
    Cancel, Error,
    editor::now_ms,
    inference::{Generator, InferenceRuntime, ModelFile, OpenOptions, Placement, Session},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// `module.runtime.load {module_id, resource_id}`: load a model ahead of its first task.
pub const LOAD: &str = "module.runtime.load";
/// `module.runtime.unload {module_id, resource_id?}`: release one or all of a module's models.
pub const UNLOAD: &str = "module.runtime.unload";

/// How long a loaded model may go unused before the host releases it: the recorded default of ten
/// minutes (`docs/design/ai-editing.md#models-manifest-download-activation-and-memory`).
pub const RUNTIME_IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// The most bytes the resident set holds by default, resident and loading models together: the
/// generative tier's recorded resident set of 16 GB.
pub const DEFAULT_RUNTIME_BUDGET_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// The reason a `module.runtime.load` job is cancelled when its model is unloaded meanwhile.
pub(crate) const UNLOADED: &str = "unloaded";

/// One loaded model, shared between its slot and the tasks using it.
#[derive(Clone)]
pub(crate) enum Loaded {
    Session(Arc<dyn Session>),
    Generator(Arc<dyn Generator>),
}

impl Loaded {
    fn placement(&self) -> &Placement {
        match self {
            Self::Session(session) => session.placement(),
            Self::Generator(generator) => generator.placement(),
        }
    }

    /// Whether anything besides its slot holds it: a task using it.
    fn in_use(&self) -> bool {
        match self {
            Self::Session(session) => Arc::strong_count(session) > 1,
            Self::Generator(generator) => Arc::strong_count(generator) > 1,
        }
    }
}

/// What one load opens: a module's resource as its `local-runtime` capability declares it.
#[derive(Clone, Debug)]
pub(crate) struct LoadRequest {
    pub module_id: String,
    pub resource_id: String,
    pub capability_id: String,
    pub model: ModelKind,
    pub resident_bytes: u64,
    pub file: ModelFile,
    pub options: OpenOptions,
}

impl LoadRequest {
    fn key(&self) -> Key {
        (self.module_id.clone(), self.resource_id.clone())
    }
}

/// A slot's identity: the module and the resource it holds.
type Key = (String, String);

struct Resident {
    capability_id: String,
    model: ModelKind,
    loaded: Loaded,
    resident_bytes: u64,
    loaded_ms: i64,
    last_used: Instant,
}

struct Loading {
    capability_id: String,
    model: ModelKind,
    resident_bytes: u64,
    /// Which load this is: an unload removes the entry, and a load whose entry is gone or another's
    /// when it finishes keeps nothing.
    ticket: u64,
}

#[derive(Default)]
struct State {
    resident: BTreeMap<Key, Resident>,
    loading: BTreeMap<Key, Loading>,
    next_ticket: u64,
    /// Whether the idle thread is running.
    reaper: bool,
    stopped: bool,
}

impl State {
    fn held_bytes(&self) -> u64 {
        self.resident
            .values()
            .map(|resident| resident.resident_bytes)
            .chain(self.loading.values().map(|loading| loading.resident_bytes))
            .sum()
    }

    /// Release least recently used models no task holds until `bytes` more fit in `budget`, or
    /// refuse `resource-limit` releasing nothing. The released models are returned, to be dropped
    /// outside the lock.
    fn make_room(&mut self, bytes: u64, budget: u64) -> Result<Vec<Resident>, Error> {
        let refused = || {
            Error::resource_limit(format!(
                "a model of {bytes} resident bytes does not fit the {budget} bytes the host's \
                 loaded models may hold; unload one, or wait for one in use to finish"
            ))
        };
        if bytes > budget {
            return Err(refused());
        }
        let held = self.held_bytes();
        if held + bytes <= budget {
            return Ok(Vec::new());
        }
        let mut idle: Vec<(&Key, &Resident)> = self
            .resident
            .iter()
            .filter(|(_, resident)| !resident.loaded.in_use())
            .collect();
        idle.sort_by_key(|(_, resident)| resident.last_used);
        let mut freed = 0;
        let mut evict = Vec::new();
        for (key, resident) in idle {
            if held - freed + bytes <= budget {
                break;
            }
            freed += resident.resident_bytes;
            evict.push(key.clone());
        }
        if held - freed + bytes > budget {
            return Err(refused());
        }
        Ok(evict
            .iter()
            .filter_map(|key| self.resident.remove(key))
            .collect())
    }
}

struct Shared {
    runtime: Option<Arc<dyn InferenceRuntime>>,
    idle: Duration,
    budget: u64,
    state: Mutex<State>,
    /// Signalled when the set changes: a model became resident or was released, a task released
    /// one, or a load finished. The idle thread and a load waiting on another wait on it.
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock: models are opened and dropped outside it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The resident set. See the [module documentation](self).
#[derive(Clone)]
pub(crate) struct Runtime {
    shared: Arc<Shared>,
}

impl Runtime {
    pub(crate) fn new(
        runtime: Option<Arc<dyn InferenceRuntime>>,
        idle: Duration,
        budget: u64,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                runtime,
                idle,
                budget,
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
        }
    }

    /// The host's runtime, or `not-ready` when this host runs no local inference.
    pub(crate) fn configured(&self) -> Result<&Arc<dyn InferenceRuntime>, Error> {
        self.shared
            .runtime
            .as_ref()
            .ok_or_else(|| Error::not_ready("no local inference runtime is configured"))
    }

    /// Whether `module_id`'s `resource_id` is resident, marking it used now when it is.
    pub(crate) fn touch(&self, module_id: &str, resource_id: &str) -> bool {
        let mut state = self.shared.lock();
        match state
            .resident
            .get_mut(&(module_id.to_owned(), resource_id.to_owned()))
        {
            Some(resident) => {
                resident.last_used = Instant::now();
                drop(state);
                self.shared.changed.notify_all();
                true
            }
            None => false,
        }
    }

    /// Whether a model of `bytes` could be loaded now, releasing idle models first: the owner's
    /// check before it queues a load, so a load that can never fit is refused before it waits on
    /// the lane. Releases nothing.
    pub(crate) fn admits(&self, bytes: u64) -> Result<(), Error> {
        let state = self.shared.lock();
        let held = state.held_bytes();
        let idle: u64 = state
            .resident
            .values()
            .filter(|resident| !resident.loaded.in_use())
            .map(|resident| resident.resident_bytes)
            .sum();
        if bytes > self.shared.budget || held - idle + bytes > self.shared.budget {
            return Err(Error::resource_limit(format!(
                "a model of {bytes} resident bytes does not fit the {} bytes the host's loaded \
                 models may hold; unload one, or wait for one in use to finish",
                self.shared.budget
            )));
        }
        Ok(())
    }

    /// The model `request` names: its slot's when it is resident, or opened now through the host's
    /// runtime and kept in its slot. Called on the module lane only, never the owner: opening may
    /// take seconds. `cancel` is checked before the open and by the runtime during it; a load that
    /// an unload, a memory signal or a stop overtook answers its model to its caller without
    /// keeping it.
    pub(crate) fn acquire(&self, request: &LoadRequest, cancel: &Cancel) -> Result<Loaded, Error> {
        let runtime = self.configured()?.clone();
        let key = request.key();
        let (ticket, released) = {
            let mut state = self.shared.lock();
            loop {
                cancel.check()?;
                if state.stopped {
                    return Err(Error::not_ready("the catalog owner is stopping"));
                }
                if let Some(resident) = state.resident.get_mut(&key) {
                    resident.last_used = Instant::now();
                    return Ok(resident.loaded.clone());
                }
                if !state.loading.contains_key(&key) {
                    break;
                }
                // Another lane is opening the same model; it answers soon, or is cancelled.
                state = self
                    .shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(50))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            let released = state.make_room(request.resident_bytes, self.shared.budget)?;
            let ticket = state.next_ticket;
            state.next_ticket += 1;
            state.loading.insert(
                key.clone(),
                Loading {
                    capability_id: request.capability_id.clone(),
                    model: request.model,
                    resident_bytes: request.resident_bytes,
                    ticket,
                },
            );
            (ticket, released)
        };
        drop(released);
        let opened = match request.model {
            ModelKind::Session => runtime
                .open_session(&request.file, &request.options, cancel)
                .map(|session| Loaded::Session(Arc::from(session))),
            ModelKind::Generator => runtime
                .open_generator(&request.file, &request.options, cancel)
                .map(|generator| Loaded::Generator(Arc::from(generator))),
        };
        let start_reaper = {
            let mut state = self.shared.lock();
            let ours = state
                .loading
                .get(&key)
                .is_some_and(|loading| loading.ticket == ticket);
            if ours {
                state.loading.remove(&key);
            }
            let keep = ours && !state.stopped;
            let start = match &opened {
                Ok(loaded) if keep => {
                    state.resident.insert(
                        key,
                        Resident {
                            capability_id: request.capability_id.clone(),
                            model: request.model,
                            loaded: loaded.clone(),
                            resident_bytes: request.resident_bytes,
                            loaded_ms: now_ms(),
                            last_used: Instant::now(),
                        },
                    );
                    !std::mem::replace(&mut state.reaper, true)
                }
                _ => false,
            };
            drop(state);
            self.shared.changed.notify_all();
            start
        };
        if start_reaper {
            self.start_reaper();
        }
        opened
    }

    /// Release `module_id`'s models: `resource_id`'s, or all of them. A load in progress keeps
    /// nothing when it finishes. Answers the resources released, resident or loading, in order.
    pub(crate) fn unload(&self, module_id: &str, resource_id: Option<&str>) -> Vec<String> {
        let matches = |(module, resource): &Key| {
            module == module_id && resource_id.is_none_or(|wanted| wanted == resource)
        };
        let (released, names) = {
            let mut state = self.shared.lock();
            let keys: Vec<Key> = state
                .resident
                .keys()
                .chain(state.loading.keys())
                .filter(|key| matches(key))
                .cloned()
                .collect();
            let mut released = Vec::new();
            let mut names: Vec<String> = Vec::new();
            for key in keys {
                if let Some(resident) = state.resident.remove(&key) {
                    released.push(resident);
                }
                state.loading.remove(&key);
                if !names.contains(&key.1) {
                    names.push(key.1);
                }
            }
            names.sort();
            (released, names)
        };
        drop(released);
        self.shared.changed.notify_all();
        names
    }

    /// The host's memory signal: release every model of every module, as an unload of each would.
    /// Answers how many were resident or loading.
    pub(crate) fn release_all(&self) -> usize {
        let released = {
            let mut state = self.shared.lock();
            let count = state.resident.len() + state.loading.len();
            state.loading.clear();
            (std::mem::take(&mut state.resident), count)
        };
        let (models, count) = released;
        drop(models);
        self.shared.changed.notify_all();
        count
    }

    /// Release everything and refuse every later load: the owner is stopping. The idle thread ends.
    pub(crate) fn shutdown(&self) {
        let models = {
            let mut state = self.shared.lock();
            state.stopped = true;
            state.loading.clear();
            std::mem::take(&mut state.resident)
        };
        drop(models);
        self.shared.changed.notify_all();
    }

    /// `module.status`'s `runtime`: each of `module_id`'s resident and loading models.
    pub(crate) fn status(&self, module_id: &str) -> Vec<Value> {
        let state = self.shared.lock();
        let now = Instant::now();
        let resident = state
            .resident
            .iter()
            .filter(|((module, _), _)| module == module_id)
            .map(|((_, resource), resident)| {
                json!({
                    "resource_id": resource,
                    "capability": resident.capability_id,
                    "model": resident.model.name(),
                    "state": "loaded",
                    "resident_bytes": resident.resident_bytes,
                    "placement": resident.loaded.placement(),
                    "loaded_ms": resident.loaded_ms,
                    "idle_ms": u64::try_from(now.duration_since(resident.last_used).as_millis())
                        .unwrap_or(u64::MAX),
                    "in_use": resident.loaded.in_use(),
                })
            });
        let loading = state
            .loading
            .iter()
            .filter(|((module, _), _)| module == module_id)
            .map(|((_, resource), loading)| {
                json!({
                    "resource_id": resource,
                    "capability": loading.capability_id,
                    "model": loading.model.name(),
                    "state": "loading",
                    "resident_bytes": loading.resident_bytes,
                })
            });
        resident.chain(loading).collect()
    }

    /// The resident set's bytes and budget, for `module.status`.
    pub(crate) fn budget(&self) -> Value {
        let state = self.shared.lock();
        json!({"held_bytes": state.held_bytes(), "budget_bytes": self.shared.budget})
    }

    fn start_reaper(&self) {
        let shared = self.shared.clone();
        let started = std::thread::Builder::new()
            .name("luxforge-runtime-idle".into())
            .spawn(move || reap(&shared));
        if started.is_err() {
            // Without the thread nothing is released for idleness; an unload, a memory signal or
            // the owner stopping still release every model. The next load tries again.
            self.shared.lock().reaper = false;
        }
    }
}

/// The idle thread: sleep until the earliest idle deadline, release the models past it, and end
/// once nothing is resident or the owner has stopped.
fn reap(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        if state.stopped || state.resident.is_empty() {
            state.reaper = false;
            return;
        }
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut next = now + shared.idle;
        for (key, resident) in &state.resident {
            if resident.loaded.in_use() {
                // A task holds it: it is used now. Its task's end touches it and wakes this.
                continue;
            }
            let deadline = resident.last_used + shared.idle;
            if deadline <= now {
                expired.push(key.clone());
            } else {
                next = next.min(deadline);
            }
        }
        if !expired.is_empty() {
            let released: Vec<Resident> = expired
                .iter()
                .filter_map(|key| state.resident.remove(key))
                .collect();
            drop(state);
            drop(released);
            state = shared.lock();
            continue;
        }
        state = shared
            .changed
            .wait_timeout(state, next.saturating_duration_since(now))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// What a task's context holds for one `local-runtime` capability it uses: the resident set and the
/// load its capability names. When the task ends it marks a model it used as used now, so the idle
/// interval counts from the end of the task.
pub(crate) struct RuntimeGrant {
    pub runtime: Runtime,
    pub request: LoadRequest,
    used: std::sync::atomic::AtomicBool,
}

impl RuntimeGrant {
    pub(crate) fn new(runtime: Runtime, request: LoadRequest) -> Self {
        Self {
            runtime,
            request,
            used: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn acquire(&self, cancel: &Cancel) -> Result<Loaded, Error> {
        let loaded = self.runtime.acquire(&self.request, cancel)?;
        self.used.store(true, std::sync::atomic::Ordering::Release);
        Ok(loaded)
    }
}

impl Drop for RuntimeGrant {
    fn drop(&mut self) {
        if self.used.load(std::sync::atomic::Ordering::Acquire) {
            self.runtime
                .touch(&self.request.module_id, &self.request.resource_id);
        }
    }
}

impl std::fmt::Debug for RuntimeGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeGrant")
            .field("module_id", &self.request.module_id)
            .field("resource_id", &self.request.resource_id)
            .finish_non_exhaustive()
    }
}
