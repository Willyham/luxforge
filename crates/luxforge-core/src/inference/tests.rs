//! The port's contract, held against the deterministic fake: tensors and their bounds, choosing a
//! backend, opening and its refusals, a session's run, and a generator's determinism, coverage,
//! progress, cancellation at step boundaries, failure knobs and provenance.
use super::{
    Conditioning, ElementType, GenerateRequest, Generator, InferenceRuntime, MAX_PROMPT_CHARS,
    ModelFile, ModelIdentity, OpenOptions, Precision, Session, StepProgress, Tensor, TensorData,
    TensorSpec,
    fake::{FAKE_BACKEND, FAKE_INPUT, FAKE_OUTPUT, FAKE_RUNTIME, FAKE_SAMPLER, FakeInference},
    prompt_hash,
};
use crate::{Cancel, ErrorKind};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

fn assert_port<T: ?Sized + Send + Sync>() {}

/// A model file under a fresh temporary directory, removed with it.
struct Model {
    dir: PathBuf,
    file: ModelFile,
}

impl Model {
    fn new(name: &str, sha: char) -> Self {
        let dir = luxforge_testbase::paths::temp_dir(&format!("inference-{name}"));
        let path = dir.join("model.onnx");
        fs::write(&path, b"not a real model").unwrap();
        Self {
            dir,
            file: ModelFile::new(
                ModelIdentity {
                    id: "segmenter".into(),
                    version: "1".into(),
                    sha256: sha.to_string().repeat(64),
                },
                path,
            ),
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn options(precision: Precision) -> OpenOptions {
    OpenOptions {
        backends: vec!["coreml".into(), FAKE_BACKEND.into()],
        precision,
    }
}

#[test]
fn the_port_is_object_safe_send_and_sync() {
    assert_port::<dyn InferenceRuntime>();
    assert_port::<dyn Session>();
    assert_port::<dyn Generator>();
    let runtime = FakeInference::shared();
    assert_eq!(format!("{runtime:?}"), format!("{FAKE_RUNTIME} 1"));
}

#[test]
fn tensors_hold_exactly_their_shape_and_specs_admit_free_dimensions() {
    let tensor = Tensor::new("x", vec![1, 2, 3], TensorData::F32(vec![0.0; 6])).unwrap();
    assert_eq!((tensor.name(), tensor.shape()), ("x", &[1, 2, 3][..]));
    assert_eq!(
        Tensor::new("x", vec![2, 2], TensorData::U8(vec![0; 3]))
            .unwrap_err()
            .kind,
        ErrorKind::Validation
    );
    assert_eq!(
        Tensor::new("x", vec![1 << 20, 1 << 20], TensorData::F32(Vec::new()))
            .unwrap_err()
            .kind,
        ErrorKind::ResourceLimit,
        "refused before anything is allocated"
    );
    assert_eq!(
        Tensor::new("x", vec![u64::MAX, 3], TensorData::Bool(Vec::new()))
            .unwrap_err()
            .kind,
        ErrorKind::ResourceLimit
    );
    let spec = TensorSpec {
        name: "x".into(),
        element: ElementType::F32,
        shape: vec![Some(1), None, Some(3)],
    };
    assert!(spec.admits(&tensor));
    let wide = Tensor::new("x", vec![1, 9, 3], TensorData::F32(vec![0.0; 27])).unwrap();
    assert!(spec.admits(&wide));
    let wrong = Tensor::new("x", vec![2, 2, 3], TensorData::F32(vec![0.0; 12])).unwrap();
    assert!(!spec.admits(&wrong));
    let integers = Tensor::new("x", vec![1, 2, 3], TensorData::I64(vec![0; 6])).unwrap();
    assert!(!spec.admits(&integers));
}

#[test]
fn a_model_opens_on_the_first_backend_offered_and_refuses_what_cannot_open() {
    let model = Model::new("open", 'a');
    let runtime = FakeInference::new();
    let controls = runtime.controls();
    let cancel = Cancel::new();
    // Core ML is not offered here, so the session lands on the CPU backend.
    let session = runtime
        .open_session(&model.file, &options(Precision::Fp32), &cancel)
        .unwrap();
    assert_eq!(session.placement().backend, FAKE_BACKEND);
    assert_eq!(session.placement().precision, Precision::Fp32);
    assert_eq!(session.model(), model.file.identity());
    assert_eq!((controls.opens(), controls.live()), (1, 1));
    drop(session);
    assert_eq!(controls.live(), 0, "dropping a model releases it");
    let refused = runtime
        .open_session(
            &model.file,
            &OpenOptions {
                backends: vec!["coreml".into()],
                precision: Precision::Fp32,
            },
            &cancel,
        )
        .err()
        .unwrap();
    assert_eq!(refused.kind, ErrorKind::NotReady);
    assert!(refused.detail.contains("coreml"), "{}", refused.detail);
    let missing = ModelFile::new(model.file.identity().clone(), model.dir.join("absent"));
    assert_eq!(
        runtime
            .open_generator(&missing, &options(Precision::Fp32), &cancel)
            .err()
            .unwrap()
            .kind,
        ErrorKind::SourceUnavailable
    );
    controls.fail_opens(true);
    assert_eq!(
        runtime
            .open_generator(&model.file, &options(Precision::Fp32), &cancel)
            .err()
            .unwrap()
            .kind,
        ErrorKind::Render
    );
    controls.fail_opens(false);
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert_eq!(
        runtime
            .open_session(&model.file, &options(Precision::Fp32), &cancelled)
            .err()
            .unwrap()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!((controls.opens(), controls.live()), (1, 0));
}

#[test]
fn a_session_runs_named_tensors_deterministically_at_its_precision() {
    let model = Model::new("session", 'b');
    let runtime = FakeInference::new();
    let cancel = Cancel::new();
    let input = Tensor::new(
        FAKE_INPUT,
        vec![1, 1, 2, 3],
        TensorData::F32(vec![-1.0, -0.5, 0.0, 0.25, 0.5, 1.0]),
    )
    .unwrap();
    let full = runtime
        .open_session(&model.file, &options(Precision::Fp32), &cancel)
        .unwrap();
    assert_eq!(full.inputs()[0].name, FAKE_INPUT);
    assert_eq!(full.outputs()[0].name, FAKE_OUTPUT);
    let first = full.run(std::slice::from_ref(&input), &cancel).unwrap();
    let again = full.run(std::slice::from_ref(&input), &cancel).unwrap();
    assert_eq!(first, again, "a pure function of the input and the model");
    assert_eq!(first[0].name(), FAKE_OUTPUT);
    assert_eq!(first[0].shape(), input.shape());
    let values = first[0].f32().unwrap();
    assert!(
        values.windows(2).all(|pair| pair[0] < pair[1]),
        "{values:?}"
    );
    // Another model's hash is another function.
    let other = Model::new("session-other", 'c');
    let other = runtime
        .open_session(&other.file, &options(Precision::Fp32), &cancel)
        .unwrap()
        .run(std::slice::from_ref(&input), &cancel)
        .unwrap();
    assert_ne!(other, first);
    // At fp16 every output is a half float.
    let half = runtime
        .open_session(&model.file, &options(Precision::Fp16), &cancel)
        .unwrap();
    assert_eq!(half.placement().precision, Precision::Fp16);
    let halved = half.run(std::slice::from_ref(&input), &cancel).unwrap();
    for (half, full) in halved[0].f32().unwrap().iter().zip(values) {
        assert_eq!(*half, half::f16::from_f32(*full).to_f32());
    }
    // Refusals: a wrong name or rank, a failing run and a cancelled one.
    let wrong = Tensor::new("pixels", vec![1, 1, 2, 3], TensorData::F32(vec![0.0; 6])).unwrap();
    assert_eq!(
        full.run(&[wrong], &cancel).unwrap_err().kind,
        ErrorKind::Validation
    );
    let flat = Tensor::new(FAKE_INPUT, vec![6], TensorData::F32(vec![0.0; 6])).unwrap();
    assert_eq!(
        full.run(&[flat], &cancel).unwrap_err().kind,
        ErrorKind::Validation
    );
    runtime.controls().fail_runs(true);
    assert_eq!(
        full.run(std::slice::from_ref(&input), &cancel)
            .unwrap_err()
            .kind,
        ErrorKind::Render
    );
    runtime.controls().fail_runs(false);
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert_eq!(
        full.run(std::slice::from_ref(&input), &cancelled)
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(runtime.controls().runs(), 4);
}

/// A 40 × 24 gradient crop with a 24 × 16 rectangle at (8, 4) masked, its edge half covered.
fn crop() -> (u32, u32, Vec<f32>, Vec<f32>) {
    let (width, height) = (40_u32, 24_u32);
    let mut image = Vec::new();
    let mut mask = Vec::new();
    for y in 0..height {
        for x in 0..width {
            image.extend([x as f32 / width as f32, y as f32 / height as f32, 0.25]);
            let inside = (8..32).contains(&x) && (4..20).contains(&y);
            let edge = inside && (x == 8 || x == 31 || y == 4 || y == 19);
            mask.push(match (inside, edge) {
                (true, true) => 0.5,
                (true, false) => 1.0,
                _ => 0.0,
            });
        }
    }
    (width, height, image, mask)
}

fn request<'a>(
    image: &'a [f32],
    mask: &'a [f32],
    seed: u64,
    conditioning: Conditioning<'a>,
) -> GenerateRequest<'a> {
    GenerateRequest {
        width: 40,
        height: 24,
        image,
        mask,
        conditioning,
        seed,
        steps: 4,
    }
}

#[test]
fn a_generator_fills_only_the_mask_deterministically_and_seeds_differ_visibly() {
    let model = Model::new("generator", 'd');
    let runtime = FakeInference::new();
    let cancel = Cancel::new();
    let generator = runtime
        .open_generator(&model.file, &options(Precision::Fp16), &cancel)
        .unwrap();
    let (_, _, image, mask) = crop();
    let mut steps = Vec::new();
    let first = generator
        .generate(
            &request(&image, &mask, 7, Conditioning::None),
            &mut |step| steps.push(step),
            &cancel,
        )
        .unwrap();
    assert_eq!(
        steps,
        (1..=4)
            .map(|step| StepProgress { step, steps: 4 })
            .collect::<Vec<_>>(),
        "progress after every step"
    );
    let again = generator
        .generate(
            &request(&image, &mask, 7, Conditioning::None),
            &mut |_| {},
            &cancel,
        )
        .unwrap();
    assert_eq!(first, again, "the same inputs and seed give the same fill");
    // Every unmasked value is the input's, bit for bit; every masked one changed.
    for (pixel, coverage) in mask.iter().enumerate() {
        for channel in 0..3 {
            let (input, output) = (image[pixel * 3 + channel], first.image[pixel * 3 + channel]);
            if *coverage == 0.0 {
                assert_eq!(input.to_bits(), output.to_bits(), "pixel {pixel}");
            }
        }
    }
    let changed = |a: &[f32], b: &[f32]| {
        a.chunks(3)
            .zip(b.chunks(3))
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| (a - b).abs() > 0.05))
            .count()
    };
    let filled = mask.iter().filter(|coverage| **coverage == 1.0).count();
    assert!(changed(&image, &first.image) * 10 >= filled * 9);
    // Another seed is another, visibly different fill of the same pixels.
    let other = generator
        .generate(
            &request(&image, &mask, 8, Conditioning::None),
            &mut |_| {},
            &cancel,
        )
        .unwrap();
    assert!(
        changed(&first.image, &other.image) * 2 >= filled,
        "seeds 7 and 8 differ in at least half of the filled pixels"
    );
    // A prompt is another fill again, and its hash is recorded, never its text.
    let prompted = generator
        .generate(
            &request(&image, &mask, 7, Conditioning::Prompt("a stone wall")),
            &mut |_| {},
            &cancel,
        )
        .unwrap();
    assert_ne!(prompted.image, first.image);
    assert_eq!(
        prompted.provenance.prompt_hash,
        Some(prompt_hash("a stone wall"))
    );
    // An embedding conditions it too.
    let embedding = Tensor::new("empty-scene", vec![4], TensorData::F32(vec![0.1; 4])).unwrap();
    let embedded = generator
        .generate(
            &request(&image, &mask, 7, Conditioning::Embedding(&embedding)),
            &mut |_| {},
            &cancel,
        )
        .unwrap();
    assert_ne!(embedded.image, first.image);
    assert_eq!(runtime.controls().generations(), 5);
    // Provenance: every field a real model fills.
    let provenance = &first.provenance;
    assert_eq!(&provenance.model, model.file.identity());
    assert_eq!(provenance.runtime.name, FAKE_RUNTIME);
    assert_eq!(
        (
            provenance.backend.as_str(),
            provenance.precision,
            provenance.seed,
            provenance.steps,
            provenance.sampler.as_str(),
            provenance.prompt_hash.as_deref(),
        ),
        (FAKE_BACKEND, Precision::Fp16, 7, 4, FAKE_SAMPLER, None)
    );
    let fields = provenance.fields();
    assert_eq!(fields["model"], "segmenter");
    assert_eq!(fields["precision"], "fp16");
    assert_eq!(fields["seed"], 7);
    assert!(!fields.contains_key("prompt_hash"));
    assert!(serde_json::to_vec(&fields).unwrap().len() < 1024);
}

#[test]
fn a_generation_stops_at_a_step_boundary_fails_on_its_knob_and_is_slow_on_request() {
    let model = Model::new("generator-knobs", 'e');
    let runtime = FakeInference::new();
    let controls = runtime.controls();
    let cancel = Cancel::new();
    let generator = runtime
        .open_generator(&model.file, &options(Precision::Fp32), &cancel)
        .unwrap();
    let (_, _, image, mask) = crop();
    // Cancelled from its own progress after step 2: step 3 never reports.
    let mut reported = Vec::new();
    let stop = Cancel::new();
    let error = generator
        .generate(
            &request(&image, &mask, 1, Conditioning::None),
            &mut |step| {
                reported.push(step.step);
                if step.step == 2 {
                    stop.cancel();
                }
            },
            &stop,
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(reported, [1, 2]);
    controls.fail_at_step(Some(3));
    let mut reported = Vec::new();
    let failed = generator
        .generate(
            &request(&image, &mask, 1, Conditioning::None),
            &mut |step| reported.push(step.step),
            &cancel,
        )
        .unwrap_err();
    assert_eq!(failed.kind, ErrorKind::Render);
    assert_eq!(reported, [1, 2]);
    controls.fail_at_step(None);
    controls.set_step_delay(Duration::from_millis(15));
    let started = Instant::now();
    generator
        .generate(
            &request(&image, &mask, 1, Conditioning::None),
            &mut |_| {},
            &cancel,
        )
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(60),
        "four slow steps"
    );
    assert_eq!(
        controls.generations(),
        1,
        "only the finished generation counts"
    );
}

#[test]
fn a_request_no_generator_should_run_is_refused_by_name() {
    let (_, _, image, mask) = crop();
    let long = "x".repeat(MAX_PROMPT_CHARS + 1);
    let integers = Tensor::new("ids", vec![2], TensorData::I64(vec![1, 2])).unwrap();
    let mut bad_mask = mask.clone();
    bad_mask[3] = 1.5;
    let mut bad_image = image.clone();
    bad_image[0] = f32::NAN;
    let cases: Vec<(GenerateRequest<'_>, &str)> = vec![
        (
            GenerateRequest {
                steps: 0,
                ..request(&image, &mask, 0, Conditioning::None)
            },
            "steps",
        ),
        (
            GenerateRequest {
                width: 41,
                ..request(&image, &mask, 0, Conditioning::None)
            },
            "image values",
        ),
        (
            GenerateRequest {
                width: 0,
                ..request(&image, &mask, 0, Conditioning::None)
            },
            "width",
        ),
        (
            request(&image, &bad_mask, 0, Conditioning::None),
            "coverage",
        ),
        (request(&bad_image, &mask, 0, Conditioning::None), "finite"),
        (
            request(&image, &mask, 0, Conditioning::Prompt(&long)),
            "prompt",
        ),
        (
            request(&image, &mask, 0, Conditioning::Prompt("  ")),
            "prompt",
        ),
        (
            request(&image, &mask, 0, Conditioning::Embedding(&integers)),
            "f32",
        ),
    ];
    for (request, expected) in cases {
        let error = request.check().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains(expected), "{}", error.detail);
    }
    let fits = "y".repeat(MAX_PROMPT_CHARS);
    request(&image, &mask, 0, Conditioning::Prompt(&fits))
        .check()
        .unwrap();
}
