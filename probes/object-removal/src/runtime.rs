//! The inference port: one narrow session trait over interchangeable runtimes, so a model runs
//! unchanged on each backend the probe compares.

use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::tensor::{Data, Tensor};

/// A runtime and the device placement it is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// ONNX Runtime's CPU execution provider.
    OrtCpu,
    /// ONNX Runtime with the Core ML execution provider (MLProgram), falling back to its CPU
    /// provider for nodes Core ML does not take.
    OrtCoreMl(CoreMlUnits),
    /// `OrtCoreMl` over a copy of the model whose free input dimensions are all fixed at 1. The
    /// provider takes only nodes with static input shapes, so it runs none of a model that declares
    /// a free dimension such as `batch_size` or `num_points`. ONNX Runtime writes the copy itself
    /// (see `ort_backend::fixed_copy`). An input of any other shape runs on a CPU session of the
    /// original model, opened the first time one arrives.
    OrtCoreMlFixed(CoreMlUnits),
    /// tract's CPU runtime (`tract-cpu`), optimized and run through a `TypedModel` plan.
    TractCpu,
    /// tract's Metal runtime (`tract-metal`), via `tract_metal::MetalTransform`.
    TractMetal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreMlUnits {
    All,
    CpuAndGpu,
    CpuAndNeuralEngine,
    CpuOnly,
}

impl Backend {
    pub const ALL: &[Backend] = &[
        Backend::OrtCpu,
        Backend::OrtCoreMl(CoreMlUnits::All),
        Backend::OrtCoreMl(CoreMlUnits::CpuAndGpu),
        Backend::OrtCoreMl(CoreMlUnits::CpuAndNeuralEngine),
        Backend::TractCpu,
        Backend::TractMetal,
    ];

    /// The fixed-shape Core ML backends. They parse, but are not in `ALL`.
    pub const FIXED: &[Backend] = &[
        Backend::OrtCoreMlFixed(CoreMlUnits::All),
        Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndGpu),
        Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndNeuralEngine),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Backend::OrtCpu => "ort-cpu",
            Backend::OrtCoreMl(CoreMlUnits::All) => "ort-coreml-all",
            Backend::OrtCoreMl(CoreMlUnits::CpuAndGpu) => "ort-coreml-gpu",
            Backend::OrtCoreMl(CoreMlUnits::CpuAndNeuralEngine) => "ort-coreml-ane",
            Backend::OrtCoreMl(CoreMlUnits::CpuOnly) => "ort-coreml-cpu",
            Backend::OrtCoreMlFixed(CoreMlUnits::All) => "ort-coreml-all-fixed",
            Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndGpu) => "ort-coreml-gpu-fixed",
            Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndNeuralEngine) => "ort-coreml-ane-fixed",
            Backend::OrtCoreMlFixed(CoreMlUnits::CpuOnly) => "ort-coreml-cpu-fixed",
            Backend::TractCpu => "tract-cpu",
            Backend::TractMetal => "tract-metal",
        }
    }
}

impl FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Backend::ALL
            .iter()
            .chain(Backend::FIXED)
            .chain(&[
                Backend::OrtCoreMl(CoreMlUnits::CpuOnly),
                Backend::OrtCoreMlFixed(CoreMlUnits::CpuOnly),
            ])
            .copied()
            .find(|b| b.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Backend::ALL
                    .iter()
                    .chain(Backend::FIXED)
                    .map(|b| b.name())
                    .collect();
                format!(
                    "unknown backend {s:?}; expected one of {}",
                    names.join(", ")
                )
            })
    }
}

/// One declared input or output of a loaded model.
#[derive(Clone, Debug)]
pub struct Io {
    pub name: String,
    /// Element type and shape as the runtime reports them; a dynamic dimension is `-1`.
    pub description: String,
}

/// A loaded model. `run` takes named inputs and returns every output by name, in the model's
/// declared order.
pub trait Session: Send {
    fn backend(&self) -> Backend;
    fn inputs(&self) -> &[Io];
    fn outputs(&self) -> &[Io];
    fn run(&mut self, inputs: &[(&str, &Tensor)]) -> Result<Vec<(String, Tensor)>, String>;
}

/// Loads a model on a backend and reports how long the load took (for Core ML this includes
/// compiling the model, or reading the compiled cache).
pub fn open(backend: Backend, model: &Path) -> Result<(Box<dyn Session>, Duration), String> {
    let start = Instant::now();
    let session: Box<dyn Session> = match backend {
        Backend::OrtCpu | Backend::OrtCoreMl(_) | Backend::OrtCoreMlFixed(_) => {
            Box::new(ort_backend::OrtSession::open(backend, model)?)
        }
        Backend::TractCpu | Backend::TractMetal => {
            Box::new(crate::tract_backend::TractSession::open(backend, model)?)
        }
    };
    Ok((session, start.elapsed()))
}

/// Looks an output up by name.
pub fn take(outputs: &mut Vec<(String, Tensor)>, name: &str) -> Result<Tensor, String> {
    let i = outputs
        .iter()
        .position(|(n, _)| n == name)
        .ok_or_else(|| format!("model has no output {name:?}"))?;
    Ok(outputs.remove(i).1)
}

mod ort_backend {
    use std::path::{Path, PathBuf};

    use ort::ep::coreml::{ComputeUnits, ModelFormat};
    use ort::logging::LogLevel;
    use ort::session::Session as OrtInner;
    use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
    use ort::value::{DynValue, TensorElementType, ValueType};

    use super::{Backend, CoreMlUnits, Io, Session};
    use crate::tensor::{Data, Tensor};

    pub struct OrtSession {
        backend: Backend,
        inner: OrtInner,
        inputs: Vec<Io>,
        outputs: Vec<Io>,
        /// Set for a fixed-shape backend.
        fixed: Option<Fixed>,
    }

    /// A fixed-shape session's inputs and its way out for other shapes.
    struct Fixed {
        /// Each input's name and the shape the copy fixes it at.
        shapes: Vec<(String, Vec<usize>)>,
        /// The original model, for a CPU session that runs any other shape.
        original: PathBuf,
        flexible: Option<OrtInner>,
    }

    fn err(e: impl std::fmt::Display) -> String {
        format!("onnx runtime: {e}")
    }

    /// Core ML's compiled models, and the fixed-shape copies, live here. Remove it for a cold load.
    pub fn cache_dir() -> PathBuf {
        std::env::temp_dir().join("luxforge-removal-probe-coreml")
    }

    /// ONNX Runtime's session log level from `LUXFORGE_REMOVAL_ORT_LOG` (`verbose`, `info`,
    /// `warning` or `error`); at `warning` the Core ML provider reports how many nodes it takes,
    /// and at `verbose` why it refuses each one it does not.
    fn log_level() -> Option<LogLevel> {
        match std::env::var("LUXFORGE_REMOVAL_ORT_LOG").ok()?.as_str() {
            "verbose" => Some(LogLevel::Verbose),
            "info" => Some(LogLevel::Info),
            "warning" => Some(LogLevel::Warning),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }

    fn builder() -> Result<SessionBuilder, String> {
        let mut b = OrtInner::builder()
            .map_err(err)?
            // Basic, as darktable found for the SAM 2 decoder: extended fusions break its shape inference.
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(err)?;
        if let Some(level) = log_level() {
            b = b.with_log_level(level).map_err(err)?;
        }
        Ok(b)
    }

    fn describe(o: &ort::value::Outlet) -> Io {
        Io {
            name: o.name().to_string(),
            description: o.dtype().to_string(),
        }
    }

    /// A copy of `model` with every named free input dimension fixed at 1, written by ONNX Runtime
    /// itself: a CPU session with free-dimension overrides at the basic optimization level, which
    /// is provider-independent, saves the graph it optimized. Its shape computations are then
    /// constants, and every input and output shape is static.
    ///
    /// Setting the same overrides on the Core ML session instead lets the provider take the whole
    /// graph, but the compiled partition then asks at run time for a `Shape` node's output that
    /// nothing feeds (`Feature …_Shape_output_0 is required but not specified`), for both
    /// EfficientViT-SAM models. The copy is written once per model file, into `cache_dir`, and
    /// reused.
    fn fixed_copy(model: &Path) -> Result<PathBuf, String> {
        let meta = std::fs::metadata(model).map_err(|e| format!("{}: {e}", model.display()))?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let stem = model
            .file_stem()
            .map_or("model".into(), |s| s.to_string_lossy());
        let dir = cache_dir().join("fixed");
        let copy = dir.join(format!("{stem}-{}-{modified}-fixed.onnx", meta.len()));
        if copy.is_file() {
            return Ok(copy);
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let probe = builder()?.commit_from_file(model).map_err(err)?;
        let mut symbols: Vec<String> = Vec::new();
        for input in probe.inputs() {
            let ValueType::Tensor {
                shape,
                dimension_symbols,
                ..
            } = input.dtype()
            else {
                return Err(format!("input {:?} is not a tensor", input.name()));
            };
            for (&d, symbol) in shape.iter().zip(dimension_symbols.iter()) {
                if d >= 0 {
                    continue;
                }
                if symbol.is_empty() {
                    return Err(format!(
                        "input {:?} has an unnamed free dimension, which cannot be fixed by name",
                        input.name()
                    ));
                }
                if !symbols.contains(symbol) {
                    symbols.push(symbol.clone());
                }
            }
        }
        drop(probe);
        let partial = dir.join(format!(
            "{}.{}.partial",
            copy.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let mut b = builder()?;
        for s in &symbols {
            b = b.with_dimension_override(s, 1).map_err(err)?;
        }
        let saved = b
            .with_optimized_model_path(&partial)
            .map_err(err)?
            .commit_from_file(model)
            .map_err(err)?;
        drop(saved);
        std::fs::rename(&partial, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
        Ok(copy)
    }

    impl OrtSession {
        pub fn open(backend: Backend, model: &Path) -> Result<Self, String> {
            let (units, path, fixed) = match backend {
                Backend::OrtCpu => (None, model.to_path_buf(), false),
                Backend::OrtCoreMl(u) => (Some(u), model.to_path_buf(), false),
                Backend::OrtCoreMlFixed(u) => (Some(u), fixed_copy(model)?, true),
                Backend::TractCpu | Backend::TractMetal => {
                    return Err(format!("{} is not an ONNX Runtime backend", backend.name()));
                }
            };
            let mut builder = builder()?;
            if let Some(units) = units {
                let units = match units {
                    CoreMlUnits::All => ComputeUnits::All,
                    CoreMlUnits::CpuAndGpu => ComputeUnits::CPUAndGPU,
                    CoreMlUnits::CpuAndNeuralEngine => ComputeUnits::CPUAndNeuralEngine,
                    CoreMlUnits::CpuOnly => ComputeUnits::CPUOnly,
                };
                let ep = ort::ep::CoreML::default()
                    .with_model_format(ModelFormat::MLProgram)
                    .with_compute_units(units)
                    .with_static_input_shapes(true)
                    .with_model_cache_dir(cache_dir().display())
                    .build()
                    .error_on_failure();
                builder = builder.with_execution_providers([ep]).map_err(err)?;
            }
            let inner = builder.commit_from_file(&path).map_err(err)?;
            let fixed = if fixed {
                let shapes = inner
                    .inputs()
                    .iter()
                    .map(|o| {
                        let shape = o
                            .dtype()
                            .tensor_shape()
                            .filter(|s| s.iter().all(|&d| d >= 0))
                            .ok_or_else(|| {
                                format!("the fixed copy's input {:?} is not static", o.name())
                            })?;
                        Ok((
                            o.name().to_string(),
                            shape.iter().map(|&d| d as usize).collect(),
                        ))
                    })
                    .collect::<Result<_, String>>()?;
                Some(Fixed {
                    shapes,
                    original: model.to_path_buf(),
                    flexible: None,
                })
            } else {
                None
            };
            let inputs = inner.inputs().iter().map(describe).collect();
            let outputs = inner.outputs().iter().map(describe).collect();
            Ok(Self {
                backend,
                inner,
                inputs,
                outputs,
                fixed,
            })
        }
    }

    fn to_value(t: &Tensor) -> Result<DynValue, String> {
        let shape: Vec<i64> = t.shape.iter().map(|&d| d as i64).collect();
        Ok(match &t.data {
            Data::F32(v) => ort::value::Tensor::from_array((shape, v.clone()))
                .map_err(err)?
                .into_dyn(),
            Data::I64(v) => ort::value::Tensor::from_array((shape, v.clone()))
                .map_err(err)?
                .into_dyn(),
            Data::I32(v) => ort::value::Tensor::from_array((shape, v.clone()))
                .map_err(err)?
                .into_dyn(),
        })
    }

    fn from_value(name: &str, v: &DynValue) -> Result<Tensor, String> {
        let ValueType::Tensor { ty, .. } = v.dtype() else {
            return Err(format!("output {name:?} is not a tensor"));
        };
        let dims = |s: &ort::value::Shape| s.iter().map(|&d| d as usize).collect::<Vec<_>>();
        Ok(match ty {
            TensorElementType::Float32 => {
                let (s, d) = v.try_extract_tensor::<f32>().map_err(err)?;
                Tensor::f32(dims(s), d.to_vec())
            }
            TensorElementType::Int64 => {
                let (s, d) = v.try_extract_tensor::<i64>().map_err(err)?;
                Tensor::i64(dims(s), d.to_vec())
            }
            TensorElementType::Int32 => {
                let (s, d) = v.try_extract_tensor::<i32>().map_err(err)?;
                Tensor::i32(dims(s), d.to_vec())
            }
            other => {
                return Err(format!(
                    "output {name:?} has unsupported element type {other}"
                ));
            }
        })
    }

    impl Session for OrtSession {
        fn backend(&self) -> Backend {
            self.backend
        }

        fn inputs(&self) -> &[Io] {
            &self.inputs
        }

        fn outputs(&self) -> &[Io] {
            &self.outputs
        }

        fn run(&mut self, inputs: &[(&str, &Tensor)]) -> Result<Vec<(String, Tensor)>, String> {
            let values = inputs
                .iter()
                .map(|(n, t)| Ok((n.to_string(), to_value(t)?)))
                .collect::<Result<Vec<_>, String>>()?;
            let session = match &mut self.fixed {
                Some(fixed)
                    if !inputs.iter().all(|(n, t)| {
                        fixed
                            .shapes
                            .iter()
                            .any(|(name, shape)| name == n && *shape == t.shape)
                    }) =>
                {
                    if fixed.flexible.is_none() {
                        let cpu = builder()?.commit_from_file(&fixed.original).map_err(err)?;
                        fixed.flexible = Some(cpu);
                    }
                    fixed.flexible.as_mut().expect("opened above")
                }
                _ => &mut self.inner,
            };
            let outputs = session.run(values).map_err(err)?;
            outputs
                .iter()
                .map(|(n, v)| Ok((n.to_string(), from_value(n, &v)?)))
                .collect()
        }
    }
}

/// Mean and maximum absolute difference between two f32 tensors of one shape.
pub fn difference(a: &Tensor, b: &Tensor) -> Result<(f64, f32), String> {
    if a.shape != b.shape {
        return Err(format!(
            "shapes differ: {:?} against {:?}",
            a.shape, b.shape
        ));
    }
    let (Data::F32(x), Data::F32(y)) = (&a.data, &b.data) else {
        return Err("difference compares f32 tensors only".into());
    };
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for (p, q) in x.iter().zip(y) {
        let d = (p - q).abs();
        sum += d as f64;
        max = max.max(d);
    }
    Ok((sum / x.len().max(1) as f64, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_backend_name_round_trips_through_from_str() {
        for &b in Backend::ALL {
            assert_eq!(Backend::from_str(b.name()), Ok(b));
        }
    }

    #[test]
    fn all_lists_every_backend_family_once() {
        let names: Vec<&str> = Backend::ALL.iter().map(|b| b.name()).collect();
        assert_eq!(
            names,
            [
                "ort-cpu",
                "ort-coreml-all",
                "ort-coreml-gpu",
                "ort-coreml-ane",
                "tract-cpu",
                "tract-metal",
            ]
        );
    }

    #[test]
    fn coreml_cpu_only_parses_though_it_is_not_in_all() {
        assert_eq!(
            Backend::from_str("ort-coreml-cpu"),
            Ok(Backend::OrtCoreMl(CoreMlUnits::CpuOnly))
        );
        assert!(!Backend::ALL.contains(&Backend::OrtCoreMl(CoreMlUnits::CpuOnly)));
    }

    #[test]
    fn fixed_backends_parse_but_are_not_in_all() {
        for &b in Backend::FIXED
            .iter()
            .chain(&[Backend::OrtCoreMlFixed(CoreMlUnits::CpuOnly)])
        {
            assert_eq!(Backend::from_str(b.name()), Ok(b));
            assert!(!Backend::ALL.contains(&b));
        }
        assert_eq!(
            Backend::from_str("ort-coreml-gpu-fixed"),
            Ok(Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndGpu))
        );
    }

    /// The EfficientViT-SAM decoder declares `batch_size` and `num_points`. On a fixed backend a
    /// one-point prompt runs on Core ML, close to but not bit-identical with ort-cpu; a batch of
    /// two runs on the CPU session and is identical.
    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn a_fixed_session_runs_its_own_shape_on_core_ml_and_others_on_the_cpu() {
        let dir = crate::models::directory(None);
        let decoder = crate::models::EVIT_L0_DECODER.verified(&dir).unwrap();
        let (mut cpu, _) = open(Backend::OrtCpu, &decoder).unwrap();
        let (mut fixed, _) =
            open(Backend::OrtCoreMlFixed(CoreMlUnits::CpuAndGpu), &decoder).unwrap();
        assert!(
            fixed
                .inputs()
                .iter()
                .all(|io| !io.description.contains("batch_size")),
            "{:?}",
            fixed.inputs()
        );
        let embedding = Tensor::f32(
            [1, 256, 64, 64],
            (0..256 * 64 * 64)
                .map(|i| ((i % 97) as f32 / 97.0 - 0.5) * 0.2)
                .collect(),
        );
        let run = |s: &mut Box<dyn Session>, b: usize| {
            let coords = Tensor::f32([b, 1, 2], [300.0, 400.0, 700.0, 200.0][..2 * b].to_vec());
            let labels = Tensor::f32([b, 1], vec![1.0; b]);
            let mut out = s
                .run(&[
                    ("image_embeddings", &embedding),
                    ("point_coords", &coords),
                    ("point_labels", &labels),
                ])
                .unwrap();
            take(&mut out, "masks").unwrap()
        };
        let (mean, max) = difference(&run(&mut fixed, 1), &run(&mut cpu, 1)).unwrap();
        assert!(max > 0.0 && max < 1e-3, "mean {mean}, max {max}");
        assert_eq!(run(&mut fixed, 2), run(&mut cpu, 2));
    }

    #[test]
    fn unknown_backend_name_is_rejected_with_the_valid_list() {
        let err = Backend::from_str("tract-cuda").unwrap_err();
        assert!(err.contains("tract-cuda"), "{err}");
        assert!(err.contains("tract-metal"), "{err}");
    }

    #[test]
    fn difference_reports_mean_and_max_absolute_error() {
        let a = Tensor::f32([2, 2], vec![0.0, 1.0, 2.0, 3.0]);
        let b = Tensor::f32([2, 2], vec![0.0, 1.5, 2.0, 5.0]);
        let (mean, max) = difference(&a, &b).unwrap();
        assert!((mean - 0.625).abs() < 1e-6, "mean was {mean}");
        assert!((max - 2.0).abs() < 1e-6, "max was {max}");
    }

    #[test]
    fn difference_rejects_mismatched_shapes() {
        let a = Tensor::f32([2], vec![0.0, 1.0]);
        let b = Tensor::f32([1, 2], vec![0.0, 1.0]);
        assert!(difference(&a, &b).is_err());
    }

    #[test]
    fn difference_rejects_non_float_tensors() {
        let a = Tensor::i64([2], vec![0, 1]);
        let b = Tensor::i64([2], vec![0, 1]);
        assert!(difference(&a, &b).is_err());
    }
}
