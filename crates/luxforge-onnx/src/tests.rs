use super::*;
use crate::{
    ffi::Assigned,
    session::placement,
    tiny::{Side, affine},
};
use luxforge_core::inference::{
    Device, ElementType, ModelIdentity, Precision, Session, Tensor, TensorData,
};
use std::fs;

/// A model directory under a fresh temporary directory, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        Self(luxforge_testbase::paths::temp_dir(&format!("onnx-{name}")))
    }

    /// `bytes` written as `model.onnx`, identified by their own SHA-256.
    fn model(&self, bytes: &[u8]) -> ModelFile {
        let path = self.0.join("model.onnx");
        fs::write(&path, bytes).unwrap();
        ModelFile::new(
            ModelIdentity {
                id: "affine".into(),
                version: "1".into(),
                sha256: fixed::sha256(&path).unwrap(),
            },
            path,
        )
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SCALE: [f32; 3] = [2.0, -1.0, 0.5];
const BIAS: [f32; 3] = [0.25, 1.0, -3.0];

fn options(backends: &[&str]) -> OpenOptions {
    OpenOptions {
        backends: backends.iter().map(|backend| (*backend).into()).collect(),
        precision: Precision::Fp32,
    }
}

fn open(
    runtime: &OnnxRuntime,
    model: &ModelFile,
    backends: &[&str],
) -> Result<Box<dyn Session>, Error> {
    runtime.open_session(model, &options(backends), &Cancel::never())
}

fn refusal<T>(result: Result<T, Error>) -> Error {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => error,
    }
}

/// `x` of `batch` rows of three.
fn x(batch: u64) -> Tensor {
    let values = (0..batch * 3).map(|index| index as f32 - 1.5).collect();
    Tensor::new("x", vec![batch, 3], TensorData::F32(values)).unwrap()
}

/// What the affine model answers for `x`.
fn expected(x: &Tensor) -> Vec<f32> {
    x.f32()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, value)| value * SCALE[index % 3] + BIAS[index % 3])
        .collect()
}

fn run(session: &dyn Session, input: Tensor) -> Vec<f32> {
    let outputs = session.run(&[input], &Cancel::never()).unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].name(), "y");
    outputs[0].f32().unwrap().to_vec()
}

#[test]
fn the_runtime_offers_its_platforms_backends_and_names_the_linked_version() {
    let runtime = OnnxRuntime::new(None);
    let expected: &[&str] = if cfg!(target_os = "macos") {
        &["coreml", "cpu"]
    } else if cfg!(target_os = "windows") {
        &["directml", "cpu"]
    } else {
        &["cpu"]
    };
    assert_eq!(runtime.backends(), expected);
    let identity = runtime.identity();
    assert_eq!(identity.name, "onnxruntime");
    // The library linked is the release the pinned archives hold.
    assert_eq!(identity.version, PINNED_VERSION);
}

#[test]
fn a_developer_host_serves_the_fake_beside_onnx_runtime_under_its_own_name() {
    let native = OnnxRuntime::new(None).backends();
    assert_eq!(host_runtime(None, false).backends(), native);
    assert_eq!(host_runtime(None, false).identity().name, "onnxruntime");
    let developer = host_runtime(None, true);
    let mut expected = native.clone();
    expected.push("fake".into());
    assert_eq!(developer.backends(), expected);
    assert_eq!(developer.identity().name, "onnxruntime + luxforge-fake");
    // `cpu` opens the model on ONNX Runtime and `fake` on the fake, which opens any file.
    let dir = Dir::new("host");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let onnx = developer
        .open_session(&model, &options(&["cpu"]), &Cancel::never())
        .unwrap();
    assert_eq!(onnx.inputs()[0].name, "x");
    let fake = developer
        .open_session(&model, &options(&["fake"]), &Cancel::never())
        .unwrap();
    assert_eq!(fake.inputs()[0].name, "input");
}

#[test]
fn a_cpu_session_runs_named_tensors_and_reports_its_placement() {
    let dir = Dir::new("cpu");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let runtime = OnnxRuntime::new(None);
    // The first backend offered of those asked for.
    let session = open(&runtime, &model, &["tpu", "cpu"]).unwrap();
    assert_eq!(session.model(), model.identity());
    let placement = session.placement();
    assert_eq!(placement.backend, "cpu");
    assert_eq!(placement.device, Device::Cpu);
    assert_eq!(placement.precision, Precision::Fp32);
    // The CPU provider runs the whole graph: it is not partitioned.
    assert_eq!(placement.partitions, None);
    // ONNX Runtime recorded where each node went: both on the CPU provider, one part each.
    let session = session::builder()
        .unwrap()
        .commit_from_file(model.path())
        .unwrap();
    let cpu = |nodes| Assigned {
        provider: "CPUExecutionProvider".into(),
        nodes,
    };
    assert_eq!(ffi::assignment(&session).unwrap(), [cpu(1), cpu(1)]);
    let session = open(&runtime, &model, &["cpu"]).unwrap();
    assert_eq!(session.inputs().len(), 1);
    assert_eq!(session.inputs()[0].name, "x");
    assert_eq!(session.inputs()[0].element, ElementType::F32);
    assert_eq!(session.inputs()[0].shape, [None, Some(3)]);
    assert_eq!(session.outputs()[0].name, "y");
    for batch in [1, 2, 5] {
        let input = x(batch);
        let want = expected(&input);
        assert_eq!(run(session.as_ref(), input), want, "batch {batch}");
    }
    // Nothing was derived beside a model the CPU opened.
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn a_backend_this_runtime_does_not_offer_is_not_ready() {
    let dir = Dir::new("backend");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let error = refusal(open(&OnnxRuntime::new(None), &model, &["tpu", "npu"]));
    assert_eq!(error.kind, ErrorKind::NotReady);
    assert!(error.detail.contains("tpu, npu"), "{}", error.detail);
}

#[test]
fn a_missing_unreadable_or_misidentified_model_is_refused_by_kind() {
    let dir = Dir::new("errors");
    let runtime = OnnxRuntime::new(None);
    let model = dir.model(b"this is not an ONNX model");
    let error = refusal(open(&runtime, &model, &["cpu"]));
    assert_eq!(error.kind, ErrorKind::Incompatible, "{}", error.detail);
    assert!(
        error
            .detail
            .starts_with("model affine could not be opened on cpu: "),
        "{}",
        error.detail
    );
    let missing = ModelFile::new(model.identity().clone(), dir.0.join("absent.onnx"));
    let error = refusal(open(&runtime, &missing, &["cpu"]));
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    let unkeyed = ModelFile::new(
        ModelIdentity {
            sha256: "../escape".into(),
            ..model.identity().clone()
        },
        model.path(),
    );
    let error = refusal(open(&runtime, &unkeyed, &["cpu"]));
    assert_eq!(error.kind, ErrorKind::Validation);
}

#[test]
fn a_run_refuses_inputs_the_model_does_not_take_before_running() {
    let dir = Dir::new("inputs");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let session = open(&OnnxRuntime::new(None), &model, &["cpu"]).unwrap();
    let cancel = Cancel::never();
    let wrong_name = Tensor::new("z", vec![1, 3], TensorData::F32(vec![0.0; 3])).unwrap();
    let wrong_side = Tensor::new("x", vec![1, 4], TensorData::F32(vec![0.0; 4])).unwrap();
    let wrong_type = Tensor::new("x", vec![1, 3], TensorData::I64(vec![0; 3])).unwrap();
    for (inputs, says) in [
        (vec![wrong_name], "has no input z; it takes x"),
        (
            vec![wrong_side],
            "takes F32 of shape [?, 3], not F32 of shape [1, 4]",
        ),
        (vec![wrong_type], "not I64"),
        (vec![x(1), x(1)], "input x is given twice"),
        (vec![], "needs input x"),
    ] {
        let error = refusal(session.run(&inputs, &cancel));
        assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
        assert!(error.detail.contains(says), "{}", error.detail);
    }
}

#[test]
fn a_cancelled_open_or_run_answers_cancelled() {
    let dir = Dir::new("cancel");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let runtime = OnnxRuntime::new(None);
    let cancel = Cancel::new();
    let session = runtime
        .open_session(&model, &options(&["cpu"]), &cancel)
        .unwrap();
    cancel.cancel();
    let error = refusal(session.run(&[x(1)], &cancel));
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let error = refusal(runtime.open_session(&model, &options(&["cpu"]), &cancel));
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn a_watched_load_is_cancelled_through_the_runtimes_flag() {
    let dir = Dir::new("watch");
    let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
    let commit = |cancel: &Cancel| {
        use std::sync::atomic::{AtomicBool, Ordering};
        let builder = session::builder().unwrap();
        let canceler = builder.canceler();
        let mut builder = builder;
        let flagged = AtomicBool::new(false);
        cancel::watching_with(
            cancel,
            || {
                canceler.cancel().unwrap();
                flagged.store(true, Ordering::SeqCst);
            },
            || {
                // The load starts once the watcher has set ONNX Runtime's flag, if it will.
                if cancel.check().is_err() {
                    luxforge_testbase::wait_until("the watcher cancels the load", || {
                        flagged.load(Ordering::SeqCst)
                    });
                }
                builder.commit_from_file(model.path())
            },
        )
    };
    assert!(commit(&Cancel::never()).is_ok());
    let cancel = Cancel::new();
    cancel.cancel();
    let error = match commit(&cancel) {
        Ok(_) => panic!("a cancelled load opened"),
        Err(error) => runtime_error("model affine could not be opened", error, Some(&cancel)),
    };
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn runtime_failures_map_to_the_ports_error_kinds() {
    use ort::error::ErrorCode;
    for (code, kind) in [
        (ErrorCode::NoSuchFile, ErrorKind::SourceUnavailable),
        (ErrorCode::InvalidProtobuf, ErrorKind::Incompatible),
        (ErrorCode::InvalidGraph, ErrorKind::Incompatible),
        (ErrorCode::NotImplemented, ErrorKind::Incompatible),
        (ErrorCode::NoModel, ErrorKind::Incompatible),
        (ErrorCode::InvalidArgument, ErrorKind::Validation),
        (ErrorCode::GenericFailure, ErrorKind::Render),
        (ErrorCode::RuntimeException, ErrorKind::Render),
        (ErrorCode::ExecutionProviderFailure, ErrorKind::Render),
    ] {
        let error = runtime_error(
            "model m failed to run",
            ort::Error::new_with_code(code, "why"),
            None,
        );
        assert_eq!(error.kind, kind, "{code:?}");
        assert_eq!(error.detail, "model m failed to run: why");
    }
    // A failure after the job was cancelled is the cancellation.
    let cancel = Cancel::new();
    cancel.cancel();
    let error = runtime_error(
        "model m could not be opened on coreml",
        ort::Error::new_with_code(ErrorCode::GenericFailure, "load cancelled"),
        Some(&cancel),
    );
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn placement_reads_where_the_graph_was_assigned() {
    let part = |provider: &str, nodes| Assigned {
        provider: provider.into(),
        nodes,
    };
    let coreml = "CoreMLExecutionProvider";
    let cpu = "CPUExecutionProvider";
    let cases = [
        // Every node on Core ML, in one part: the GPU.
        (vec![part(coreml, 437)], Device::Gpu, Some(1)),
        // Split with the CPU: mixed, counting Core ML's parts.
        (
            vec![part(coreml, 270), part(cpu, 11), part(coreml, 4)],
            Device::Mixed,
            Some(2),
        ),
        // Nothing on Core ML: the CPU.
        (vec![part(cpu, 422)], Device::Cpu, Some(0)),
    ];
    for (assigned, device, partitions) in cases {
        let placed = placement(Backend::CoreMl, Some(&assigned), Precision::Fp32);
        assert_eq!(placed.backend, "coreml");
        assert_eq!(placed.device, device, "{assigned:?}");
        assert_eq!(placed.partitions, partitions, "{assigned:?}");
        assert_eq!(placed.precision, Precision::Fp32);
    }
    let placed = placement(
        Backend::DirectMl,
        Some(&[part("DmlExecutionProvider", 9)]),
        Precision::Fp16,
    );
    assert_eq!(
        (placed.device, placed.precision),
        (Device::Gpu, Precision::Fp16)
    );
    // An assignment that could not be read is not claimed for either device.
    let placed = placement(Backend::CoreMl, None, Precision::Fp32);
    assert_eq!((placed.device, placed.partitions), (Device::Mixed, None));
    for assigned in [None, Some(&[part(cpu, 1), part(cpu, 1)][..])] {
        let placed = placement(Backend::Cpu, assigned, Precision::Fp32);
        assert_eq!(placed.backend, "cpu");
        assert_eq!((placed.device, placed.partitions), (Device::Cpu, None));
    }
}

#[test]
fn the_compile_cache_is_keyed_by_model_runtime_and_system_build() {
    let sha = "ab".repeat(32);
    assert_eq!(
        cache::key(&sha, "1.28.0", "25F84").unwrap(),
        format!("{sha}-onnxruntime-1.28.0-25F84")
    );
    assert_eq!(cache::key(&sha, "1.28.0", "../x"), None);
    assert_eq!(cache::key("", "1.28.0", "25F84"), None);
    let plist = "<dict>\n\t<key>ProductBuildVersion</key>\n\t<string>25F84</string>\n\
                 \t<key>ProductVersion</key>\n\t<string>26.5.2</string>\n</dict>";
    assert_eq!(cache::build_of(plist).as_deref(), Some("25F84"));
    assert_eq!(cache::build_of("<key>ProductVersion</key>"), None);
    assert_eq!(
        cache::build_of("<key>ProductBuildVersion</key><string>a/b</string>"),
        None
    );
    if cfg!(target_os = "macos") {
        assert!(cache::os_build().is_some());
    }
    // Opening a key removes the model's other keys and nobody else's; removing a model removes
    // all of its own.
    let root = Dir::new("cache");
    let coreml = root.0.join("coreml");
    let other = "cd".repeat(32);
    for name in [
        format!("{sha}-onnxruntime-1.27.0-25F84"),
        format!("{sha}-onnxruntime-1.28.0-24A1"),
        format!("{other}-onnxruntime-1.28.0-25F84"),
    ] {
        fs::create_dir_all(coreml.join(name)).unwrap();
    }
    if let Some(dir) = cache::compile_dir(&root.0, &sha, "1.28.0") {
        assert!(dir.starts_with(&coreml));
        let mut left: Vec<_> = fs::read_dir(&coreml)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [format!("{other}-onnxruntime-1.28.0-25F84")]);
        fs::create_dir_all(&dir).unwrap();
    }
    assert_eq!(remove_compiled(&root.0, &other), 1);
    assert!(
        !coreml
            .join(format!("{other}-onnxruntime-1.28.0-25F84"))
            .exists()
    );
}

/// The fixed-shape copy and the Core ML backend, where there is one.
#[cfg(target_os = "macos")]
mod coreml {
    use super::*;

    #[test]
    fn coreml_runs_the_fixed_copy_written_once_and_other_shapes_on_the_cpu() {
        let dir = Dir::new("coreml");
        let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
        let cache = Dir::new("coreml-cache");
        let runtime = OnnxRuntime::new(Some(cache.0.clone()));
        let session = open(&runtime, &model, &["coreml", "cpu"]).unwrap();
        // The copy and its record, beside the model.
        let copy = fixed::copy_path(model.path());
        let record_path = fixed::record_path(model.path());
        assert!(copy.is_file() && record_path.is_file());
        let record: fixed::Record =
            serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        assert_eq!(record.format, fixed::RECORD_FORMAT);
        assert_eq!(record.source_sha256, model.identity().sha256);
        assert_eq!(record.runtime_version, PINNED_VERSION);
        assert_eq!(record.dimensions, ["batch"]);
        assert_eq!(record.fixed_shapes(), [("x".to_owned(), vec![1, 3])]);
        let written = record.copy.clone().unwrap();
        assert_eq!(written.file, "model.onnx.coreml-fixed.onnx");
        assert_eq!(written.sha256, fixed::sha256(&copy).unwrap());
        // The session reports the original's free batch and where Core ML put the graph.
        assert_eq!(session.inputs()[0].shape, [None, Some(3)]);
        let placement = session.placement().clone();
        assert_eq!(placement.backend, "coreml");
        assert_eq!(placement.precision, Precision::Fp32);
        // Core ML takes both nodes, as one fused part.
        assert_eq!(
            (placement.device, placement.partitions),
            (Device::Gpu, Some(1)),
            "{placement:?}"
        );
        // The compile cache is under the runtime's root, keyed by this model.
        let compiled = fs::read_dir(cache.0.join("coreml"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(compiled.len(), 1, "{compiled:?}");
        assert!(compiled[0].starts_with(&format!(
            "{}-onnxruntime-{PINNED_VERSION}-",
            model.identity().sha256
        )));
        // One row runs the copy on Core ML, close to exact; two run the original on the CPU,
        // exactly.
        let one = x(1);
        let want = expected(&one);
        let got = run(session.as_ref(), one);
        for (got, want) in got.iter().zip(&want) {
            assert!((got - want).abs() < 1e-3, "{got} against {want}");
        }
        let two = x(2);
        let want = expected(&two);
        assert_eq!(run(session.as_ref(), two), want);
        drop(session);
        // A second open reuses the copy: the record is not written again.
        let modified = fs::metadata(&record_path).unwrap().modified().unwrap();
        let again = open(&runtime, &model, &["coreml"]).unwrap();
        assert_eq!(again.placement(), &placement);
        assert_eq!(
            fs::metadata(&record_path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            runtime.prepare(&model, &Cancel::never()).unwrap(),
            [copy.clone(), record_path.clone()]
        );
    }

    #[test]
    fn a_stale_or_damaged_copy_is_written_again() {
        let dir = Dir::new("coreml-stale");
        let model = dir.model(&affine(Side::Named("batch"), SCALE, BIAS));
        let runtime = OnnxRuntime::new(None);
        let cancel = Cancel::never();
        let first = runtime.prepare(&model, &cancel).unwrap();
        assert_eq!(first.len(), 2);
        let record_path = fixed::record_path(model.path());
        let original: fixed::Record =
            serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        // A damaged copy fails its recorded hash, and is replaced.
        let copy = fixed::copy_path(model.path());
        let mut bytes = fs::read(&copy).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&copy, &bytes).unwrap();
        assert!(fixed::matching(&model, PINNED_VERSION).is_none());
        runtime.prepare(&model, &cancel).unwrap();
        assert!(fixed::matching(&model, PINNED_VERSION).is_some());
        // A record of another model, runtime or format is replaced too.
        for stale in [
            fixed::Record {
                source_sha256: "00".repeat(32),
                ..original.clone()
            },
            fixed::Record {
                runtime_version: "1.27.0".into(),
                ..original.clone()
            },
            fixed::Record {
                format: fixed::RECORD_FORMAT + 1,
                ..original.clone()
            },
        ] {
            fs::write(&record_path, serde_json::to_vec(&stale).unwrap()).unwrap();
            assert!(fixed::matching(&model, PINNED_VERSION).is_none());
            runtime.prepare(&model, &cancel).unwrap();
            let rewritten: fixed::Record =
                serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
            assert_eq!(rewritten, original);
        }
        // Unknown fields are not a record this build reads.
        fs::write(&record_path, b"{\"format\":1,\"extra\":true}").unwrap();
        assert!(fixed::matching(&model, PINNED_VERSION).is_none());
        // No partial file is left behind.
        let names: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".partial")),
            "{names:?}"
        );
    }

    #[test]
    fn a_static_model_needs_no_copy_and_an_unnamed_free_dimension_stays_on_the_cpu() {
        let dir = Dir::new("coreml-static");
        let model = dir.model(&affine(Side::Fixed(2), SCALE, BIAS));
        let runtime = OnnxRuntime::new(None);
        let session = open(&runtime, &model, &["coreml"]).unwrap();
        assert!(!fixed::copy_path(model.path()).exists());
        let record = fixed::matching(&model, PINNED_VERSION).unwrap();
        assert_eq!(record.copy, None);
        assert!(record.dimensions.is_empty());
        let input = x(2);
        let want = expected(&input);
        for (got, want) in run(session.as_ref(), input).iter().zip(&want) {
            assert!((got - want).abs() < 1e-3, "{got} against {want}");
        }
        let unnamed = Dir::new("coreml-unnamed");
        let model = unnamed.model(&affine(Side::Unnamed, SCALE, BIAS));
        let error = refusal(open(&runtime, &model, &["coreml"]));
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(
            error.detail.contains("unnamed free dimension"),
            "{}",
            error.detail
        );
        assert!(!fixed::record_path(model.path()).exists());
        let session = open(&runtime, &model, &["cpu"]).unwrap();
        let input = x(3);
        let want = expected(&input);
        assert_eq!(run(session.as_ref(), input), want);
    }
}

/// The pinned EfficientViT-SAM L0 from the object-removal prototype, opened on the CPU and on Core
/// ML over its fixed-shape copy, matched against each other. Needs the models: `--ignored`, with
/// `LUXFORGE_ONNX_MODELS` naming their directory or the main checkout's
/// `private/models/object-removal/` holding them.
#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use luxforge_core::inference::ModelFile;

    /// Mask agreement the prototype measured between the CPU and Core ML over 200 hover points:
    /// IoU 0.998 or better.
    const MIN_MASK_IOU: f64 = 0.998;
    /// The embedding's largest difference the prototype measured was 5 × 10⁻⁵; this allows 20
    /// times that.
    const MAX_EMBEDDING_DIFFERENCE: f32 = 1e-3;

    struct Pinned {
        file: &'static str,
        sha256: &'static str,
    }

    const ENCODER: Pinned = Pinned {
        file: "l0_encoder.onnx",
        sha256: "7a37ebd16e65c31253416e33897e7e6b07699127765d538e99ecbfaa3ad1e727",
    };
    const DECODER: Pinned = Pinned {
        file: "l0_decoder.onnx",
        sha256: "053e72c5703736a8a8a92482c9990c4a26dde9dcb7ff26b05843cf0bc433f42f",
    };

    fn models() -> PathBuf {
        std::env::var_os("LUXFORGE_ONNX_MODELS").map_or_else(
            || {
                // A linked worktree's private models are its main checkout's.
                let root = luxforge_testbase::paths::repository();
                let main = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                    .output()
                    .ok()
                    .and_then(|out| String::from_utf8(out.stdout).ok())
                    .map(|dir| PathBuf::from(dir.trim()).join(".."))
                    .unwrap_or(root);
                main.join("private/models/object-removal")
            },
            PathBuf::from,
        )
    }

    /// A copy of the pinned model in its own directory, as an install holds it, checked against
    /// its pin.
    fn installed(pinned: &Pinned, dir: &Dir) -> ModelFile {
        let source = models().join(pinned.file);
        assert_eq!(
            fixed::sha256(&source).unwrap_or_else(|error| panic!("{}: {error}", source.display())),
            pinned.sha256,
            "{} is not the pinned model",
            source.display()
        );
        let path = dir.0.join(pinned.file);
        fs::copy(&source, &path).unwrap();
        ModelFile::new(
            ModelIdentity {
                id: pinned.file.trim_end_matches(".onnx").into(),
                version: "1".into(),
                sha256: pinned.sha256.into(),
            },
            path,
        )
    }

    /// A deterministic 512² picture of a few flat shapes on a graded ground, normalized as the
    /// encoder expects.
    fn picture() -> Tensor {
        const SIDE: usize = 512;
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let mut values = vec![0.0_f32; 3 * SIDE * SIDE];
        for y in 0..SIDE {
            for x in 0..SIDE {
                let (fx, fy) = (x as f32, y as f32);
                let mut rgb = [
                    0.2 + 0.3 * fy / SIDE as f32,
                    0.35,
                    0.25 + 0.2 * fx / SIDE as f32,
                ];
                if (fx - 150.0).powi(2) + (fy - 170.0).powi(2) < 70.0_f32.powi(2) {
                    rgb = [0.9, 0.2, 0.15];
                }
                if (300.0..440.0).contains(&fx) && (260.0..420.0).contains(&fy) {
                    rgb = [0.1, 0.3, 0.85];
                }
                if (60.0..200.0).contains(&fx) && (350.0..380.0).contains(&fy) {
                    rgb = [0.95, 0.95, 0.9];
                }
                for (channel, value) in rgb.iter().enumerate() {
                    values[channel * SIDE * SIDE + y * SIDE + x] =
                        (value - MEAN[channel]) / STD[channel];
                }
            }
        }
        Tensor::new(
            "input_image",
            vec![1, 3, SIDE as u64, SIDE as u64],
            TensorData::F32(values),
        )
        .unwrap()
    }

    fn largest_difference(a: &Tensor, b: &Tensor) -> f32 {
        assert_eq!(a.shape(), b.shape());
        a.f32()
            .unwrap()
            .iter()
            .zip(b.f32().unwrap())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max)
    }

    /// The masks (logits above zero) of one decode, for each point.
    fn decode(decoder: &dyn Session, embedding: &Tensor, points: &[[f32; 2]]) -> Tensor {
        let batch = points.len() as u64;
        let embedding = Tensor::new(
            "image_embeddings",
            embedding.shape().to_vec(),
            embedding.data().clone(),
        )
        .unwrap();
        let coords = Tensor::new(
            "point_coords",
            vec![batch, 1, 2],
            TensorData::F32(points.iter().flatten().copied().collect()),
        )
        .unwrap();
        let labels = Tensor::new(
            "point_labels",
            vec![batch, 1],
            TensorData::F32(vec![1.0; points.len()]),
        )
        .unwrap();
        let outputs = decoder
            .run(&[embedding, coords, labels], &Cancel::never())
            .unwrap();
        outputs
            .into_iter()
            .find(|output| output.name() == "masks")
            .expect("a masks output")
    }

    fn iou(a: &[f32], b: &[f32]) -> f64 {
        let (mut both, mut either) = (0_u64, 0_u64);
        for (a, b) in a.iter().zip(b) {
            let (a, b) = (*a > 0.0, *b > 0.0);
            both += u64::from(a && b);
            either += u64::from(a || b);
        }
        if either == 0 {
            1.0
        } else {
            both as f64 / either as f64
        }
    }

    #[test]
    #[ignore = "needs the pinned EfficientViT-SAM models (LUXFORGE_ONNX_MODELS)"]
    fn efficientvit_sam_matches_between_the_cpu_and_core_ml_over_its_fixed_copy() {
        let dir = Dir::new("evit");
        let cache = Dir::new("evit-cache");
        let encoder = installed(&ENCODER, &dir);
        let decoder = installed(&DECODER, &dir);
        let runtime = OnnxRuntime::new(Some(cache.0.clone()));
        let cpu_encoder = open(&runtime, &encoder, &["cpu"]).unwrap();
        let cpu_decoder = open(&runtime, &decoder, &["cpu"]).unwrap();
        let started = std::time::Instant::now();
        let gpu_encoder = open(&runtime, &encoder, &["coreml"]).unwrap();
        let gpu_decoder = open(&runtime, &decoder, &["coreml"]).unwrap();
        println!("Core ML first open of both: {:?}", started.elapsed());
        for session in [&cpu_encoder, &cpu_decoder, &gpu_encoder, &gpu_decoder] {
            println!("{}: {:?}", session.model().id, session.placement());
        }
        for session in [&gpu_encoder, &gpu_decoder] {
            let placement = session.placement();
            assert_eq!(placement.backend, "coreml");
            assert_eq!(placement.precision, Precision::Fp32);
            assert_ne!(placement.device, Device::Cpu, "{placement:?}");
        }
        assert_eq!(cpu_encoder.placement().device, Device::Cpu);
        let picture = picture();
        let cpu_embedding = cpu_encoder
            .run(std::slice::from_ref(&picture), &Cancel::never())
            .unwrap()
            .remove(0);
        let gpu_embedding = gpu_encoder
            .run(&[picture], &Cancel::never())
            .unwrap()
            .remove(0);
        let difference = largest_difference(&cpu_embedding, &gpu_embedding);
        println!(
            "embedding {:?}: largest difference {difference:e}",
            cpu_embedding.shape()
        );
        assert!(difference <= MAX_EMBEDDING_DIFFERENCE, "{difference}");
        // Hover points in the 1024² prompt frame (the picture is 512², so twice its pixels): on
        // each shape, on an edge and on the ground.
        let points = [
            [300.0, 340.0],
            [740.0, 680.0],
            [260.0, 730.0],
            [440.0, 340.0],
            [900.0, 120.0],
            [100.0, 950.0],
            [600.0, 520.0],
            [880.0, 840.0],
        ];
        let mut worst = 1.0_f64;
        let mut logits = 0.0_f32;
        for point in points {
            // One point runs on Core ML over the copy.
            let cpu = decode(cpu_decoder.as_ref(), &cpu_embedding, &[point]);
            let gpu = decode(gpu_decoder.as_ref(), &cpu_embedding, &[point]);
            let agreement = iou(cpu.f32().unwrap(), gpu.f32().unwrap());
            let difference = largest_difference(&cpu, &gpu);
            println!("point {point:?}: mask IoU {agreement:.5}, logits within {difference:e}");
            worst = worst.min(agreement);
            logits = logits.max(difference);
        }
        println!("worst mask IoU {worst:.5} (at least {MIN_MASK_IOU})");
        assert!(worst >= MIN_MASK_IOU, "{worst}");
        // Close, not identical: Core ML ran it, not the CPU.
        assert!(logits > 0.0);
        // A batch of two points is a shape the copy does not take: the CPU session of the
        // original runs it, exactly as the CPU backend does.
        let cpu = decode(cpu_decoder.as_ref(), &cpu_embedding, &points[..2]);
        let gpu = decode(gpu_decoder.as_ref(), &cpu_embedding, &points[..2]);
        assert_eq!(cpu, gpu);
        // Reopened, the copy and the compile are reused.
        drop((gpu_encoder, gpu_decoder));
        let started = std::time::Instant::now();
        let _encoder = open(&runtime, &encoder, &["coreml"]).unwrap();
        let _decoder = open(&runtime, &decoder, &["coreml"]).unwrap();
        println!("Core ML reopen of both, cached: {:?}", started.elapsed());
    }
}
