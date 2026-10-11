//! An opened model: an ONNX Runtime session on the chosen backend, and on Core ML the CPU session
//! of the original that takes the shapes the fixed-shape copy does not.
use crate::{
    Backend, OnnxRuntime, cache, cancel, convert,
    ffi::{self, Assigned},
    fixed, runtime_error,
};
use luxforge_core::{
    Cancel, Error,
    inference::{Device, ModelFile, ModelIdentity, Placement, Precision, Tensor, TensorSpec},
};
use ort::session::{
    Session as OrtSession,
    builder::{GraphOptimizationLevel, SessionBuilder},
};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

/// A session builder at the basic optimization level that records its execution-provider
/// assignment. Basic, as the prototype and darktable found: the extended fusions break the SAM 2
/// decoder's shape inference.
pub(crate) fn builder() -> Result<SessionBuilder, Error> {
    let failed = |error: ort::Error| {
        runtime_error("ONNX Runtime could not configure a session", error, None)
    };
    OrtSession::builder()
        .map_err(failed)?
        .with_optimization_level(GraphOptimizationLevel::Level1)
        .map_err(|error| failed(error.into()))?
        .with_config_entry(ffi::RECORD_ASSIGNMENT, "1")
        .map_err(|error| failed(error.into()))
}

/// A builder for `backend` at `precision`, with Core ML's compile cache at `cache` when given.
fn backend_builder(
    backend: Backend,
    precision: Precision,
    cache: Option<&Path>,
) -> Result<SessionBuilder, Error> {
    let failed = |error: ort::Error| {
        runtime_error(
            &format!(
                "ONNX Runtime could not configure the {} backend",
                backend.name()
            ),
            error,
            None,
        )
    };
    let builder = builder()?;
    match backend {
        Backend::Cpu => Ok(builder),
        Backend::CoreMl => coreml(builder, precision, cache).map_err(failed),
        Backend::DirectMl => directml(builder).map_err(failed),
    }
}

/// The Core ML provider on the CPU and GPU only: the Neural Engine computes in fp16, which changed
/// the prototype's masks, and is not used until a model passes an fp16 parity check. MLProgram,
/// static input shapes only, and low-precision accumulation on the GPU only for an fp16 model.
#[cfg(target_os = "macos")]
fn coreml(
    builder: SessionBuilder,
    precision: Precision,
    cache: Option<&Path>,
) -> Result<SessionBuilder, ort::Error> {
    let mut coreml = ort::ep::CoreML::default()
        .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
        .with_compute_units(ort::ep::coreml::ComputeUnits::CPUAndGPU)
        .with_static_input_shapes(true)
        .with_low_precision_accumulation_on_gpu(precision == Precision::Fp16);
    if let Some(cache) = cache {
        coreml = coreml.with_model_cache_dir(cache.display());
    }
    Ok(builder.with_execution_providers([coreml.build().error_on_failure()])?)
}

#[cfg(not(target_os = "macos"))]
fn coreml(
    _builder: SessionBuilder,
    _precision: Precision,
    _cache: Option<&Path>,
) -> Result<SessionBuilder, ort::Error> {
    Err(ort::Error::new("this build has no Core ML provider"))
}

/// The DirectML provider, which runs neither memory patterns nor parallel execution.
#[cfg(target_os = "windows")]
fn directml(builder: SessionBuilder) -> Result<SessionBuilder, ort::Error> {
    Ok(builder
        .with_memory_pattern(false)?
        .with_parallel_execution(false)?
        .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])?)
}

#[cfg(not(target_os = "windows"))]
fn directml(_builder: SessionBuilder) -> Result<SessionBuilder, ort::Error> {
    Err(ort::Error::new("this build has no DirectML provider"))
}

/// Commit `builder` over the model file at `path`, cancelled mid-load when `cancel` fires.
fn commit(
    builder: SessionBuilder,
    path: &Path,
    model: &ModelIdentity,
    backend: Backend,
    cancel: &Cancel,
) -> Result<OrtSession, Error> {
    let canceler = builder.canceler();
    let mut builder = builder;
    let session =
        cancel::watching(cancel, canceler, || builder.commit_from_file(path)).map_err(|error| {
            runtime_error(
                &format!(
                    "model {} could not be opened on {}",
                    model.id,
                    backend.name()
                ),
                error,
                Some(cancel),
            )
        })?;
    cancel.check()?;
    Ok(session)
}

/// Where `session`'s graph runs, from ONNX Runtime's record of its assignment: the CPU when no
/// node is on the backend's accelerator, the accelerator's device when every node is, and `mixed`
/// when the graph is split between them; `partitions` counts the accelerator's parts, each of
/// which ONNX Runtime records as one fused subgraph. The CPU backend runs the whole graph on the
/// CPU, unpartitioned (ONNX Runtime records each of its nodes as a part of its own). When the
/// record cannot be read, an accelerator's placement is `mixed` with no partition count: it is not
/// known what it took.
pub(crate) fn placement(
    backend: Backend,
    assigned: Option<&[Assigned]>,
    precision: Precision,
) -> Placement {
    let provider = backend.provider();
    let (device, partitions) = match (backend, assigned) {
        (Backend::Cpu, _) => (Device::Cpu, None),
        (_, None) => (Device::Mixed, None),
        (_, Some(assigned)) => {
            let on: Vec<_> = assigned
                .iter()
                .filter(|part| part.provider == provider)
                .collect();
            let elsewhere: usize = assigned
                .iter()
                .filter(|part| part.provider != provider)
                .map(|part| part.nodes)
                .sum();
            let taken: usize = on.iter().map(|part| part.nodes).sum();
            let device = if taken == 0 {
                Device::Cpu
            } else if elsewhere == 0 {
                Device::Gpu
            } else {
                Device::Mixed
            };
            (device, Some(on.len() as u32))
        }
    };
    Placement {
        backend: backend.name().to_owned(),
        device,
        precision,
        partitions,
    }
}

/// The opened model. See the [module documentation](self).
pub(crate) struct OnnxSession {
    model: ModelIdentity,
    placement: Placement,
    inputs: Vec<TensorSpec>,
    outputs: Vec<TensorSpec>,
    state: Mutex<State>,
}

struct State {
    session: OrtSession,
    /// Set when the session runs the fixed-shape copy.
    fixed: Option<Fixed>,
}

/// A fixed-shape session's input shapes, and the CPU session of the original that runs the others.
struct Fixed {
    shapes: Vec<(String, Vec<u64>)>,
    original: PathBuf,
    flexible: Option<OrtSession>,
}

impl OnnxSession {
    /// Which session runs `inputs`.
    fn route<'s>(
        &self,
        state: &'s mut State,
        inputs: &[Tensor],
        cancel: &Cancel,
    ) -> Result<&'s mut OrtSession, Error> {
        let Some(fixed) = &mut state.fixed else {
            return Ok(&mut state.session);
        };
        let taken = inputs.iter().all(|tensor| {
            fixed
                .shapes
                .iter()
                .any(|(name, shape)| name == tensor.name() && shape == tensor.shape())
        });
        if taken {
            return Ok(&mut state.session);
        }
        if fixed.flexible.is_none() {
            let builder = backend_builder(Backend::Cpu, self.placement.precision, None)?;
            fixed.flexible = Some(commit(
                builder,
                &fixed.original,
                &self.model,
                Backend::Cpu,
                cancel,
            )?);
        }
        Ok(fixed.flexible.as_mut().expect("opened above"))
    }

    /// Every declared input exactly once, each as its spec admits.
    fn check(&self, inputs: &[Tensor]) -> Result<(), Error> {
        for (index, tensor) in inputs.iter().enumerate() {
            let spec = self
                .inputs
                .iter()
                .find(|spec| spec.name == tensor.name())
                .ok_or_else(|| {
                    Error::validation(format!(
                        "model {} has no input {}; it takes {}",
                        self.model.id,
                        tensor.name(),
                        names(&self.inputs)
                    ))
                })?;
            if inputs[..index]
                .iter()
                .any(|seen| seen.name() == tensor.name())
            {
                return Err(Error::validation(format!(
                    "input {} is given twice",
                    tensor.name()
                )));
            }
            if !spec.admits(tensor) {
                return Err(Error::validation(format!(
                    "model {}'s input {} takes {:?} of shape {}, not {:?} of shape {:?}",
                    self.model.id,
                    spec.name,
                    spec.element,
                    shape_text(&spec.shape),
                    tensor.data().element(),
                    tensor.shape()
                )));
            }
        }
        if inputs.len() != self.inputs.len() {
            let missing: Vec<_> = self
                .inputs
                .iter()
                .filter(|spec| !inputs.iter().any(|tensor| tensor.name() == spec.name))
                .map(|spec| spec.name.as_str())
                .collect();
            return Err(Error::validation(format!(
                "model {} needs input {}",
                self.model.id,
                missing.join(", ")
            )));
        }
        Ok(())
    }
}

fn names(specs: &[TensorSpec]) -> String {
    specs
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn shape_text(shape: &[Option<u64>]) -> String {
    let sides: Vec<_> = shape
        .iter()
        .map(|side| side.map_or("?".to_owned(), |side| side.to_string()))
        .collect();
    format!("[{}]", sides.join(", "))
}

impl luxforge_core::inference::Session for OnnxSession {
    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn placement(&self) -> &Placement {
        &self.placement
    }

    fn inputs(&self) -> &[TensorSpec] {
        &self.inputs
    }

    fn outputs(&self) -> &[TensorSpec] {
        &self.outputs
    }

    fn run(&self, inputs: &[Tensor], cancel: &Cancel) -> Result<Vec<Tensor>, Error> {
        cancel.check()?;
        self.check(inputs)?;
        let values = inputs
            .iter()
            .map(|tensor| Ok((tensor.name(), convert::input(tensor)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        // ONNX Runtime runs a session through `&mut`: one run at a time per opened model.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let session = self.route(&mut state, inputs, cancel)?;
        let mut answered = session.run(values).map_err(|error| {
            runtime_error(
                &format!("model {} failed to run", self.model.id),
                error,
                Some(cancel),
            )
        })?;
        cancel.check()?;
        self.outputs
            .iter()
            .map(|spec| {
                let value = answered.remove(&spec.name).ok_or_else(|| {
                    Error::internal(format!(
                        "model {} answered no output {}",
                        self.model.id, spec.name
                    ))
                })?;
                convert::output(&spec.name, &value)
            })
            .collect()
    }
}

/// Open `model` on `backend`. See the [crate documentation](crate).
pub(crate) fn open(
    runtime: &OnnxRuntime,
    model: &ModelFile,
    backend: Backend,
    precision: Precision,
    cancel: &Cancel,
) -> Result<OnnxSession, Error> {
    let identity = model.identity().clone();
    let path = model.path();
    let (session, specs, fixed) = match backend {
        Backend::Cpu | Backend::DirectMl => {
            let session = commit(
                backend_builder(backend, precision, None)?,
                path,
                &identity,
                backend,
                cancel,
            )?;
            let specs = (
                convert::specs(session.inputs())?,
                convert::specs(session.outputs())?,
            );
            (session, specs, None)
        }
        Backend::CoreMl => {
            let record = fixed::ensure(model, cancel)?;
            let cache = runtime.cache_dir().and_then(|root| {
                cache::compile_dir(root, &identity.sha256, &crate::ffi::version())
            });
            let opened = record.opened(path);
            let session = match commit(
                backend_builder(backend, precision, cache.as_deref())?,
                &opened,
                &identity,
                backend,
                cancel,
            ) {
                Ok(session) => session,
                // A compile cache Core ML can no longer load (one written before the system
                // changed under the same key) is dropped and compiled afresh, once.
                Err(error) if cache.as_ref().is_some_and(|dir| dir.exists()) => {
                    cancel.check()?;
                    let dir = cache.as_deref().expect("checked above");
                    let _ = std::fs::remove_dir_all(dir);
                    commit(
                        backend_builder(backend, precision, Some(dir))?,
                        &opened,
                        &identity,
                        backend,
                        cancel,
                    )
                    .map_err(|_| error)?
                }
                Err(error) => return Err(error),
            };
            let shapes = record.fixed_shapes();
            // The copy's inputs are the shapes the record says it fixed.
            for outlet in session.inputs() {
                let spec = convert::spec(outlet)?;
                let fixed = shapes
                    .iter()
                    .find(|(name, _)| *name == spec.name)
                    .map(|(_, shape)| shape.iter().copied().map(Some).collect::<Vec<_>>());
                if fixed.as_ref() != Some(&spec.shape) {
                    return Err(Error::incompatible(format!(
                        "the fixed-shape copy of model {} takes input {} as {}, not as recorded",
                        identity.id,
                        spec.name,
                        shape_text(&spec.shape)
                    )));
                }
            }
            let fixed = record.copy.is_some().then(|| Fixed {
                shapes,
                original: path.to_path_buf(),
                flexible: None,
            });
            (session, (record.inputs, record.outputs), fixed)
        }
    };
    let assigned = ffi::assignment(&session).ok();
    let placement = placement(backend, assigned.as_deref(), precision);
    let (inputs, outputs) = specs;
    Ok(OnnxSession {
        model: identity,
        placement,
        inputs,
        outputs,
        state: Mutex::new(State { session, fixed }),
    })
}
