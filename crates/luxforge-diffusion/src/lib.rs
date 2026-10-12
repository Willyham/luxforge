//! The generative tier's local runtime: stable-diffusion.cpp and ggml, built from the vendored,
//! pinned sources in `vendor/` with the `cc` crate (`native/build.rs`, THIRD_PARTY.md), behind the
//! core's [`InferenceRuntime`] and [`Generator`] (`docs/design/ai-editing.md`, "The inference port
//! and runtime").
//!
//! [`DiffusionRuntime`] names itself `stable-diffusion.cpp` with its pinned version and offers the
//! backend [`METAL_BACKEND`] on macOS and [`CPU_BACKEND`] on Linux; Windows compiles no runtime and
//! offers none. It opens the qualified model, FLUX.2 klein 4B, as a [`KleinGenerator`] from an
//! installed resource whose files are laid out as [`layout`] names them, and opens no tensor
//! session. The desktop and `luxforge-json` hand it to the catalog owner beside ONNX Runtime
//! through a `CompositeInference` (`luxforge_onnx::host_runtime_with`), so the core links no
//! generative runtime.
//!
//! # Bounds
//!
//! One candidate per call, at most [`MAX_SIDE`] a side, the size its memory was measured at: a
//! larger request is refused `resource-limit` before anything is allocated. Generations run one at
//! a time in a process (the runtime's progress callback is process-wide, and the GPU is one), and
//! a context's compute buffers are reserved when it first generates, so a held generator's memory
//! does not grow with its candidates. Cancellation is honoured at the runtime's step boundaries and
//! between its encode and decode stages: a running step finishes first (about 30 s at 1024 px on
//! the M4 Pro).
use luxforge_core::{
    Cancel, Error,
    inference::{
        Device, Generator, InferenceRuntime, ModelFile, OpenOptions, Placement, Precision,
        RuntimeIdentity, Session,
    },
};
use std::{path::PathBuf, sync::Arc};

mod generator;
pub mod layout;
#[cfg(luxforge_sdcpp)]
#[allow(dead_code)]
mod native;
#[cfg(test)]
mod tests;

pub use generator::{
    KleinGenerator, LORA_PROMPT, MAX_SIDE, REPLACE_PREFIX, outlined, replace_prompt,
};

/// The runtime's name in provenance.
pub const RUNTIME_NAME: &str = "stable-diffusion.cpp";
/// The pinned upstream version and its ggml, as `native/build.rs` compiles them in.
pub const RUNTIME_VERSION: &str = "master-956-1b0ba10 (ggml 0.25.3 d25b121c)";
/// The backend name this runtime serves klein under on macOS: ggml's Metal backend.
pub const METAL_BACKEND: &str = "metal";
/// The backend name on Linux: ggml's CPU backend. Not `cpu`, which ONNX Runtime's names.
pub const CPU_BACKEND: &str = "diffusion-cpu";

/// The backends this build offers: Metal on macOS, the CPU on Linux, none where no runtime is
/// compiled.
pub fn offered_backends() -> Vec<String> {
    if cfg!(not(luxforge_sdcpp)) {
        Vec::new()
    } else if cfg!(target_os = "macos") {
        vec![METAL_BACKEND.into()]
    } else {
        vec![CPU_BACKEND.into()]
    }
}

/// stable-diffusion.cpp behind the inference port.
#[derive(Debug, Default)]
pub struct DiffusionRuntime;

impl DiffusionRuntime {
    pub fn new() -> Self {
        Self
    }

    /// The runtime as a host holds it.
    pub fn shared() -> Arc<dyn InferenceRuntime> {
        Arc::new(Self)
    }
}

impl InferenceRuntime for DiffusionRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity {
            name: RUNTIME_NAME.into(),
            version: RUNTIME_VERSION.into(),
        }
    }

    fn backends(&self) -> Vec<String> {
        offered_backends()
    }

    /// Compile the Metal kernels once on this machine, so the model's first generation does not
    /// (`docs/design/ai-editing.md` records what it costs). Writes nothing into the model's
    /// directory: the system keeps the compiled kernels in its own cache.
    fn prepare(&self, _model: &ModelFile, cancel: &Cancel) -> Result<Vec<PathBuf>, Error> {
        cancel.check()?;
        #[cfg(luxforge_sdcpp)]
        native::warm().map_err(|detail| {
            Error::render(format!(
                "the {METAL_BACKEND} backend did not open: {detail}"
            ))
        })?;
        Ok(Vec::new())
    }

    fn open_session(
        &self,
        model: &ModelFile,
        _options: &OpenOptions,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Session>, Error> {
        Err(Error::incompatible(format!(
            "{RUNTIME_NAME} opens model {} only as a generator",
            model.identity().id
        )))
    }

    fn open_generator(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Generator>, Error> {
        let backend = options.choose(&self.backends())?.to_owned();
        cancel.check()?;
        let placement = Placement {
            device: if backend == METAL_BACKEND {
                Device::Gpu
            } else {
                Device::Cpu
            },
            backend,
            // A 4-bit transformer with bf16 layers and a bf16 autoencoder.
            precision: Precision::Mixed,
            partitions: None,
        };
        Ok(Box::new(KleinGenerator::open(
            model,
            placement,
            self.identity(),
            cancel,
        )?))
    }
}
