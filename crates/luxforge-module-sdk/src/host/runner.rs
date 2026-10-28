//! A module's component run on the module engine itself, `luxforge-wasm`'s [`WasmHost`]: the same
//! Wasmtime configuration, memory limit and epoch deadline the host renders with, so the harness and
//! `module-check` run exactly what the host runs.
use super::Error;
use crate::{Field, Value};
use luxforge_core::module_package::{FieldEntry, FieldValue, ModulePackage};
use luxforge_wasm::{Adjustment, WasmHost};
use std::cell::Cell;
use std::sync::Arc;

/// A component, checked to import nothing and to export the `adjust` interface, from which
/// instances are made.
pub struct Runner {
    id: String,
    component: Vec<u8>,
    /// The module the check loaded, which the first instance takes.
    loaded: Cell<Option<Arc<Adjustment>>>,
}

impl Runner {
    /// Load `component` under the name `id`, refusing one that is not a component of the
    /// interface or imports anything.
    pub fn new(id: &str, component: &[u8]) -> Result<Self, Error> {
        let loaded = load(id, component)?;
        Ok(Self {
            id: id.to_owned(),
            component: component.to_vec(),
            loaded: Cell::new(Some(loaded)),
        })
    }

    /// A package's component, under its manifest's `id`.
    pub fn package(package: &ModulePackage) -> Result<Self, Error> {
        Self::new(&package.manifest.id, &package.component)
    }

    /// A fresh instance, with its own memory: the module loaded again, so nothing an earlier
    /// instance ran is reused. The engine instantiates it on its first call.
    pub fn instance(&self) -> Result<Instance, Error> {
        let adjustment = match self.loaded.take() {
            Some(adjustment) => adjustment,
            None => load(&self.id, &self.component)?,
        };
        Ok(Instance { adjustment })
    }
}

fn load(id: &str, component: &[u8]) -> Result<Arc<Adjustment>, Error> {
    WasmHost::new(None)
        .load_component(id, component)
        .map_err(|error| error.detail)
}

/// One instance of a module. A call that fails drops the engine's instance, and the next call
/// makes another.
pub struct Instance {
    adjustment: Arc<Adjustment>,
}

/// The values as the engine's field entries.
pub fn entries(values: &[Field]) -> Vec<FieldEntry> {
    values
        .iter()
        .map(|field| FieldEntry {
            name: field.name.clone(),
            value: match &field.value {
                Value::Number(number) => FieldValue::Number(*number),
                Value::Integer(integer) => FieldValue::Integer(*integer),
                Value::Boolean(boolean) => FieldValue::Boolean(*boolean),
                Value::Choice(choice) => FieldValue::Choice(choice.clone()),
                Value::Color(color) => FieldValue::Color(*color),
            },
        })
        .collect()
}

impl Instance {
    /// `coefficients`: the module's words, or its refusal; the outer error is a failure (a trap,
    /// the deadline, too many words).
    pub fn coefficients(&mut self, values: &[Field]) -> Result<Result<Vec<u32>, String>, Error> {
        self.adjustment
            .answer_coefficients(&entries(values))
            .map_err(|error| error.detail)
    }

    /// `apply-row` over `rgb`, three channels a pixel, refusing an answer of another length.
    pub fn apply_row(
        &mut self,
        words: &[u32],
        y: u32,
        x0: u32,
        rgb: &[f32],
    ) -> Result<Vec<f32>, Error> {
        let (pixels, rest) = rgb.as_chunks::<3>();
        if !rest.is_empty() {
            return Err(format!(
                "a row of {} values is not whole pixels of three channels",
                rgb.len()
            ));
        }
        let mut pixels = pixels.to_vec();
        self.adjustment
            .apply_row(words, y, x0, &mut pixels)
            .map_err(|error| error.detail)?;
        Ok(pixels.into_iter().flatten().collect())
    }

    /// `label`: the module's history summary, if it gives one.
    pub fn label(&mut self, values: &[Field]) -> Result<Option<String>, Error> {
        self.adjustment
            .label(&entries(values))
            .map_err(|error| error.detail)
    }
}
