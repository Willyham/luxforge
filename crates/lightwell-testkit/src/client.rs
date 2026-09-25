//! An independent JSON client of the catalog owner: every call is one request through
//! [`OwnerHandle::call`], exactly as a script or an agent reaches the editor, with no desktop. The
//! field-patch conformance suite and xtask's acceptance chapters drive the owner with it, so they
//! share one request, refusal, job-wait and mutation-envelope implementation.
//!
//! A check here returns what it found broken as a [`Checked`] error instead of panicking, so the
//! same code runs as a test (which panics with the message) and inside `cargo xtask
//! editor-acceptance` (which records it as evidence).
use lightwell_core::{
    ApiRequest, ApiResponse, ClientId, ModuleRegistry, OwnerHandle, Recipe, builtin_modules,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A check's result: its evidence, or what it found broken.
pub type Checked<T = ()> = Result<T, String>;

pub fn ensure(ok: bool, message: impl Into<String>) -> Checked {
    if ok { Ok(()) } else { Err(message.into()) }
}

/// Run one named step, prefixing what it found broken with the step's name.
pub fn within<T>(what: &str, step: impl FnOnce() -> Checked<T>) -> Checked<T> {
    step().map_err(|error| format!("{what}: {error}"))
}

/// How long a source or analysis job may take to settle before a check gives up on it.
const JOB_DEADLINE: Duration = Duration::from_secs(60);

/// Request identities are unique per process, so a retry is always deliberate.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub fn request_id(tag: &str) -> String {
    format!("{tag}-{}", NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
}

/// The mutation envelope every asset change carries.
pub fn mutation(revision: u64, request_id: &str, actor: &str) -> Value {
    json!({"expected_revision": revision, "request_id": request_id, "actor": actor})
}

pub fn as_u64(value: &Value, what: &str) -> Checked<u64> {
    value
        .as_u64()
        .ok_or_else(|| format!("{what} is not a number: {value}"))
}

pub fn as_str(value: &Value, what: &str) -> Checked<String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{what} is not a string: {value}"))
}

fn request(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Checked<ApiResponse> {
    owner
        .call(
            client,
            ApiRequest {
                id: request_id(method),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: the owner did not answer: {error}"))
}

/// One call that must succeed.
pub fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Checked<Value> {
    let response = request(owner, client, method, params)?;
    match response.error {
        Some(error) => Err(format!("{method} failed: {} {}", error.code, error.message)),
        None => response
            .result
            .ok_or_else(|| format!("{method} answered neither a result nor an error")),
    }
}

/// One call that must be refused, answering `(code, message)`.
pub fn refused(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Checked<(String, String)> {
    let response = request(owner, client, method, params)?;
    match response.error {
        Some(error) => Ok((error.code, error.message)),
        None => Err(format!(
            "{method} was accepted, answering {}; a refusal was required",
            response.result.unwrap_or(Value::Null)
        )),
    }
}

/// Wait for a source job to leave the queue and answer its last status.
pub fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Checked<Value> {
    let deadline = Instant::now() + JOB_DEADLINE;
    loop {
        let status = call(owner, client, "job.status", json!({"job_id": job}))?;
        match status["status"].as_str() {
            Some("queued" | "running") => {
                ensure(Instant::now() < deadline, "a source job never settled")?;
                std::thread::sleep(Duration::from_millis(2));
            }
            _ => return Ok(status),
        }
    }
}

/// Import one file through the source job an independent client waits on, answering the imported
/// asset record once the job is ready.
pub fn import(owner: &OwnerHandle, client: ClientId, path: &Path, actor: &str) -> Checked<Value> {
    let queued = call(
        owner,
        client,
        "catalog.import",
        json!({"path": path, "mutation": {"request_id": request_id("import"), "actor": actor}}),
    )?;
    let status = settle(owner, client, &queued["job_id"])?;
    ensure(
        status["status"] == json!("ready"),
        format!("the import settled as {status}"),
    )?;
    Ok(status["asset"].clone())
}

/// Prepare the verified source so an evaluating call is answered rather than deferred: a freshly
/// opened catalog has none in its cache, so the first evaluating call would answer
/// `preparation-required` with a job to wait on.
pub fn prepare(owner: &OwnerHandle, client: ClientId, asset: &Value) -> Checked {
    let prepared = call(owner, client, "source.prepare", json!({"asset_id": asset}))?;
    if !prepared["job_id"].is_null() {
        settle(owner, client, &prepared["job_id"])?;
    }
    Ok(())
}

/// One `analysis.request` followed by `analysis.read` until it settles. The answer is the settled
/// read with the request's `job_id` and `requested_identity` added, so a caller can hold the read
/// identity to the requested one.
pub fn analyse(
    owner: &OwnerHandle,
    client: ClientId,
    asset: &Value,
    target: Value,
) -> Checked<Value> {
    let requested = call(
        owner,
        client,
        "analysis.request",
        json!({"asset_id": asset, "target": target}),
    )?;
    let job_id = requested["job_id"].clone();
    let deadline = Instant::now() + JOB_DEADLINE;
    loop {
        let mut read = call(owner, client, "analysis.read", json!({"job_id": job_id}))?;
        match read["status"].as_str() {
            Some("queued" | "running") => {
                ensure(Instant::now() < deadline, "an analysis job never settled")?;
                std::thread::sleep(Duration::from_millis(2));
            }
            _ => {
                read["job_id"] = job_id;
                read["requested_identity"] = requested["identity"].clone();
                return Ok(read);
            }
        }
    }
}

pub fn state(owner: &OwnerHandle, client: ClientId, asset: &Value) -> Checked<Value> {
    call(owner, client, "asset.state", json!({"asset_id": asset}))
}

pub fn revision(owner: &OwnerHandle, client: ClientId, asset: &Value) -> Checked<u64> {
    as_u64(&state(owner, client, asset)?["revision"], "revision")
}

/// The committed recipe of the asset's current entry, as the API reports it.
pub fn recipe(owner: &OwnerHandle, client: ClientId, asset: &Value) -> Checked<Recipe> {
    let state = state(owner, client, asset)?;
    serde_json::from_value(state["current_entry"]["snapshot"]["recipe"].clone())
        .map_err(|error| format!("asset.state answered an unreadable recipe: {error}"))
}

/// A catalog owner that is stopped and joined however the check using it ends. It dereferences to
/// its [`OwnerHandle`], so every free function above takes it too.
pub struct Owner {
    handle: OwnerHandle,
    join: Option<JoinHandle<()>>,
    actor: String,
}

impl Owner {
    /// Start an owner on `catalog`; `actor` is who its imports name.
    pub fn start(catalog: &Path, registry: ModuleRegistry, actor: &str) -> Checked<Self> {
        let (handle, join) = OwnerHandle::start_with(catalog, Arc::new(registry))
            .map_err(|error| format!("the catalog owner did not start: {error}"))?;
        Ok(Self {
            handle,
            join: Some(join),
            actor: actor.to_owned(),
        })
    }

    pub fn client(&self) -> ClientId {
        self.handle.register()
    }

    /// Stop the owner and wait for it, so the next owner opens a catalog nobody else holds.
    pub fn close(mut self) -> Checked {
        self.shut()
    }

    fn shut(&mut self) -> Checked {
        match self.join.take() {
            Some(join) => {
                self.handle.stop();
                join.join()
                    .map_err(|_| "the catalog owner thread panicked".to_owned())
            }
            None => Ok(()),
        }
    }

    /// One call that must succeed.
    pub fn call(&self, client: ClientId, method: &str, params: Value) -> Checked<Value> {
        call(&self.handle, client, method, params)
    }

    /// One call that must be refused, answering `(code, message)`.
    pub fn refused(
        &self,
        client: ClientId,
        method: &str,
        params: Value,
    ) -> Checked<(String, String)> {
        refused(&self.handle, client, method, params)
    }

    /// Import one file and prepare its verified source, answering the imported asset record.
    pub fn import(&self, client: ClientId, path: &Path) -> Checked<Value> {
        let asset = import(&self.handle, client, path, &self.actor)?;
        prepare(&self.handle, client, &asset["asset"]["id"])?;
        Ok(asset)
    }

    pub fn prepare(&self, client: ClientId, asset: &Value) -> Checked {
        prepare(&self.handle, client, asset)
    }

    pub fn analyse(&self, client: ClientId, asset: &Value, target: Value) -> Checked<Value> {
        analyse(&self.handle, client, asset, target)
    }

    pub fn state(&self, client: ClientId, asset: &Value) -> Checked<Value> {
        state(&self.handle, client, asset)
    }

    pub fn revision(&self, client: ClientId, asset: &Value) -> Checked<u64> {
        revision(&self.handle, client, asset)
    }

    /// The committed recipe of the asset's current entry, as the API reports it.
    pub fn recipe(&self, client: ClientId, asset: &Value) -> Checked<Recipe> {
        recipe(&self.handle, client, asset)
    }

    /// The sequence of the newest history entry, which moves exactly when an entry is written.
    pub fn head(&self, client: ClientId, asset: &Value) -> Checked<u64> {
        let listed = self.call(
            client,
            "history.list",
            json!({"asset_id": asset, "limit": 1}),
        )?;
        as_u64(
            &listed["entries"][0]["sequence"],
            "the newest entry's sequence",
        )
    }

    /// The owner's event sequence, which moves exactly when a change is announced.
    pub fn events(&self, client: ClientId) -> Checked<u64> {
        let events = self.call(client, "events.since", json!({"after": 0}))?;
        as_u64(&events["current_sequence"], "the event sequence")
    }

    /// `render.sample` of one pixel, of the draft when one is named.
    pub fn sample(
        &self,
        client: ClientId,
        asset: &Value,
        (x, y): (u32, u32),
        draft: Option<&Value>,
    ) -> Checked<Value> {
        let mut params = json!({"asset_id": asset, "x": x, "y": y});
        if let Some(draft) = draft {
            params["draft_id"] = draft.clone();
        }
        Ok(self.call(client, "render.sample", params)?["rgba"].clone())
    }

    pub fn samples(
        &self,
        client: ClientId,
        asset: &Value,
        probes: &[(u32, u32)],
        draft: Option<&Value>,
    ) -> Checked<Vec<Value>> {
        probes
            .iter()
            .map(|probe| self.sample(client, asset, *probe, draft))
            .collect()
    }

    /// `recipe.describe` of the current entry.
    pub fn describe(&self, client: ClientId, asset: &Value) -> Checked<Value> {
        self.call(client, "recipe.describe", json!({"asset_id": asset}))
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.shut();
    }
}

impl std::ops::Deref for Owner {
    type Target = OwnerHandle;

    fn deref(&self) -> &OwnerHandle {
        &self.handle
    }
}

/// The built-in registry with the one module `module_id` registered unavailable, exactly as the
/// desktop's `--disable-module` does.
pub fn registry_without(module_id: &str) -> Checked<ModuleRegistry> {
    let mut registry = ModuleRegistry::new();
    for module in builtin_modules() {
        if module.descriptor().id == module_id {
            registry.register_unavailable(module, "disabled by a test")
        } else {
            registry.register(module)
        }
        .map_err(|error| format!("a built-in module did not register: {error}"))?;
    }
    Ok(registry)
}
