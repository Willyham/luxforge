//! The inference port: the narrow contract a local model runtime implements, which the core
//! declares and implements natively nowhere (`docs/design/ai-editing.md#the-inference-port-and-runtime`).
//!
//! # Shape
//!
//! A runtime ([`InferenceRuntime`]) is handed to the catalog owner through
//! [`crate::HostConfig::inference`], exactly as the network transport and the secret store are: a
//! native implementation comes from a crate of its own (`luxforge-onnx`), developer runs of the
//! desktop and `luxforge-json` are also given [`fake`] beside it through a [`CompositeInference`]
//! that serves each backend name from one runtime, and `None` means this host runs no local
//! inference. The runtime opens a verified model file
//! ([`ModelFile`]) on the first of a list of named backends it offers, as one of two things:
//!
//! - a [`Session`], which runs named tensors in and out and reports its placement and precision;
//! - a [`Generator`], which takes an input crop in the model's encoded domain with its mask, an
//!   optional prompt or precomputed embedding, a seed and a step count, and yields one candidate
//!   per call, reporting per-step progress, honouring cancellation at step boundaries and reporting
//!   the candidate's provenance.
//!
//! Every trait here is object safe and `Send + Sync`: a loaded model is held between tasks in the
//! host's resident set (the `local-runtime` capability, [`crate::capabilities`]) and used on the
//! module lane. Nothing here is called on the catalog owner or an interface thread.
//!
//! [`fake`] is the deterministic runtime developer runs and tests are given in place of a real
//! one; it lives here because `luxforge-testbase` may depend on no workspace crate and the core may
//! not name `luxforge-testkit`.
use crate::{Cancel, Error};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod composite;
pub mod fake;
mod generator;
mod tensor;
#[cfg(test)]
mod tests;

pub use composite::CompositeInference;
pub use generator::{
    Conditioning, GenerateRequest, Generated, Generator, MAX_GENERATION_SIDE, MAX_PROMPT_CHARS,
    MAX_STEPS, StepProgress,
};
pub use tensor::{ElementType, MAX_TENSOR_BYTES, Tensor, TensorData, TensorSpec};

/// Which model a session or generator runs: the managed resource it was opened from, its pinned
/// version and the SHA-256 the install verified.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelIdentity {
    /// The model's identity, normally the module's resource identity.
    pub id: String,
    pub version: String,
    /// 64 lowercase hexadecimal digits.
    pub sha256: String,
}

/// Which runtime ran a model, and its version: `onnxruntime 1.28.0`, or the fake's own.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub name: String,
    pub version: String,
}

/// The arithmetic a model runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Precision {
    Fp32,
    Fp16,
    Bf16,
    Int8,
    /// Parts of the graph run at different precisions.
    Mixed,
}

impl Precision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fp32 => "fp32",
            Self::Fp16 => "fp16",
            Self::Bf16 => "bf16",
            Self::Int8 => "int8",
            Self::Mixed => "mixed",
        }
    }
}

/// Where a model's work runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Device {
    Cpu,
    Gpu,
    NeuralEngine,
    /// The graph is partitioned across more than one device.
    Mixed,
}

/// Where an opened model runs and at what precision: the named backend the runtime chose (an
/// execution provider such as `cpu`, `coreml` or `directml`), the device its work lands on and
/// the precision it computes in. `partitions` is how many pieces the backend split the graph
/// into, when it says.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub backend: String,
    pub device: Device,
    pub precision: Precision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<u32>,
}

/// A model file whose bytes the host verified: an installed resource, checked against its pinned
/// length and SHA-256 when it was installed. The host builds one from the installed resource; a
/// test or a qualification run that builds one vouches for the file itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFile {
    identity: ModelIdentity,
    path: PathBuf,
}

impl ModelFile {
    pub fn new(identity: ModelIdentity, path: impl Into<PathBuf>) -> Self {
        Self {
            identity,
            path: path.into(),
        }
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// How to open a model: the backends to try, in order, and the precision to run it at. A runtime
/// opens it on the first named backend it offers ([`InferenceRuntime::backends`]) and refuses with
/// `not-ready` when it offers none of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    pub backends: Vec<String>,
    pub precision: Precision,
}

impl OpenOptions {
    /// The first of `self.backends` that `offered` lists, or `not-ready` naming both.
    pub fn choose<'a>(&'a self, offered: &[String]) -> Result<&'a str, Error> {
        self.backends
            .iter()
            .find(|backend| offered.contains(backend))
            .map(String::as_str)
            .ok_or_else(|| {
                Error::not_ready(format!(
                    "this runtime offers none of the backends {}; it offers {}",
                    self.backends.join(", "),
                    offered.join(", ")
                ))
            })
    }
}

/// The host's local model runtime: what [`crate::HostConfig::inference`] holds. It opens a
/// verified model file as a [`Session`] or a [`Generator`]. Opening may take seconds (a Core ML
/// compile on first load) and is always called on a lane, never on the catalog owner; it checks
/// `cancel` where it can.
pub trait InferenceRuntime: Send + Sync {
    /// The runtime's name and version, recorded in every candidate's provenance.
    fn identity(&self) -> RuntimeIdentity;

    /// The named backends this runtime offers on this host, in its own preference order.
    fn backends(&self) -> Vec<String>;

    /// Write what this runtime derives from a verified model file, once, inside the model's own
    /// directory, ahead of its first open (`luxforge-onnx`'s fixed-shape Core ML copy and its
    /// record): what a host calls after installing a model. It answers every derived file, written
    /// now or found already written and still matching. A runtime that derives nothing answers
    /// none, which is the default. An open never depends on it having run: the first open derives
    /// what it needs.
    fn prepare(&self, model: &ModelFile, cancel: &Cancel) -> Result<Vec<PathBuf>, Error> {
        let _ = (model, cancel);
        Ok(Vec::new())
    }

    /// [`prepare`](Self::prepare) for a model that will be opened on one of `backends`, as a
    /// `local-runtime` capability names them: what a host calls after installing it. A runtime that
    /// serves several (a [`CompositeInference`]) asks only the members serving one of them, so a
    /// model for the fake is never handed to ONNX Runtime. The default prepares when this runtime
    /// offers one of `backends`, and answers nothing otherwise.
    fn prepare_for(
        &self,
        model: &ModelFile,
        backends: &[String],
        cancel: &Cancel,
    ) -> Result<Vec<PathBuf>, Error> {
        let offered = self.backends();
        if backends.iter().any(|backend| offered.contains(backend)) {
            self.prepare(model, cancel)
        } else {
            Ok(Vec::new())
        }
    }

    /// Remove what this runtime keeps of the model whose SHA-256 is `sha256` outside the model's
    /// own directory (`luxforge-onnx`'s Core ML compile cache), as a host does when it removes the
    /// model, and answer how many entries went. A runtime that keeps nothing there answers none,
    /// which is the default. Never fails: what cannot be removed is recompiled or pruned later.
    fn forget(&self, sha256: &str) -> usize {
        let _ = sha256;
        0
    }

    /// Open `model` as a tensor session on the first backend of `options` it offers.
    fn open_session(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Session>, Error>;

    /// Open `model` as a generator on the first backend of `options` it offers. A runtime that
    /// runs no generative model refuses, which is the default.
    fn open_generator(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Generator>, Error> {
        let _ = (options, cancel);
        Err(Error::incompatible(format!(
            "runtime {} cannot open model {} as a generator",
            self.identity().name,
            model.identity().id
        )))
    }
}

impl std::fmt::Debug for dyn InferenceRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let identity = self.identity();
        write!(f, "{} {}", identity.name, identity.version)
    }
}

/// One opened model that runs named tensors in and out.
pub trait Session: Send + Sync {
    /// The model the session was opened from.
    fn model(&self) -> &ModelIdentity;

    /// Where it runs and at what precision.
    fn placement(&self) -> &Placement;

    /// The inputs the model declares, by name.
    fn inputs(&self) -> &[TensorSpec];

    /// The outputs the model declares, by name.
    fn outputs(&self) -> &[TensorSpec];

    /// Run the model once over `inputs`, each named after one of [`Self::inputs`], and answer
    /// every declared output. `cancel` is checked before the run and wherever the runtime can stop
    /// one; a cancelled run answers `cancelled` and never a partial output. Calls are serialized
    /// by the implementation when its runtime needs it.
    fn run(&self, inputs: &[Tensor], cancel: &Cancel) -> Result<Vec<Tensor>, Error>;
}

/// The SHA-256 of a prompt's UTF-8 bytes, as provenance records it: what every generator reports,
/// so the text itself is never stored.
pub fn prompt_hash(prompt: &str) -> String {
    format!("{:x}", Sha256::digest(prompt.as_bytes()))
}

/// What a generated candidate records of how it was made. The frozen patch stores it; nothing
/// regenerates from it, because the same seed on another machine, driver or runtime version is not
/// promised to reproduce the pixels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub model: ModelIdentity,
    pub runtime: RuntimeIdentity,
    pub backend: String,
    pub precision: Precision,
    pub seed: u64,
    pub steps: u32,
    pub sampler: String,
    /// [`prompt_hash`] of the prompt, or of an embedding's name for a precomputed one; `None`
    /// for a prompt-free fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
}

impl Provenance {
    /// The flat string and number fields a candidate's provenance holds (a candidate publication
    /// takes only those): `model`, `model_version`, `model_sha256`, `runtime`, `runtime_version`,
    /// `backend`, `precision`, `seed`, `steps`, `sampler` and, when there is one, `prompt_hash`.
    /// About 400 bytes, well inside a candidate's 2 KiB of provenance.
    pub fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        fields.insert("model".into(), json!(self.model.id));
        fields.insert("model_version".into(), json!(self.model.version));
        fields.insert("model_sha256".into(), json!(self.model.sha256));
        fields.insert("runtime".into(), json!(self.runtime.name));
        fields.insert("runtime_version".into(), json!(self.runtime.version));
        fields.insert("backend".into(), json!(self.backend));
        fields.insert("precision".into(), json!(self.precision.as_str()));
        fields.insert("seed".into(), json!(self.seed));
        fields.insert("steps".into(), json!(self.steps));
        fields.insert("sampler".into(), json!(self.sampler));
        if let Some(hash) = &self.prompt_hash {
            fields.insert("prompt_hash".into(), json!(hash));
        }
        fields
    }
}
