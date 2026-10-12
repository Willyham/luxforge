//! The runtime's contract without the model: its layout against the registered descriptors, its
//! backend names, admission, the inputs it builds and how it refuses what it cannot open. The
//! native tests at the end need the pinned model files and are `#[ignore]`d: run them with
//! `--ignored` (and `--release`), the files in `LUXFORGE_GENERATIVE_MODELS` or the main checkout's
//! `private/models/generative/`.
use crate::{
    CPU_BACKEND, DiffusionRuntime, LORA_PROMPT, MAX_SIDE, METAL_BACKEND, RUNTIME_NAME,
    generator::{admit, inputs},
    layout::{Files, REMOVAL_CONDITIONING, REMOVAL_LORA, TOKENIZER, VAE},
    offered_backends, outlined, replace_prompt,
};
use luxforge_core::{
    Cancel, ErrorKind, ModuleRegistry,
    capabilities::descriptor::CapabilityKind,
    inference::{
        Conditioning, GenerateRequest, InferenceRuntime, ModelFile, ModelIdentity, OpenOptions,
        Precision, Tensor, TensorData,
    },
};
use std::{fs, path::PathBuf};

const AI: &str = "luxforge.ai";
const GENERATOR: &str = "generative-klein-4b";
const ENCODER: &str = "generative-klein-text-encoder";

fn identity(id: &str) -> ModelIdentity {
    ModelIdentity {
        id: id.into(),
        version: "1".into(),
        sha256: "0".repeat(64),
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "luxforge-diffusion-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `luxforge.ai`'s klein resources and runtimes are exactly what this runtime opens: the further
/// files it looks for by name, and the backend names it serves.
#[test]
fn the_layout_is_the_registered_descriptors() {
    let registry = ModuleRegistry::builtin();
    let descriptor = registry
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.id == AI)
        .expect("every run serves luxforge.ai");
    let names = |id: &str| -> Vec<String> {
        descriptor
            .resources
            .iter()
            .find(|resource| resource.id == id)
            .unwrap()
            .files
            .iter()
            .map(|file| file.name.clone())
            .collect()
    };
    assert_eq!(names(GENERATOR), [VAE, REMOVAL_LORA, REMOVAL_CONDITIONING]);
    assert_eq!(names(ENCODER), [TOKENIZER]);
    let runtimes: Vec<_> = descriptor
        .capabilities
        .iter()
        .filter_map(|capability| match &capability.kind {
            CapabilityKind::LocalRuntime {
                resource,
                backends,
                precision,
                with,
                ..
            } if resource == GENERATOR || resource == ENCODER => {
                Some((resource.clone(), backends.clone(), *precision, with.clone()))
            }
            _ => None,
        })
        .collect();
    let backends = vec![METAL_BACKEND.to_owned(), CPU_BACKEND.to_owned()];
    assert_eq!(
        runtimes,
        [
            (GENERATOR.into(), backends.clone(), Precision::Mixed, vec![]),
            (ENCODER.into(), backends.clone(), Precision::Mixed, vec![GENERATOR.into()]),
        ]
    );
    assert!(offered_backends().iter().all(|name| backends.contains(name)));
}

/// The names share a host with ONNX Runtime's and the developer fake's.
#[test]
fn its_backend_names_collide_with_no_other_runtime() {
    for taken in ["cpu", "coreml", "directml", "fake"] {
        assert_ne!(METAL_BACKEND, taken);
        assert_ne!(CPU_BACKEND, taken);
    }
    let offered = DiffusionRuntime::new().backends();
    if cfg!(target_os = "macos") {
        assert_eq!(offered, [METAL_BACKEND]);
    } else if cfg!(target_os = "linux") {
        assert_eq!(offered, [CPU_BACKEND]);
    } else {
        assert!(offered.is_empty());
    }
    assert_eq!(DiffusionRuntime::new().identity().name, RUNTIME_NAME);
}

#[test]
fn a_model_opens_its_files_by_name_and_names_a_missing_one() {
    let dir = temp("layout");
    let generator = dir.join("generator");
    let encoder = dir.join("encoder");
    fs::create_dir_all(&generator).unwrap();
    fs::create_dir_all(&encoder).unwrap();
    for name in [GENERATOR, VAE, REMOVAL_LORA] {
        fs::write(generator.join(name), b"x").unwrap();
    }
    let removal = ModelFile::new(identity(GENERATOR), generator.join(GENERATOR));
    let missing = Files::of(&removal).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::SourceUnavailable);
    assert!(missing.detail.contains(REMOVAL_CONDITIONING), "{missing:?}");
    fs::write(generator.join(REMOVAL_CONDITIONING), b"x").unwrap();
    let files = Files::of(&removal).unwrap();
    assert_eq!(files.vae, generator.join(VAE));
    assert_eq!(files.encoder, None);

    // Replace's slot holds the encoder with the generator as its companion.
    fs::write(encoder.join(ENCODER), b"x").unwrap();
    let replace = ModelFile::new(identity(ENCODER), encoder.join(ENCODER))
        .with_companions(vec![removal.clone()]);
    assert_eq!(
        Files::of(&replace).unwrap_err().kind,
        ErrorKind::SourceUnavailable,
        "the tokenizer is missing"
    );
    fs::write(encoder.join(TOKENIZER), b"{}").unwrap();
    let files = Files::of(&replace).unwrap();
    assert_eq!(files.transformer, generator.join(GENERATOR));
    assert_eq!(
        files.encoder,
        Some((encoder.join(ENCODER), encoder.join(TOKENIZER)))
    );
    assert_eq!(files.paths().len(), 6);
    let two = replace.with_companions(vec![removal.clone(), removal]);
    assert_eq!(Files::of(&two).unwrap_err().kind, ErrorKind::Incompatible);
    let _ = fs::remove_dir_all(dir);
}

fn request<'a>(
    side: u32,
    image: &'a [f32],
    mask: &'a [f32],
    conditioning: Conditioning<'a>,
) -> GenerateRequest<'a> {
    GenerateRequest {
        width: side,
        height: side,
        image,
        mask,
        conditioning,
        seed: 1,
        steps: 4,
    }
}

/// Every refusal comes before anything is allocated, by name.
#[test]
fn admission_refuses_by_name_what_klein_cannot_run() {
    let buffers = |side: u32| {
        let pixels = (side * side) as usize;
        (vec![0.5f32; pixels * 3], vec![0f32; pixels])
    };
    let (image, mask) = buffers(MAX_SIDE);
    admit(&request(MAX_SIDE, &image, &mask, Conditioning::None), false, "m").unwrap();
    let (image, mask) = buffers(MAX_SIDE + 16);
    let past = admit(&request(MAX_SIDE + 16, &image, &mask, Conditioning::None), false, "m")
        .unwrap_err();
    assert_eq!(past.kind, ErrorKind::ResourceLimit);
    assert_eq!(past.data.unwrap()["limit"], "generation-side");
    let (image, mask) = buffers(1000);
    let odd = admit(&request(1000, &image, &mask, Conditioning::None), false, "m").unwrap_err();
    assert_eq!(odd.kind, ErrorKind::Validation);
    let (image, mask) = buffers(512);
    let prompt = request(512, &image, &mask, Conditioning::Prompt("a red bicycle"));
    assert_eq!(
        admit(&prompt, false, "m").unwrap_err().kind,
        ErrorKind::Incompatible
    );
    admit(&prompt, true, "m").unwrap();
    let tensor = Tensor::new("c_crossattn", vec![1, 4], TensorData::F32(vec![0.0; 4])).unwrap();
    let embedding = request(512, &image, &mask, Conditioning::Embedding(&tensor));
    assert_eq!(
        admit(&embedding, true, "m").unwrap_err().kind,
        ErrorKind::Incompatible
    );
    // What the port refuses is refused too.
    let short = request(512, &image[..10], &mask, Conditioning::None);
    assert_eq!(
        admit(&short, true, "m").unwrap_err().kind,
        ErrorKind::Validation
    );
}

/// The inputs are the harness's: the hole mid-grey in the image the known latents come from, the
/// mask at 255 in the hole, and the reference the crop with the object still in it, outlined in
/// red `side / 64` beyond the hole's bounds and `max(side / 256, 2)` thick.
#[test]
fn the_inputs_grey_the_hole_and_outline_the_object() {
    let side = 64u32;
    let n = side as usize;
    let mut image = vec![0.25f32; n * n * 3];
    let mut mask = vec![0f32; n * n];
    for y in 20..30 {
        for x in 20..30 {
            mask[y * n + x] = 0.5;
            image[(y * n + x) * 3..(y * n + x) * 3 + 3].copy_from_slice(&[0.0, 1.0, 0.0]);
        }
    }
    let built = inputs(&request(side, &image, &mask, Conditioning::None));
    let at = |buffer: &[u8], x: usize, y: usize| buffer[(y * n + x) * 3..(y * n + x) * 3 + 3].to_vec();
    assert_eq!(at(&built.init, 25, 25), [128, 128, 128]);
    assert_eq!(at(&built.init, 5, 5), [64, 64, 64]);
    assert_eq!(built.mask[25 * n + 25], 255);
    assert_eq!(built.mask[5 * n + 5], 0);
    // The object stays in the reference; the outline is one pixel out (64 / 64) and 2 thick.
    assert_eq!(at(&built.reference, 25, 25), [0, 255, 0]);
    assert_eq!(at(&built.reference, 19, 25), [255, 0, 0]);
    assert_eq!(at(&built.reference, 20, 19), [255, 0, 0]);
    assert_eq!(at(&built.reference, 31, 31), [64, 64, 64], "outside the outline");
    assert_eq!(at(&built.reference, 27, 27), [0, 255, 0], "inside it");
    assert_eq!(at(&built.reference, 30, 30), [255, 0, 0]);
    assert_eq!(at(&built.reference, 17, 25), [64, 64, 64]);
    // No hole, no outline.
    let rgb = vec![7u8; n * n * 3];
    assert_eq!(outlined(&rgb, &vec![false; n * n], n, n), rgb);
}

#[test]
fn replace_prompts_follow_the_qualified_instruction() {
    assert_eq!(
        replace_prompt("  a red bicycle "),
        "Replace the highlighted object with a red bicycle"
    );
    assert_eq!(LORA_PROMPT, "Remove the highlighted object from the scene");
}

#[test]
fn it_opens_no_session_and_no_backend_it_does_not_offer() {
    let runtime = DiffusionRuntime::new();
    let model = ModelFile::new(identity(GENERATOR), "/nonexistent/generative-klein-4b");
    let cancel = Cancel::new();
    let options = |backend: &str| OpenOptions {
        backends: vec![backend.into()],
        precision: Precision::Mixed,
    };
    let session = runtime
        .open_session(&model, &options(METAL_BACKEND), &cancel)
        .err()
        .unwrap();
    assert_eq!(session.kind, ErrorKind::Incompatible);
    assert!(runtime.open_generator(&model, &options("cpu"), &cancel).is_err());
    if let Some(offered) = offered_backends().first() {
        let missing = runtime
            .open_generator(&model, &options(offered), &cancel)
            .err()
            .unwrap();
        assert_eq!(missing.kind, ErrorKind::SourceUnavailable, "{missing:?}");
    }
}

#[cfg(luxforge_sdcpp)]
mod native {
    //! Over the pinned model on this machine's backend.
    use super::*;
    use crate::KleinGenerator;
    use luxforge_core::inference::{Generator, Placement, StepProgress};
    use std::{
        path::Path,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    /// The pinned files' directory: `LUXFORGE_GENERATIVE_MODELS`, or the main checkout's
    /// `private/models/generative/`.
    fn models() -> PathBuf {
        if let Some(dir) = std::env::var_os("LUXFORGE_GENERATIVE_MODELS") {
            return dir.into();
        }
        main_checkout().join("private/models/generative")
    }

    fn main_checkout() -> PathBuf {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("git");
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The removal conditioning: `LUXFORGE_GENERATIVE_CONDITIONING`, or the release asset's name
    /// in the models directory.
    fn conditioning() -> PathBuf {
        std::env::var_os("LUXFORGE_GENERATIVE_CONDITIONING")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                models().join("klein-4b-removal-conditioning-ba26d15a.lfsdcond")
            })
    }

    /// The generator resource laid out as an install lays it out, its files linked from the
    /// models directory.
    fn installed(name: &str) -> (PathBuf, ModelFile) {
        let dir = temp(name).join(GENERATOR).join("1");
        fs::create_dir_all(&dir).unwrap();
        let models = models();
        for (from, to) in [
            (models.join("flux-2-klein-4b-Q4_0.gguf"), dir.join(GENERATOR)),
            (models.join("flux2-klein-4b-vae.safetensors"), dir.join(VAE)),
            (
                models.join("klein-object-remove-lora-comfy.safetensors"),
                dir.join(REMOVAL_LORA),
            ),
            (conditioning(), dir.join(REMOVAL_CONDITIONING)),
        ] {
            assert!(from.is_file(), "missing {}", from.display());
            std::os::unix::fs::symlink(&from, &to).unwrap();
        }
        let file = ModelFile::new(
            ModelIdentity {
                id: GENERATOR.into(),
                version: "test".into(),
                sha256: "d1023499ef3f2f82ff7c50e6778495195c1b6cc34835741778868428111f9ff4"
                    .into(),
            },
            dir.join(GENERATOR),
        );
        (dir, file)
    }

    fn open(model: &ModelFile) -> KleinGenerator {
        let backend = offered_backends().remove(0);
        KleinGenerator::open(
            model,
            Placement {
                device: luxforge_core::inference::Device::Gpu,
                backend,
                precision: Precision::Mixed,
                partitions: None,
            },
            DiffusionRuntime::new().identity(),
            &Cancel::new(),
        )
        .unwrap()
    }

    fn rgb(path: &Path) -> (u32, Vec<u8>) {
        let image = image::open(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            .to_rgb8();
        (image.width(), image.into_raw())
    }

    /// The harness candidate to reproduce: `LUXFORGE_GENERATIVE_HARNESS` names its path without
    /// `-model.png` (`…/klein-q4-lora-1024-s0`), whose `-input.png` and `-input-hole.png` the
    /// harness wrote with `--dump-input`.
    fn harness() -> PathBuf {
        std::env::var_os("LUXFORGE_GENERATIVE_HARNESS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                main_checkout().join(
                    "private/object-removal/evidence/task-013/harness/sky-clouds-blue/klein-q4-lora-1024-s0",
                )
            })
    }

    fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut name = path.file_name().unwrap().to_os_string();
        name.push(suffix);
        path.with_file_name(name)
    }

    /// The same crop, hole and seed give the qualification harness's candidate bytes, with each
    /// step reported once and the provenance naming the runtime, the backend, the seed and steps.
    #[test]
    #[ignore = "needs the pinned klein files and the harness's dumped input"]
    fn a_removal_reproduces_the_harness_candidate() {
        let prefix = harness();
        let seed: u64 = prefix
            .to_string_lossy()
            .rsplit_once("-s")
            .and_then(|(_, seed)| seed.parse().ok())
            .expect("the harness name ends in -sSEED");
        let (side, input) = rgb(&with_suffix(&prefix, "-input.png"));
        let (_, hole) = rgb(&with_suffix(&prefix, "-input-hole.png"));
        let (_, expected) = rgb(&with_suffix(&prefix, "-model.png"));
        let image: Vec<f32> = input.iter().map(|&v| f32::from(v) / 255.0).collect();
        let mask: Vec<f32> = hole.chunks(3).map(|p| if p[0] != 0 { 1.0 } else { 0.0 }).collect();
        let (dir, model) = installed("reproduce");
        let opened = Instant::now();
        let generator = open(&model);
        let mut steps = Vec::new();
        let started = Instant::now();
        let generated = generator
            .generate(
                &GenerateRequest {
                    width: side,
                    height: side,
                    image: &image,
                    mask: &mask,
                    conditioning: Conditioning::None,
                    seed,
                    steps: 4,
                },
                &mut |progress: StepProgress| steps.push((progress.step, progress.steps)),
                &Cancel::new(),
            )
            .unwrap();
        eprintln!(
            "open {:.1} s, generation {:.1} s at {side} px",
            (started - opened).as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
        assert_eq!(steps, [(1, 4), (2, 4), (3, 4), (4, 4)]);
        let bytes: Vec<u8> = generated
            .image
            .iter()
            .map(|v| (v * 255.0).round() as u8)
            .collect();
        let differing = bytes.iter().zip(&expected).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "{differing} of {} bytes differ", bytes.len());
        let provenance = &generated.provenance;
        assert_eq!(provenance.runtime.name, RUNTIME_NAME);
        assert_eq!(provenance.backend, offered_backends()[0]);
        assert_eq!(provenance.model.id, GENERATOR);
        assert_eq!((provenance.seed, provenance.steps), (seed, 4));
        assert!(provenance.prompt_hash.is_none());
        assert!(!provenance.sampler.is_empty());
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    /// A cancel during a step answers cancelled at the next step boundary, with no candidate.
    #[test]
    #[ignore = "needs the pinned klein files"]
    fn a_cancel_answers_at_the_next_step_and_publishes_nothing() {
        let (dir, model) = installed("cancel");
        let generator = Arc::new(open(&model));
        let side = 512u32;
        let pixels = (side * side) as usize;
        let image = vec![0.5f32; pixels * 3];
        let mut mask = vec![0f32; pixels];
        mask[pixels / 2..pixels / 2 + 64].fill(1.0);
        let cancel = Cancel::new();
        let first = Arc::new(Mutex::new(None::<Instant>));
        let seen = first.clone();
        let canceller = cancel.clone();
        let started = Instant::now();
        let answer = generator.generate(
            &GenerateRequest {
                width: side,
                height: side,
                image: &image,
                mask: &mask,
                conditioning: Conditioning::None,
                seed: 3,
                steps: 4,
            },
            &mut |_: StepProgress| {
                seen.lock().unwrap().get_or_insert_with(Instant::now);
                canceller.cancel();
            },
            &cancel,
        );
        let error = answer.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{error:?}");
        let cancelled_at = first.lock().unwrap().expect("a step was reported");
        let after = cancelled_at.elapsed();
        eprintln!(
            "cancelled {:.1} s after the first step, {:.1} s in",
            after.as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
        // The next step boundary comes within one step of the cancel.
        let step = cancelled_at - started;
        assert!(after < step + Duration::from_secs(5), "{after:?} against a step of {step:?}");
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }
}
