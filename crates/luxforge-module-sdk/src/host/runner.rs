//! A module's component run through Wasmtime, configured as the module engine is: the component
//! model through Cranelift, relaxed SIMD and threads off, NaN canonicalization on, no imports,
//! a fixed memory limit per instance and a fuel budget per call.
//!
//! TASK-006's `luxforge-wasm` is the engine the host renders with and bounds a call by an epoch
//! deadline; this runner bounds one by fuel, which needs no ticking thread. Both stop a call that
//! runs away, and neither changes a result. Once `luxforge-wasm` lands, this runner should be
//! replaced by its engine so the harness and `module-check` run exactly what the host runs.
use super::Error;
use crate::{Field, Value};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../wit/adjustment.wit",
        world: "adjustment",
    });
}

use bindings::exports::luxforge::module::adjust;

/// The most linear memory one instance may grow to.
pub const MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// The fuel one call may burn, about one unit per WebAssembly instruction: far beyond what a row
/// of a photograph needs, and finite, so a call that loops forever stops.
pub const FUEL_PER_CALL: u64 = 20_000_000_000;

fn config() -> Config {
    let mut config = Config::new();
    config
        .wasm_component_model(true)
        .wasm_relaxed_simd(false)
        .wasm_features(wasmtime::WasmFeatures::THREADS, false)
        .cranelift_nan_canonicalization(true)
        .consume_fuel(true);
    config
}

/// A compiled component, checked to import nothing, from which instances are made.
pub struct Runner {
    engine: Engine,
    component: Component,
}

impl Runner {
    /// Compile `component`, refusing one that is not a component or imports anything.
    pub fn new(component: &[u8]) -> Result<Self, Error> {
        let engine = Engine::new(&config())
            .map_err(|error| format!("the module engine does not start: {error:?}"))?;
        let component = Component::new(&engine, component)
            .map_err(|error| format!("module.wasm is not a component Wasmtime runs: {error:?}"))?;
        let imports: Vec<String> = component
            .component_type()
            .imports(&engine)
            .map(|(name, _)| format!("`{name}`"))
            .collect();
        if !imports.is_empty() {
            return Err(format!(
                "the component imports {}; a module imports nothing",
                imports.join(", ")
            ));
        }
        Ok(Self { engine, component })
    }

    /// A fresh instance, with its own memory.
    pub fn instance(&self) -> Result<Instance, Error> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES)
            .instances(1)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        let linker = Linker::new(&self.engine);
        let bindings = bindings::Adjustment::instantiate(&mut store, &self.component, &linker)
            .map_err(|error| format!("the component does not instantiate: {error:?}"))?;
        Ok(Instance { store, bindings })
    }
}

/// One instance of a module. A call that traps leaves it unusable: make another.
pub struct Instance {
    store: Store<StoreLimits>,
    bindings: bindings::Adjustment,
}

fn lower(values: &[Field]) -> Vec<adjust::Field> {
    values
        .iter()
        .map(|field| adjust::Field {
            name: field.name.clone(),
            value: match &field.value {
                Value::Number(number) => adjust::Value::Number(*number),
                Value::Integer(integer) => adjust::Value::Integer(*integer),
                Value::Boolean(boolean) => adjust::Value::Boolean(*boolean),
                Value::Choice(choice) => adjust::Value::Choice(choice.clone()),
                Value::Color([r, g, b]) => adjust::Value::Color((*r, *g, *b)),
            },
        })
        .collect()
}

impl Instance {
    fn refuel(&mut self) -> Result<(), Error> {
        self.store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|error| format!("cannot set the call's fuel: {error:?}"))
    }

    /// `coefficients`: the module's words, or its refusal; the outer error is a trap.
    pub fn coefficients(&mut self, values: &[Field]) -> Result<Result<Vec<u32>, String>, Error> {
        self.refuel()?;
        self.bindings
            .luxforge_module_adjust()
            .call_coefficients(&mut self.store, &lower(values))
            .map_err(|error| format!("coefficients trapped: {error:?}"))
    }

    /// `apply-row` over `rgb`, refusing an answer of another length.
    pub fn apply_row(
        &mut self,
        words: &[u32],
        y: u32,
        x0: u32,
        rgb: &[f32],
    ) -> Result<Vec<f32>, Error> {
        self.refuel()?;
        let answer = self
            .bindings
            .luxforge_module_adjust()
            .call_apply_row(&mut self.store, words, y, x0, rgb)
            .map_err(|error| format!("apply-row trapped: {error:?}"))?;
        if answer.len() != rgb.len() {
            return Err(format!(
                "apply-row answered {} values for a row of {}",
                answer.len(),
                rgb.len()
            ));
        }
        Ok(answer)
    }

    /// `label`: the module's history summary, if it gives one.
    pub fn label(&mut self, values: &[Field]) -> Result<Option<String>, Error> {
        self.refuel()?;
        self.bindings
            .luxforge_module_adjust()
            .call_label(&mut self.store, &lower(values))
            .map_err(|error| format!("label trapped: {error:?}"))
    }
}
