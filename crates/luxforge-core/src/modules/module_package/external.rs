//! The seam between an external module's field-patch table, which the core owns, and its colour
//! arithmetic, which a WebAssembly engine outside the core runs: [`AdjustmentUnits`].
use crate::modules::field_patch::{FieldPatch, Spec, Values};
use crate::modules::{ColorOperation, ParameterDescriptor, ParameterKind, Processing, ToolModule};
use crate::{CompileStage, Error};
use serde_json::Value;
use std::sync::Arc;

/// One field's canonical value, as the `value` variant of `wit/adjustment.wit` carries it.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Number(f64),
    Integer(i64),
    Boolean(bool),
    /// An enum field's option.
    Choice(String),
    /// Three 8-bit sRGB channels.
    Color([u8; 3]),
}

/// One field of the table and its value, as the `field` record of `wit/adjustment.wit` carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldEntry {
    pub name: String,
    pub value: FieldValue,
}

/// What runs an external module's colour arithmetic: the engine behind an installed package.
///
/// The host owns everything else: it checks and canonicalizes every value, fills each field the
/// stored payload leaves out with its default, and never asks for a layer its values leave neutral
/// (every field at its default) except when a GPU plan compiles its drafted layer whole.
pub trait AdjustmentUnits: Send + Sync + 'static {
    /// The colour operation these values compile to at the layer's input stage: every field of the
    /// table in the manifest's order. An error refuses the layer and names the module.
    fn compile(&self, values: &[FieldEntry], at: CompileStage) -> Result<ColorOperation, Error>;
}

/// One installed package's module as an engine hands it to the registry assembly
/// ([`crate::RegistryOptions::external`]): the field-patch module of its manifest, loaded with the
/// engine's units, or registered unavailable with the loader's reason when its component did not
/// load. Either way it is a manifest's module, so the assembly registers nothing a package could
/// not describe; the registry registers externals after every linked module.
#[derive(Clone)]
pub struct ExternalModule {
    module: Arc<dyn ToolModule>,
    unavailable: Option<String>,
}

impl ExternalModule {
    /// `manifest`'s module, its colour arithmetic run by `units`.
    pub fn loaded(
        manifest: &super::ModuleManifest,
        units: Arc<dyn AdjustmentUnits>,
    ) -> Result<Self, Error> {
        Ok(Self {
            module: manifest.module(units)?,
            unavailable: None,
        })
    }

    /// `manifest`'s module, registered unavailable for `reason`: its descriptor, actions and effect
    /// are listed, a photo that uses it keeps every layer, and rendering it refuses with the
    /// unavailable effect, as for any missing provider.
    pub fn unavailable(
        manifest: &super::ModuleManifest,
        reason: impl Into<String>,
    ) -> Result<Self, Error> {
        let reason = reason.into();
        Ok(Self {
            module: manifest.module(Arc::new(Unloaded(reason.clone())))?,
            unavailable: Some(reason),
        })
    }

    /// The module's identity, the manifest's `id`.
    pub fn id(&self) -> &str {
        &self.module.descriptor().id
    }

    /// Why the module is registered unavailable, or `None` when it loaded.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub(crate) fn module(&self) -> &Arc<dyn ToolModule> {
        &self.module
    }
}

impl std::fmt::Debug for ExternalModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalModule")
            .field("id", &self.id())
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

/// The units of a module whose component did not load. The registry checks availability before it
/// compiles through a module, so nothing asks these; if something did, it is refused with the
/// loader's reason.
struct Unloaded(String);

impl AdjustmentUnits for Unloaded {
    fn compile(&self, _: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
        Err(Error::internal(format!(
            "an unloaded module was compiled: {}",
            self.0
        )))
    }
}

/// The field-patch module of an external manifest: the table's shared behaviour from the core, and
/// its compilation from the engine's units.
pub(super) struct ExternalPatch {
    units: Arc<dyn AdjustmentUnits>,
}

impl ExternalPatch {
    pub(super) fn new(units: Arc<dyn AdjustmentUnits>) -> Self {
        Self { units }
    }
}

/// One canonical value as its field's kind reads it. The host has checked it against the field's
/// declaration, so a mismatch is an internal defect, refused rather than guessed at.
fn entry(parameter: &ParameterDescriptor, value: &Value) -> Result<FieldEntry, Error> {
    let name = &parameter.name;
    let mismatch = || Error::internal(format!("field {name} holds {value}, not its kind"));
    let value = match &parameter.kind {
        ParameterKind::Number { .. } => FieldValue::Number(value.as_f64().ok_or_else(mismatch)?),
        ParameterKind::Integer { .. } => FieldValue::Integer(value.as_i64().ok_or_else(mismatch)?),
        ParameterKind::Boolean => FieldValue::Boolean(value.as_bool().ok_or_else(mismatch)?),
        ParameterKind::Enum { .. } => {
            FieldValue::Choice(value.as_str().ok_or_else(mismatch)?.to_owned())
        }
        ParameterKind::Color => {
            let channel = |at: usize| {
                value
                    .get(at)
                    .and_then(Value::as_u64)
                    .and_then(|channel| u8::try_from(channel).ok())
                    .ok_or_else(mismatch)
            };
            FieldValue::Color([channel(0)?, channel(1)?, channel(2)?])
        }
        _ => return Err(mismatch()),
    };
    Ok(FieldEntry {
        name: name.clone(),
        value,
    })
}

impl FieldPatch for ExternalPatch {
    /// An external module's spec is its manifest's, which `FieldPatchModule::from_spec` is given;
    /// it has no static table, so nothing asks it for one.
    fn spec() -> Spec {
        unreachable!("an external module is built from its manifest's spec")
    }

    fn compile(&self, values: &Values<'_>, at: CompileStage) -> Result<Processing, Error> {
        let values = values
            .entries()
            .map(|(parameter, value)| entry(parameter, value))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Processing::Color(self.units.compile(&values, at)?))
    }
}
