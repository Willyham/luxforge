//! tract backends (`tract-cpu`, `tract-metal`) behind `runtime::Session`.
//!
//! `open` only gets a model path, not the shapes a caller will run it with, and every pinned
//! model has at least one input whose declared shape is dynamic (`batch_size`, `num_points`, or
//! both). tract needs a fully concrete `TypedModel` to optimize and run, so shapes are
//! concretized lazily: the first `run` call for a given combination of input shapes builds a
//! plan from that call's own tensors and caches it, keyed by those shapes. Every later call with
//! the same shapes reuses the cached plan; a call with new shapes (a different point count, say)
//! builds and caches another one. `open`'s cost is parsing the ONNX file and reading its
//! declared input and output facts; the concretize-optimize-build-plan cost is charged to
//! whichever `run` first needs that shape (see `commands::parity`, which reports load and
//! first-run times separately for exactly this reason).
//!
//! The Metal backend is built by hand from `tract_metal::MetalTransform` rather than through
//! tract's `runtime_for_name("metal")` registry: that registry is populated by an `inventory`
//! submission in `tract-metal`, which only runs if the crate is linked in, and `tract-metal`'s
//! own code notes (`ops::pool::link_translators`, called from `MetalTransform::transform`) that
//! a module nothing else references can be dropped by the linker along with its registrations.
//! Calling `MetalTransform` directly is the same sequence `tract_metal::MetalRuntime` uses
//! internally (transform, then `into_optimized`) and needs no registry lookup to be linked.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tract_onnx::prelude::*;
use tract_onnx::tract_core::transform::ModelTransform;

use crate::runtime::{Backend, Io, Session};
use crate::tensor::{Data, Tensor as OurTensor};

/// `{e:#}` walks a `TractError`'s full `anyhow`-style context chain (each `.context(...)` tract
/// added on the way up), not just its top frame, which is what actually says which node and op
/// failed and why.
fn err(e: impl std::fmt::Display) -> String {
    format!("tract: {e:#}")
}

/// One input shape signature: the model's declared input order, each as that call's concrete
/// dimensions.
type ShapeKey = Vec<Vec<usize>>;

pub struct TractSession {
    backend: Backend,
    /// The parsed model, re-specialized (cloned, then given concrete input facts) for each new
    /// shape signature seen at `run`.
    template: InferenceModel,
    /// Input names in the model's declared order.
    input_names: Vec<String>,
    /// Output names in the model's declared order.
    output_names: Vec<String>,
    inputs: Vec<Io>,
    outputs: Vec<Io>,
    plans: HashMap<ShapeKey, Arc<TypedRunnableModel>>,
}

impl TractSession {
    pub fn open(backend: Backend, model: &Path) -> Result<Self, String> {
        let mut template = tract_onnx::onnx().model_for_path(model).map_err(err)?;
        // Best-effort: resolves every input and output fact determinable from the model's own
        // fixed dims (e.g. the SAM 2.1 encoder, whose inputs and outputs are all fixed) so
        // `inputs()`/`outputs()` are informative. A model with genuinely dynamic dims (batch,
        // point count) keeps those symbolic here regardless; `run` concretizes them per call.
        let _ = template.analyse(false);
        let input_outlets = template.input_outlets().map_err(err)?.to_vec();
        let output_outlets = template.output_outlets().map_err(err)?.to_vec();

        let input_names: Vec<String> = input_outlets
            .iter()
            .map(|o| template.node(o.node).name.clone())
            .collect();
        let output_names: Vec<String> = output_outlets
            .iter()
            .map(|&o| {
                template
                    .outlet_label(o)
                    .map(str::to_string)
                    .unwrap_or_else(|| template.node(o.node).name.clone())
            })
            .collect();

        let describe = |o: OutletId| -> String {
            template
                .outlet_fact(o)
                .map(|f| format!("{f:?}"))
                .unwrap_or_else(|e| format!("<no fact: {e}>"))
        };
        let inputs = input_names
            .iter()
            .zip(&input_outlets)
            .map(|(n, &o)| Io {
                name: n.clone(),
                description: describe(o),
            })
            .collect();
        let outputs = output_names
            .iter()
            .zip(&output_outlets)
            .map(|(n, &o)| Io {
                name: n.clone(),
                description: describe(o),
            })
            .collect();

        Ok(Self {
            backend,
            template,
            input_names,
            output_names,
            inputs,
            outputs,
            plans: HashMap::new(),
        })
    }

    /// Orders `inputs` (named, caller order) into the model's declared input order, erroring if
    /// one is missing.
    fn ordered<'a>(&self, inputs: &'a [(&str, &OurTensor)]) -> Result<Vec<&'a OurTensor>, String> {
        self.input_names
            .iter()
            .map(|name| {
                inputs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| *t)
                    .ok_or_else(|| format!("missing input {name:?}"))
            })
            .collect()
    }

    /// Builds (or reuses) the plan for this call's input shapes: sets a concrete fact on every
    /// input from the tensor it will actually run with, converts to a `TypedModel`, and runs the
    /// backend's optimization path.
    fn plan_for(&mut self, ordered: &[&OurTensor]) -> Result<Arc<TypedRunnableModel>, String> {
        let key: ShapeKey = ordered.iter().map(|t| t.shape.clone()).collect();
        if let Some(plan) = self.plans.get(&key) {
            return Ok(plan.clone());
        }

        let mut model = self.template.clone();
        for (ix, t) in ordered.iter().enumerate() {
            let sample = to_tract_tensor(t)?;
            model
                .set_input_fact(ix, InferenceFact::dt_shape_from_tensor(&sample))
                .map_err(err)?;
        }
        let mut typed: TypedModel = model.into_typed().map_err(err)?;

        let runnable: Arc<TypedRunnableModel> = match self.backend {
            Backend::TractCpu => typed
                .into_optimized()
                .map_err(err)?
                .into_runnable()
                .map_err(err)?,
            Backend::TractMetal => {
                tract_metal::MetalTransform::default()
                    .transform(&mut typed)
                    .map_err(err)?;
                typed
                    .into_optimized()
                    .map_err(err)?
                    .into_runnable()
                    .map_err(err)?
            }
            Backend::OrtCpu | Backend::OrtCoreMl(_) | Backend::OrtCoreMlFixed(_) => {
                unreachable!("TractSession is only opened for a tract backend")
            }
        };
        self.plans.insert(key, runnable.clone());
        Ok(runnable)
    }
}

impl Session for TractSession {
    fn backend(&self) -> Backend {
        self.backend
    }

    fn inputs(&self) -> &[Io] {
        &self.inputs
    }

    fn outputs(&self) -> &[Io] {
        &self.outputs
    }

    fn run(&mut self, inputs: &[(&str, &OurTensor)]) -> Result<Vec<(String, OurTensor)>, String> {
        let ordered = self.ordered(inputs)?;
        let plan = self.plan_for(&ordered)?;
        let values: TVec<TValue> = ordered
            .iter()
            .map(|t| Ok(to_tract_tensor(t)?.into_tvalue()))
            .collect::<Result<_, String>>()?;
        let outs = plan.run(values).map_err(err)?;
        self.output_names
            .iter()
            .cloned()
            .zip(outs.iter())
            .map(|(name, v)| Ok((name, from_tract_tensor(v)?)))
            .collect()
    }
}

fn to_tract_tensor(t: &OurTensor) -> Result<Tensor, String> {
    match &t.data {
        Data::F32(v) => Tensor::from_shape(&t.shape, v).map_err(err),
        Data::I64(v) => Tensor::from_shape(&t.shape, v).map_err(err),
        Data::I32(v) => Tensor::from_shape(&t.shape, v).map_err(err),
    }
}

fn from_tract_tensor(v: &TValue) -> Result<OurTensor, String> {
    let shape = v.shape().to_vec();
    Ok(match v.datum_type() {
        DatumType::F32 => OurTensor::f32(shape, v.view().as_slice::<f32>().map_err(err)?.to_vec()),
        DatumType::I64 => OurTensor::i64(shape, v.view().as_slice::<i64>().map_err(err)?.to_vec()),
        DatumType::I32 => OurTensor::i32(shape, v.view().as_slice::<i32>().map_err(err)?.to_vec()),
        other => return Err(format!("tract: unsupported output datum type {other:?}")),
    })
}
