//! An independent JSON client of the catalog owner: every call is one request through
//! [`OwnerHandle::call`], exactly as a script or an agent reaches the editor, with no desktop.
use super::{Checked, ensure};
use lightwell_core::{ApiRequest, ClientId, ModuleRegistry, OwnerHandle, Recipe, builtin_modules};
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

/// Request identities are unique per process, so a retry is always deliberate.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub fn request_id(tag: &str) -> String {
    format!("{tag}-{}", NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
}

/// A catalog owner that is stopped and joined however the check using it ends.
pub struct Owner {
    handle: OwnerHandle,
    join: Option<JoinHandle<()>>,
}

impl Owner {
    pub fn start(catalog: &Path, registry: ModuleRegistry) -> Checked<Self> {
        let (handle, join) = OwnerHandle::start_with(catalog, Arc::new(registry))
            .map_err(|error| format!("the catalog owner did not start: {error}"))?;
        Ok(Self {
            handle,
            join: Some(join),
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
        let response = self.request(client, method, params)?;
        match response.error {
            Some(error) => Err(format!("{method} failed: {} {}", error.code, error.message)),
            None => response
                .result
                .ok_or_else(|| format!("{method} answered neither a result nor an error")),
        }
    }

    /// One call that must be refused, answering `(code, message)`.
    pub fn refused(
        &self,
        client: ClientId,
        method: &str,
        params: Value,
    ) -> Checked<(String, String)> {
        let response = self.request(client, method, params)?;
        match response.error {
            Some(error) => Ok((error.code, error.message)),
            None => Err(format!(
                "{method} was accepted, answering {}; a refusal was required",
                response.result.unwrap_or(Value::Null)
            )),
        }
    }

    fn request(
        &self,
        client: ClientId,
        method: &str,
        params: Value,
    ) -> Checked<lightwell_core::ApiResponse> {
        self.handle
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

    /// Import one file through the source job an independent client waits on, and prepare its
    /// verified source so an evaluating call is answered rather than deferred.
    pub fn import(&self, client: ClientId, path: &Path) -> Checked<Value> {
        let queued = self.call(
            client,
            "catalog.import",
            json!({"path": path, "mutation": {"request_id": request_id("import"), "actor": ACTOR}}),
        )?;
        let status = self.settle(client, &queued["job_id"])?;
        ensure(
            status["status"] == json!("ready"),
            format!("the import settled as {status}"),
        )?;
        let asset = status["asset"].clone();
        self.prepare(client, &asset["asset"]["id"])?;
        Ok(asset)
    }

    pub fn prepare(&self, client: ClientId, asset: &Value) -> Checked {
        let prepared = self.call(client, "source.prepare", json!({"asset_id": asset}))?;
        if !prepared["job_id"].is_null() {
            self.settle(client, &prepared["job_id"])?;
        }
        Ok(())
    }

    fn settle(&self, client: ClientId, job: &Value) -> Checked<Value> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = self.call(client, "job.status", json!({"job_id": job}))?;
            match status["status"].as_str() {
                Some("queued" | "running") => {
                    ensure(Instant::now() < deadline, "a source job never settled")?;
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => return Ok(status),
            }
        }
    }

    /// One `analysis.request` followed by `analysis.read` until it settles.
    pub fn analyse(&self, client: ClientId, asset: &Value, target: Value) -> Checked<Value> {
        let requested = self.call(
            client,
            "analysis.request",
            json!({"asset_id": asset, "target": target}),
        )?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let read = self.call(
                client,
                "analysis.read",
                json!({"job_id": requested["job_id"]}),
            )?;
            match read["status"].as_str() {
                Some("queued" | "running") => {
                    ensure(Instant::now() < deadline, "an analysis job never settled")?;
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => return Ok(read),
            }
        }
    }

    pub fn state(&self, client: ClientId, asset: &Value) -> Checked<Value> {
        self.call(client, "asset.state", json!({"asset_id": asset}))
    }

    pub fn revision(&self, client: ClientId, asset: &Value) -> Checked<u64> {
        let state = self.state(client, asset)?;
        state["revision"]
            .as_u64()
            .ok_or_else(|| format!("asset.state answered no revision: {state}"))
    }

    /// The committed recipe of the asset's current entry, as the API reports it.
    pub fn recipe(&self, client: ClientId, asset: &Value) -> Checked<Recipe> {
        let state = self.state(client, asset)?;
        serde_json::from_value(state["current_entry"]["snapshot"]["recipe"].clone())
            .map_err(|error| format!("asset.state answered an unreadable recipe: {error}"))
    }

    /// The sequence of the newest history entry, which moves exactly when an entry is written.
    pub fn head(&self, client: ClientId, asset: &Value) -> Checked<u64> {
        let listed = self.call(
            client,
            "history.list",
            json!({"asset_id": asset, "limit": 1}),
        )?;
        listed["entries"][0]["sequence"]
            .as_u64()
            .ok_or_else(|| format!("history.list answered no entry: {listed}"))
    }

    /// The owner's event sequence, which moves exactly when a change is announced.
    pub fn events(&self, client: ClientId) -> Checked<u64> {
        let events = self.call(client, "events.since", json!({"after": 0}))?;
        events["current_sequence"]
            .as_u64()
            .ok_or_else(|| format!("events.since answered no sequence: {events}"))
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

/// Who the suite's mutations name.
pub const ACTOR: &str = "field-patch-conformance";

/// The mutation envelope every asset change carries.
pub fn mutation(revision: u64, tag: &str) -> Value {
    json!({"expected_revision": revision, "request_id": request_id(tag), "actor": ACTOR})
}

/// The built-in registry with the one module `module_id` registered unavailable, exactly as the
/// desktop's `--disable-module` does.
pub fn registry_without(module_id: &str) -> Checked<ModuleRegistry> {
    let mut registry = ModuleRegistry::new();
    for module in builtin_modules() {
        if module.descriptor().id == module_id {
            registry.register_unavailable(module, "disabled by the conformance suite")
        } else {
            registry.register(module)
        }
        .map_err(|error| format!("a built-in module did not register: {error}"))?;
    }
    Ok(registry)
}
