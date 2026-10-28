//! The engine behind external adjustment modules (`docs/design/module-manager.md#contract`).
//!
//! An installed package's `module.wasm` is a WebAssembly component of the
//! `luxforge:module/adjustment@0.1.0` world in the repository's `wit/adjustment.wit`. This crate
//! runs it in Wasmtime and hands the registry assembly the package's module
//! ([`WasmHost::externals`]): the field-patch module of its manifest, whose colour arithmetic
//! calls the component. The core runs no WebAssembly and names no engine; it sees an
//! [`AdjustmentUnits`] and the [`PointwiseColor`](luxforge_core::PointwiseColor) units it
//! compiles.
//!
//! - **Loading** compiles the component, or reads its compiled code from the cache
//!   ([`WasmHost::new`]'s directory), refuses a component that imports anything, and checks that
//!   it exports the `adjust` interface with its three functions. It creates no instance. A package
//!   that does not load registers unavailable with the reason.
//! - **Instances** are created lazily, one per worker that calls the module at once, and kept for
//!   reuse up to [`MAX_IDLE_INSTANCES`]. Each has its own store, with at most
//!   [`Limits::memory_bytes`] of linear memory and one memory, and each call an epoch deadline of
//!   [`Limits::deadline`]. An instance a call failed in is dropped.
//! - **A layer** compiles to one unit whose words are the component's `coefficients` for the
//!   layer's values; the words, with the package hash, are the unit's identity. Its rows call
//!   `apply-row`, at most [`MAX_ROW_PIXELS`] pixels a call.
//! - **A failure** fails the render, sample or export that met it, with an error naming the module
//!   and saying what failed (a trap, the deadline, a wrong-length answer), whose `data` holds
//!   `{"module", "failure"}`; no frame is produced.
mod cache;
mod engine;
mod unit;

pub use unit::Unit;

use luxforge_core::Error;
use luxforge_core::module_package::{
    AdjustmentUnits, ExternalModule, FieldEntry, FieldValue, ModulePackage,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use wasmtime::component::Linker;
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};

/// The bindings of `wit/adjustment.wit`.
mod bindings {
    wasmtime::component::bindgen!({
        path: "../../wit/adjustment.wit",
        world: "adjustment",
    });
}

use bindings::exports::luxforge::module::adjust::{Field, Value};

/// The most pixels one `apply-row` call is handed. A longer run is passed in pieces, each with its
/// own first column, which a pointwise unit cannot tell apart: the host's rows are at most 16384
/// pixels already, so this bounds what any caller copies into an instance.
pub const MAX_ROW_PIXELS: usize = 16384;
/// The most words `coefficients` may answer.
pub const MAX_WORDS: usize = 4096;
/// The most characters a `label` may answer.
pub const MAX_LABEL_CHARS: usize = 64;
/// The most instances of one module kept for reuse once their calls end.
pub const MAX_IDLE_INSTANCES: usize = 64;

/// What bounds one instance and one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most linear memory one instance may grow to, in bytes.
    pub memory_bytes: usize,
    /// The longest one call (instantiation, `coefficients`, `apply-row` or `label`) may run.
    pub deadline: Duration,
}

impl Limits {
    /// A 32 MiB memory, many times a row's 192 KiB of pixels, and one second a call, many times
    /// what a row of arithmetic costs.
    pub const DEFAULT: Self = Self {
        memory_bytes: 32 * 1024 * 1024,
        deadline: Duration::from_secs(1),
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where a loaded module's compiled code came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compiled {
    /// Read from the cache.
    Cached,
    /// Compiled, and written to the cache.
    Compiled,
    /// Compiled, with no cache directory or a cache that could not be written.
    Uncached,
}

/// Loads packages into modules: the compiled-code cache's directory and the limits every module
/// it loads runs under.
#[derive(Clone, Debug)]
pub struct WasmHost {
    cache: Option<PathBuf>,
    limits: Limits,
}

impl WasmHost {
    /// A host caching compiled code in `cache`, created when first written, or compiling every
    /// load with none.
    pub fn new(cache: Option<PathBuf>) -> Self {
        Self {
            cache,
            limits: Limits::DEFAULT,
        }
    }

    /// The same host with these limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The registry's external module for each package, in order: loaded, or unavailable with the
    /// reason it did not load ([`RegistryOptions::external`](luxforge_core::RegistryOptions)).
    pub fn externals(&self, packages: &[ModulePackage]) -> Vec<ExternalModule> {
        packages
            .iter()
            .filter_map(|package| self.external(package).ok())
            .collect()
    }

    /// The registry's external module for `package`. Only a manifest whose table builds no module,
    /// which a package that read cannot hold, is an error.
    pub fn external(&self, package: &ModulePackage) -> Result<ExternalModule, Error> {
        match self.loaded(package) {
            Ok(adjustment) => {
                ExternalModule::loaded(&package.manifest, Arc::new(adjustment).units())
            }
            Err(reason) => ExternalModule::unavailable(&package.manifest, reason),
        }
    }

    /// `package`'s component, loaded, or a validation error naming the module and why it did not
    /// load. The returned module runs its functions directly, as the registry's units do, for a
    /// harness that holds a module to a native implementation.
    pub fn load(&self, package: &ModulePackage) -> Result<Arc<Adjustment>, Error> {
        let id = &package.manifest.id;
        self.loaded(package).map(Arc::new).map_err(|reason| {
            Error::validation(format!("module {id} did not load: {reason}"))
                .with_data(serde_json::json!({"module": id, "failure": "load"}))
        })
    }

    fn loaded(&self, package: &ModulePackage) -> Result<Adjustment, String> {
        let engine = engine::engine()?;
        let (component, compiled) = cache::component(
            engine,
            self.cache.as_deref(),
            &package.hash,
            &package.component,
        )?;
        let imports: Vec<String> = component
            .component_type()
            .imports(engine)
            .map(|(name, _)| name.to_owned())
            .collect();
        if !imports.is_empty() {
            return Err(format!(
                "its component imports {}, and an adjustment module imports nothing",
                imports.join(", ")
            ));
        }
        let pre = Linker::<State>::new(engine)
            .instantiate_pre(&component)
            .map_err(|error| format!("its component does not instantiate: {error:#}"))?;
        let pre = bindings::AdjustmentPre::new(pre).map_err(|error| {
            format!("its component is not an adjustment module of this interface: {error:#}")
        })?;
        let mut identity = [0; 4];
        let digest = Sha256::digest(package.hash.as_bytes());
        for (word, bytes) in identity.iter_mut().zip(digest.chunks_exact(8)) {
            *word = u64::from_le_bytes(bytes.try_into().unwrap_or_default());
        }
        Ok(Adjustment {
            id: package.manifest.id.clone(),
            identity,
            compiled,
            limits: self.limits,
            ticks: engine::ticks(self.limits.deadline),
            engine,
            pre,
            idle: Mutex::new(Vec::new()),
        })
    }
}

/// A store's own state: its memory and instance limits.
struct State {
    limits: StoreLimits,
}

/// One instance and the store it lives in, used by one call at a time.
struct Worker {
    store: Store<State>,
    bindings: bindings::Adjustment,
}

/// A loaded module: its component, ready to instantiate, and its idle instances.
pub struct Adjustment {
    id: String,
    /// The package hash, digested: the part of every unit's identity that names the code.
    identity: [u64; 4],
    compiled: Compiled,
    limits: Limits,
    ticks: u64,
    engine: &'static Engine,
    pre: bindings::AdjustmentPre<State>,
    idle: Mutex<Vec<Worker>>,
}

impl std::fmt::Debug for Adjustment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adjustment")
            .field("id", &self.id)
            .field("compiled", &self.compiled)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Adjustment {
    /// The module's identity, its manifest's `id`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Where its compiled code came from.
    pub fn compiled(&self) -> Compiled {
        self.compiled
    }

    /// This module as the units of its manifest's module
    /// ([`ModuleManifest::module`](luxforge_core::module_package::ModuleManifest::module)): each
    /// layer compiles to one [`Unit`] holding the words its values give.
    pub fn units(self: &Arc<Self>) -> Arc<dyn AdjustmentUnits> {
        Arc::new(unit::Units(self.clone()))
    }

    /// The instances kept for reuse now: at most one for each call that ran at once, and at most
    /// [`MAX_IDLE_INSTANCES`].
    pub fn idle_instances(&self) -> usize {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The component's words for these values: `coefficients`, at most [`MAX_WORDS`] of them. The
    /// component's own refusal is a validation error with its text.
    pub fn coefficients(&self, values: &[FieldEntry]) -> Result<Vec<u32>, Error> {
        let fields = fields(values);
        let answer = self.call("coefficients", |store, adjust| {
            adjust.call_coefficients(store, &fields)
        })?;
        let words = answer.map_err(|text| {
            Error::validation(format!("module {} refused the layer: {text}", self.id))
                .with_data(serde_json::json!({"module": self.id, "failure": "refused"}))
        })?;
        if words.len() > MAX_WORDS {
            return Err(self.failure(
                "length",
                format!(
                    "answered {} words, more than the {MAX_WORDS} a unit holds",
                    words.len()
                ),
            ));
        }
        Ok(words)
    }

    /// The component's CPU reference over one contiguous run of row `y` from column `x0`, in
    /// place: `apply-row`, in pieces of at most [`MAX_ROW_PIXELS`]. A failure leaves `rgb`
    /// unspecified; the caller produces no frame from it.
    pub fn apply_row(
        &self,
        words: &[u32],
        y: u32,
        x0: u32,
        rgb: &mut [[f32; 3]],
    ) -> Result<(), Error> {
        let mut x = x0;
        for piece in rgb.chunks_mut(MAX_ROW_PIXELS) {
            let input = piece.as_flattened_mut();
            let output = self.call("apply-row", |store, adjust| {
                adjust.call_apply_row(store, words, y, x, input)
            })?;
            if output.len() != input.len() {
                return Err(self.failure(
                    "length",
                    format!(
                        "answered {} values for a row of {}",
                        output.len(),
                        input.len()
                    ),
                ));
            }
            input.copy_from_slice(&output);
            x = x.saturating_add(piece.len() as u32);
        }
        Ok(())
    }

    /// The component's history summary for these values, `label`, of at most
    /// [`MAX_LABEL_CHARS`] characters.
    pub fn label(&self, values: &[FieldEntry]) -> Result<Option<String>, Error> {
        let fields = fields(values);
        let label = self.call("label", |store, adjust| adjust.call_label(store, &fields))?;
        if let Some(label) = &label
            && label.chars().count() > MAX_LABEL_CHARS
        {
            return Err(self.failure(
                "length",
                format!("answered a label longer than {MAX_LABEL_CHARS} characters"),
            ));
        }
        Ok(label)
    }

    /// Run `call` on an idle instance, or a new one, within the deadline. An instance the call
    /// failed in is dropped; one it succeeded in is kept for the next call.
    fn call<R>(
        &self,
        function: &str,
        call: impl FnOnce(
            &mut Store<State>,
            &bindings::exports::luxforge::module::adjust::Guest,
        ) -> wasmtime::Result<R>,
    ) -> Result<R, Error> {
        let _running = engine::enter(self.engine);
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let mut worker = match idle {
            Some(worker) => worker,
            None => self.instantiate()?,
        };
        worker.store.set_epoch_deadline(self.ticks);
        let answer = call(&mut worker.store, worker.bindings.luxforge_module_adjust())
            .map_err(|error| self.classify(function, &error))?;
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < MAX_IDLE_INSTANCES {
            idle.push(worker);
        }
        Ok(answer)
    }

    /// A new instance in a store of its own, under the limits.
    fn instantiate(&self) -> Result<Worker, Error> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .memories(1)
            .instances(16)
            .tables(16)
            .table_elements(65536)
            .build();
        let mut store = Store::new(self.engine, State { limits });
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(self.ticks);
        let bindings = self
            .pre
            .instantiate(&mut store)
            .map_err(|error| self.classify("instantiation", &error))?;
        Ok(Worker { store, bindings })
    }

    /// A failed call as the error a render reports.
    fn classify(&self, function: &str, error: &wasmtime::Error) -> Error {
        match error.downcast_ref::<Trap>() {
            Some(Trap::Interrupt) => self.failure(
                "deadline",
                format!(
                    "overran its {} ms deadline in {function}",
                    self.limits.deadline.as_millis()
                ),
            ),
            Some(trap) => {
                let trap = trap.to_string();
                let trap = trap.strip_prefix("wasm trap: ").unwrap_or(&trap);
                self.failure("trap", format!("trapped in {function}: {trap}"))
            }
            None => self.failure("invalid", format!("failed in {function}: {error:#}")),
        }
    }

    /// The error of a call that failed with `failure` (a stable code) and `detail`.
    fn failure(&self, failure: &str, detail: String) -> Error {
        Error::render(format!("module {} {detail}", self.id))
            .with_data(serde_json::json!({"module": self.id, "failure": failure}))
    }
}

/// The values as the component's `field` records.
fn fields(values: &[FieldEntry]) -> Vec<Field> {
    values
        .iter()
        .map(|entry| Field {
            name: entry.name.clone(),
            value: match &entry.value {
                FieldValue::Number(value) => Value::Number(*value),
                FieldValue::Integer(value) => Value::Integer(*value),
                FieldValue::Boolean(value) => Value::Boolean(*value),
                FieldValue::Choice(value) => Value::Choice(value.clone()),
                FieldValue::Color([r, g, b]) => Value::Color((*r, *g, *b)),
            },
        })
        .collect()
}
