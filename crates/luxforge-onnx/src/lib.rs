//! The host's local model runtime: ONNX Runtime, statically linked through `ort`, behind the core's
//! inference port (`luxforge_core::inference`). The desktop and `luxforge-json` give it to the
//! catalog owner through `HostConfig::inference`, as `luxforge-net`'s transport is given; the core
//! and its test binaries link no runtime. See
//! `docs/design/ai-editing.md#the-inference-port-and-runtime`.
//!
//! # Backends
//!
//! [`OnnxRuntime`] offers the backends a `local-runtime` capability names, in this order:
//!
//! - `coreml` on macOS: ONNX Runtime's Core ML execution provider, in the MLProgram format, on the
//!   CPU and GPU compute units and never the Neural Engine, over the model's fixed-shape copy
//!   ([`fixed`](crate#the-fixed-shape-copy)), with nodes Core ML does not take on the CPU provider;
//! - `directml` on Windows: the DirectML execution provider from the operating system's copy, with
//!   the CPU provider for the rest;
//! - `cpu` everywhere: the CPU execution provider alone.
//!
//! An open takes the first of the capability's backends this runtime offers
//! ([`luxforge_core::inference::OpenOptions::choose`]) and reports a real placement: which
//! provider each part of the graph was assigned to, read from ONNX Runtime's own record of the
//! assignment, as the device the work lands on and the accelerator's partition count. Every graph
//! is optimized at ONNX Runtime's basic level, which the prototype found the extended fusions of
//! some decoders need.
//!
//! # Precision
//!
//! The runtime runs a model's graph as written, at the precision its capability declares and the
//! placement reports. The one setting it changes is Core ML's: an `fp32` model may not accumulate
//! at low precision on the GPU, and only an `fp16` one may. The Neural Engine is never asked for,
//! since it computes in fp16 and changed the prototype's masks.
//!
//! # The fixed-shape copy
//!
//! Core ML takes only nodes whose input shapes are static, so it runs none of a model that declares
//! a free dimension such as `batch_size` or `num_points`. The `coreml` backend opens a copy of the
//! model with every named free input dimension fixed at 1, which ONNX Runtime itself writes: a CPU
//! session with free-dimension overrides saves the graph it optimized at the basic level. The copy
//! and its record (`<model>.coreml-fixed.onnx` and `<model>.coreml-fixed.json`) are written once,
//! in the model's own directory, by [`luxforge_core::inference::InferenceRuntime::prepare`] after an
//! install or by the first open, and reused while the record matches the model's hash, this
//! runtime's version and the copy's own length and hash. A run whose input shapes the copy does not
//! take (a batch of prompts) runs on a CPU session of the original, opened the first time one
//! arrives. A model without free dimensions is recorded as taken whole and needs no copy.
//!
//! # The Core ML compile cache
//!
//! Core ML compiles a model on its first load. The compiled model is kept under the application
//! data root (`<data>/inference/coreml/<sha256>-onnxruntime-<version>-<os build>/`), keyed by the
//! model's hash, this runtime's version and the macOS build, so a model, runtime or system update
//! compiles afresh and never loads a stale compile; opening a key removes the model's other keys.
//! Without a data root, or when the macOS build cannot be read, Core ML compiles on every open.
//!
//! # Cancellation
//!
//! An open is cancelled at once, mid-load, through ONNX Runtime's load canceller: the first load of
//! a model on Core ML can take seconds. A run checks its cancellation before it starts and after it
//! ends, and answers `cancelled`, never a partial output.
use luxforge_core::{
    Cancel, Error, ErrorKind,
    inference::{InferenceRuntime, ModelFile, OpenOptions, RuntimeIdentity, Session},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

mod cache;
mod cancel;
mod convert;
mod ffi;
mod fixed;
mod session;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tiny;

pub use cache::remove_compiled;

/// The runtime's name, as provenance and `module.status` record it.
pub const RUNTIME_NAME: &str = "onnxruntime";
/// The ONNX Runtime release the pinned archives hold (`xtask/src/onnxruntime.rs`); the version a
/// placement and provenance report is read from the linked library itself.
pub const PINNED_VERSION: &str = "1.28.0";
/// The CPU execution provider, on every platform.
pub const CPU: &str = "cpu";
/// The Core ML execution provider, on macOS.
pub const COREML: &str = "coreml";
/// The DirectML execution provider, on Windows.
pub const DIRECTML: &str = "directml";

/// One of the backends this runtime can offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    Cpu,
    CoreMl,
    DirectMl,
}

impl Backend {
    /// The backends this build offers, in its preference order: the platform's accelerator first.
    pub(crate) fn offered() -> &'static [Backend] {
        if cfg!(target_os = "macos") {
            &[Backend::CoreMl, Backend::Cpu]
        } else if cfg!(target_os = "windows") {
            &[Backend::DirectMl, Backend::Cpu]
        } else {
            &[Backend::Cpu]
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Cpu => CPU,
            Self::CoreMl => COREML,
            Self::DirectMl => DIRECTML,
        }
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::offered()
            .iter()
            .copied()
            .find(|backend| backend.name() == name)
    }

    /// The execution provider ONNX Runtime names in its graph assignment.
    pub(crate) fn provider(self) -> &'static str {
        match self {
            Self::Cpu => "CPUExecutionProvider",
            Self::CoreMl => "CoreMLExecutionProvider",
            Self::DirectMl => "DmlExecutionProvider",
        }
    }
}

/// The ONNX Runtime the host runs local models on. See the [crate documentation](crate).
#[derive(Clone, Debug, Default)]
pub struct OnnxRuntime {
    /// Where compiled Core ML models are kept, normally `<data>/inference`; `None` keeps none.
    cache_dir: Option<PathBuf>,
}

impl OnnxRuntime {
    /// A runtime keeping its Core ML compile cache under `cache_dir`, normally `<data>/inference`.
    /// Nothing is created, and ONNX Runtime is not started, until the first model opens.
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self { cache_dir }
    }

    /// The runtime as a host is handed it ([`luxforge_core::HostConfig::inference`]).
    pub fn shared(cache_dir: Option<PathBuf>) -> Arc<dyn InferenceRuntime> {
        Arc::new(Self::new(cache_dir))
    }

    /// The version of the linked ONNX Runtime, as the library reports it.
    pub fn version() -> String {
        ffi::version()
    }

    pub(crate) fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }
}

impl InferenceRuntime for OnnxRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity {
            name: RUNTIME_NAME.into(),
            version: Self::version(),
        }
    }

    fn backends(&self) -> Vec<String> {
        Backend::offered()
            .iter()
            .map(|backend| backend.name().to_owned())
            .collect()
    }

    /// On macOS, the model's fixed-shape Core ML copy and its record; nothing elsewhere.
    fn prepare(&self, model: &ModelFile, cancel: &Cancel) -> Result<Vec<PathBuf>, Error> {
        if Backend::named(COREML).is_none() {
            return Ok(Vec::new());
        }
        check_model(model)?;
        Ok(fixed::ensure(model, cancel)?.files(model.path()))
    }

    fn open_session(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Session>, Error> {
        cancel.check()?;
        let chosen = options.choose(&self.backends())?;
        let backend = Backend::named(chosen)
            .ok_or_else(|| Error::internal(format!("backend {chosen} is not offered")))?;
        check_model(model)?;
        Ok(Box::new(session::open(
            self,
            model,
            backend,
            options.precision,
            cancel,
        )?))
    }
}

/// The runtime a host serves: ONNX Runtime with its compile cache under `cache_dir`, and in a
/// developer run the deterministic fake beside it under the backend name
/// [`luxforge_core::inference::fake::FAKE_HOST_BACKEND`], through a composite that serves each
/// backend name from one of them. Nothing starts until a model opens.
pub fn host_runtime(cache_dir: Option<PathBuf>, developer: bool) -> Arc<dyn InferenceRuntime> {
    use luxforge_core::inference::{
        CompositeInference,
        fake::{FAKE_BACKEND, FAKE_HOST_BACKEND, FakeInference},
    };
    let onnx = OnnxRuntime::shared(cache_dir);
    if !developer {
        return onnx;
    }
    CompositeInference::new()
        .with(onnx)
        .and_then(|composite| {
            composite.with_alias(FAKE_HOST_BACKEND, FakeInference::shared(), FAKE_BACKEND)
        })
        .expect("ONNX Runtime offers no backend named fake")
        .shared()
}

/// The model file is there and its identity can key a cache.
fn check_model(model: &ModelFile) -> Result<(), Error> {
    let sha256 = &model.identity().sha256;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Error::validation(format!(
            "model {}'s SHA-256 {sha256:?} is not 64 lowercase hexadecimal digits",
            model.identity().id
        )));
    }
    if !model.path().is_file() {
        return Err(Error::new(
            ErrorKind::SourceUnavailable,
            format!(
                "model file of {} is missing: {}",
                model.identity().id,
                model.path().display()
            ),
        ));
    }
    Ok(())
}

/// An ONNX Runtime failure as the port reports it, `context` naming what failed. A load or run
/// whose `cancel` fired answers `cancelled` whatever ONNX Runtime said.
pub(crate) fn runtime_error(
    context: &str,
    error: impl Into<ort::Error>,
    cancel: Option<&Cancel>,
) -> Error {
    if let Some(cancel) = cancel
        && let Err(cancelled) = cancel.check()
    {
        return cancelled;
    }
    let error: ort::Error = error.into();
    use ort::error::ErrorCode;
    let kind = match error.code() {
        ErrorCode::NoSuchFile => ErrorKind::SourceUnavailable,
        ErrorCode::InvalidProtobuf
        | ErrorCode::InvalidGraph
        | ErrorCode::NoModel
        | ErrorCode::NotImplemented
        | ErrorCode::ModelLoaded => ErrorKind::Incompatible,
        ErrorCode::InvalidArgument => ErrorKind::Validation,
        _ => ErrorKind::Render,
    };
    Error::new(kind, format!("{context}: {}", error.message()))
}
