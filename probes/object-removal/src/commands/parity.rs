//! `removal-probe parity`: a correctness and load/run timing table for every pinned model on
//! every backend, against ONNX Runtime's CPU provider as the reference.
//!
//! This is a correctness check, not a benchmark: each model runs a fixed, deterministic,
//! seeded input once per backend (load, then a first run, then three more "warm" runs whose
//! median is reported). It is not the timing tier (`bench`, `measure`) and draws no performance
//! conclusion; see `docs/design/object-removal.md`'s "What the prototype must show" for the
//! M4 measurements that belong in TASK-006 instead.
//!
//! A model whose test input is itself model output (both decoders take a segmenter's own
//! embeddings) is seeded by running that encoder once, on ort-cpu, regardless of which backends
//! were requested: that is test-input preparation, not a measured backend.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::args::Args;
use crate::models::{self, Model};
use crate::runtime::{self, Backend};
use crate::tensor::{Data, Tensor};

/// An encoder's ort-cpu outputs on its seeded input, keyed by segmenter family, cached so the
/// matching decoder pays for it once.
type EmbeddingCache = HashMap<&'static str, Result<Vec<(String, Tensor)>, String>>;

pub const USAGE: &str =
    "  parity [--models DIR] [--backends B,B,...] [--model ID] [--out FILE.json]
                                            load/run time and correctness of every pinned model
                                            on every backend, against ort-cpu (default backends:
                                            ort-cpu,ort-coreml-all,ort-coreml-gpu,ort-coreml-ane,
                                            tract-cpu,tract-metal)";

pub fn run(mut a: Args) -> Result<(), String> {
    let models_dir = models::directory(a.path("--models")?);
    let backend_list = a.opt("--backends")?;
    let model_filter = a.opt("--model")?;
    let out = a.path("--out")?;
    a.done()?;

    let backends: Vec<Backend> = match &backend_list {
        Some(s) => s
            .split(',')
            .map(|n| Backend::from_str(n.trim()))
            .collect::<Result<_, _>>()?,
        None => Backend::ALL.to_vec(),
    };
    if let Some(id) = &model_filter
        && !models::ALL.iter().any(|m| m.id == id)
    {
        return Err(format!(
            "unknown model {id:?}; expected one of {}",
            models::ALL
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let wanted = |id: &str| model_filter.as_deref().is_none_or(|f| f == id);

    // Encoder outputs the decoders prompt against: computed once each, on ort-cpu, whichever
    // backends were requested. Built lazily so a `--model` filter on the encoder alone doesn't
    // also require the other family's encoder to load successfully.
    let mut embeddings: EmbeddingCache = HashMap::new();

    let mut rows: Vec<Row> = Vec::new();
    for &m in models::ALL {
        if !wanted(m.id) {
            continue;
        }
        let path = match m.verified(&models_dir) {
            Ok(p) => p,
            Err(e) => {
                for &b in &backends {
                    rows.push(Row::error(m.id, b, format!("model file: {e}")));
                }
                continue;
            }
        };
        let inputs = match fixed_inputs(m, &models_dir, &mut embeddings) {
            Ok(i) => i,
            Err(e) => {
                for &b in &backends {
                    rows.push(Row::error(m.id, b, format!("building test input: {e}")));
                }
                continue;
            }
        };

        // The reference run, always on ort-cpu, whether or not it was requested: every other
        // backend's row is diffed against it.
        let baseline = run_once(Backend::OrtCpu, &path, &inputs);

        for &b in &backends {
            let row = if b == Backend::OrtCpu {
                match &baseline {
                    Ok(t) => Row::ok(m.id, b, t, t),
                    Err(e) => Row::error(m.id, b, e.clone()),
                }
            } else {
                match (&baseline, run_once(b, &path, &inputs)) {
                    (Ok(base), Ok(t)) => Row::ok(m.id, b, &t, base),
                    (Err(be), _) => {
                        Row::error(m.id, b, format!("no ort-cpu baseline to compare: {be}"))
                    }
                    (_, Err(e)) => Row::error(m.id, b, e),
                }
            };
            rows.push(row);
        }
    }

    print_table(&rows);
    if let Some(path) = out {
        let json = serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("\nwrote {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Fixed, deterministic, seeded test inputs.
// ---------------------------------------------------------------------------------------------

/// A tiny seeded PRNG (SplitMix64), so every run of `parity` sees byte-identical inputs without
/// pulling in a `rand` dependency for a handful of deterministic draws.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A float uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A seeded pseudo-random image in the ImageNet-normalized range `[-2.5, 2.5]`: roughly the span
/// of `(x - mean) / std` for `x` in `[0, 1]` and the usual ImageNet mean/std, wide enough to
/// exercise both encoders without favoring either's exact normalization constants.
fn seeded_normalized_image(seed: u64, shape: [usize; 4]) -> Tensor {
    let mut rng = Rng::new(seed);
    let n: usize = shape.iter().product();
    let data = (0..n).map(|_| rng.unit() * 5.0 - 2.5).collect();
    Tensor::f32(shape, data)
}

/// LaMa's test input: a smooth per-channel gradient in `[0, 1]` (so the mean/max diffs below are
/// not dominated by high-frequency noise a resize would blur differently per backend) plus
/// seeded low-amplitude noise, and a centred square hole. `mask` is 1 inside the hole, 0 outside,
/// matching the "fill this in" convention `fill::remove` (TASK-003) composites against.
fn lama_inputs(seed: u64, size: usize) -> (Tensor, Tensor) {
    let mut rng = Rng::new(seed);
    let mut image = vec![0f32; 3 * size * size];
    for c in 0..3 {
        for y in 0..size {
            for x in 0..size {
                let gradient = 0.5 * (x as f32 / (size - 1) as f32)
                    + 0.5 * (y as f32 / (size - 1) as f32)
                    + 0.1 * c as f32;
                let noise = (rng.unit() - 0.5) * 0.1;
                image[c * size * size + y * size + x] = (gradient + noise).clamp(0.0, 1.0);
            }
        }
    }
    let image = Tensor::f32([1, 3, size, size], image);

    let hole = size / 4;
    let lo = (size - hole) / 2;
    let hi = lo + hole;
    let mut mask = vec![0f32; size * size];
    for y in lo..hi {
        for x in lo..hi {
            mask[y * size + x] = 1.0;
        }
    }
    let mask = Tensor::f32([1, 1, size, size], mask);
    (image, mask)
}

/// Runs `model`'s encoder on ort-cpu with a seeded image, caching the result under `key` so the
/// matching decoder (and only it) pays for it once.
fn encoder_embeddings<'a>(
    key: &'static str,
    model: &Model,
    models_dir: &Path,
    seed: u64,
    input_name: &str,
    shape: [usize; 4],
    cache: &'a mut EmbeddingCache,
) -> &'a Result<Vec<(String, Tensor)>, String> {
    cache.entry(key).or_insert_with(|| {
        let path = model.verified(models_dir)?;
        let (mut session, _) = runtime::open(Backend::OrtCpu, &path)?;
        let input = seeded_normalized_image(seed, shape);
        session.run(&[(input_name, &input)])
    })
}

fn fixed_inputs(
    model: &Model,
    models_dir: &Path,
    embeddings: &mut EmbeddingCache,
) -> Result<Vec<(String, Tensor)>, String> {
    // A centre point in each family's own prompt frame: EfficientViT-SAM L0 prompts in the
    // long-side-1024 frame the encoder pads to from 512; SAM 2.1 Small's encoder resizes to
    // exactly 1024x1024. Both frames are 1024 square, so the same centre coordinate serves both.
    let point_coords = Tensor::f32([1, 1, 2], vec![512.0, 512.0]);
    let point_labels = Tensor::f32([1, 1], vec![1.0]);

    Ok(match model.id {
        "evit-l0-encoder" => vec![(
            "input_image".to_string(),
            seeded_normalized_image(1, [1, 3, 512, 512]),
        )],
        "evit-l0-decoder" => {
            let emb = encoder_embeddings(
                "evit-l0",
                &models::EVIT_L0_ENCODER,
                models_dir,
                1,
                "input_image",
                [1, 3, 512, 512],
                embeddings,
            )
            .as_ref()
            .map_err(|e| e.clone())?;
            let image_embeddings = find(emb, "image_embeddings")?;
            vec![
                ("image_embeddings".to_string(), image_embeddings),
                ("point_coords".to_string(), point_coords),
                ("point_labels".to_string(), point_labels),
            ]
        }
        "sam21-small-encoder" => vec![(
            "image".to_string(),
            seeded_normalized_image(2, [1, 3, 1024, 1024]),
        )],
        "sam21-small-decoder" => {
            let emb = encoder_embeddings(
                "sam21-small",
                &models::SAM21_SMALL_ENCODER,
                models_dir,
                2,
                "image",
                [1, 3, 1024, 1024],
                embeddings,
            )
            .as_ref()
            .map_err(|e| e.clone())?;
            let image_embed = find(emb, "image_embed")?;
            let feats0 = find(emb, "high_res_feats_0")?;
            let feats1 = find(emb, "high_res_feats_1")?;
            let mask_input = Tensor::f32([1, 1, 256, 256], vec![0.0; 256 * 256]);
            let has_mask_input = Tensor::f32([1], vec![0.0]);
            vec![
                ("image_embed".to_string(), image_embed),
                ("high_res_feats_0".to_string(), feats0),
                ("high_res_feats_1".to_string(), feats1),
                ("point_coords".to_string(), point_coords),
                ("point_labels".to_string(), point_labels),
                ("mask_input".to_string(), mask_input),
                ("has_mask_input".to_string(), has_mask_input),
            ]
        }
        "lama" => {
            let (image, mask) = lama_inputs(3, 512);
            vec![("image".to_string(), image), ("mask".to_string(), mask)]
        }
        other => return Err(format!("parity has no fixed input for model {other:?}")),
    })
}

fn find(outputs: &[(String, Tensor)], name: &str) -> Result<Tensor, String> {
    outputs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, t)| t.clone())
        .ok_or_else(|| format!("encoder produced no output {name:?}"))
}

// ---------------------------------------------------------------------------------------------
// Running and timing one model on one backend.
// ---------------------------------------------------------------------------------------------

struct Timed {
    load: Duration,
    first_run: Duration,
    /// Median of 3 runs after the first.
    warm_run: Duration,
    outputs: Vec<(String, Tensor)>,
}

fn run_once(backend: Backend, path: &Path, inputs: &[(String, Tensor)]) -> Result<Timed, String> {
    let (mut session, load) = runtime::open(backend, path)?;
    let pairs: Vec<(&str, &Tensor)> = inputs.iter().map(|(n, t)| (n.as_str(), t)).collect();

    let t0 = Instant::now();
    let mut outputs = session.run(&pairs)?;
    let first_run = t0.elapsed();

    let mut warm_ms = [0f64; 3];
    for slot in &mut warm_ms {
        let t = Instant::now();
        outputs = session.run(&pairs)?;
        *slot = t.elapsed().as_secs_f64() * 1e3;
    }
    warm_ms.sort_by(f64::total_cmp);
    let warm_run = Duration::from_secs_f64(warm_ms[1] / 1e3);

    Ok(Timed {
        load,
        first_run,
        warm_run,
        outputs,
    })
}

// ---------------------------------------------------------------------------------------------
// Reporting.
// ---------------------------------------------------------------------------------------------

#[derive(Serialize)]
struct OutputDiff {
    name: String,
    /// `None` when the shapes disagree, or either tensor is not f32 (`runtime::difference`'s
    /// domain); `note` explains why in that case.
    mean_abs_diff: Option<f64>,
    max_abs_diff: Option<f32>,
    /// Only set for an output named `masks`: the IoU of `logit > 0` against ort-cpu, over the
    /// whole tensor (every channel and batch element flattened together).
    mask_iou_vs_ort_cpu: Option<f64>,
    note: Option<String>,
}

#[derive(Serialize)]
struct Row {
    model: String,
    backend: String,
    status: &'static str,
    error: Option<String>,
    load_ms: Option<f64>,
    first_run_ms: Option<f64>,
    /// Median of 3 runs after the first.
    warm_run_ms: Option<f64>,
    outputs: Vec<OutputDiff>,
}

impl Row {
    fn error(model: &str, backend: Backend, error: String) -> Row {
        Row {
            model: model.to_string(),
            backend: backend.name().to_string(),
            status: "error",
            error: Some(error),
            load_ms: None,
            first_run_ms: None,
            warm_run_ms: None,
            outputs: Vec::new(),
        }
    }

    fn ok(model: &str, backend: Backend, t: &Timed, baseline: &Timed) -> Row {
        let outputs = t
            .outputs
            .iter()
            .map(|(name, tensor)| {
                let base = baseline.outputs.iter().find(|(n, _)| n == name);
                match base {
                    None => OutputDiff {
                        name: name.clone(),
                        mean_abs_diff: None,
                        max_abs_diff: None,
                        mask_iou_vs_ort_cpu: None,
                        note: Some("ort-cpu produced no matching output".to_string()),
                    },
                    Some((_, base_tensor)) => {
                        let (mean, max, note) = match runtime::difference(tensor, base_tensor) {
                            Ok((mean, max)) => (Some(mean), Some(max), None),
                            Err(e) => (None, None, Some(e)),
                        };
                        let iou = (name == "masks")
                            .then(|| mask_iou(tensor, base_tensor))
                            .flatten();
                        OutputDiff {
                            name: name.clone(),
                            mean_abs_diff: mean,
                            max_abs_diff: max,
                            mask_iou_vs_ort_cpu: iou,
                            note,
                        }
                    }
                }
            })
            .collect();
        Row {
            model: model.to_string(),
            backend: backend.name().to_string(),
            status: "ok",
            error: None,
            load_ms: Some(t.load.as_secs_f64() * 1e3),
            first_run_ms: Some(t.first_run.as_secs_f64() * 1e3),
            warm_run_ms: Some(t.warm_run.as_secs_f64() * 1e3),
            outputs,
        }
    }
}

/// IoU of `logit > 0` between two same-shaped f32 tensors, over every element flattened
/// together. `None` if the shapes disagree or either tensor is not f32; `Some(1.0)` if neither
/// tensor has any positive logit (both predict an empty mask).
fn mask_iou(a: &Tensor, b: &Tensor) -> Option<f64> {
    if a.shape != b.shape {
        return None;
    }
    let (Data::F32(x), Data::F32(y)) = (&a.data, &b.data) else {
        return None;
    };
    let (mut intersection, mut union) = (0u64, 0u64);
    for (p, q) in x.iter().zip(y) {
        let (pi, qi) = (*p > 0.0, *q > 0.0);
        if pi || qi {
            union += 1;
        }
        if pi && qi {
            intersection += 1;
        }
    }
    Some(if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    })
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<22} {:<16} {:<6} {:>10} {:>10} {:>10}",
        "model", "backend", "status", "load_ms", "first_ms", "warm_ms"
    );
    for r in rows {
        let fmt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.1}"));
        println!(
            "{:<22} {:<16} {:<6} {:>10} {:>10} {:>10}",
            r.model,
            r.backend,
            r.status,
            fmt(r.load_ms),
            fmt(r.first_run_ms),
            fmt(r.warm_run_ms),
        );
        if let Some(e) = &r.error {
            println!("    error: {e}");
        }
        for o in &r.outputs {
            let mean = o
                .mean_abs_diff
                .map_or("-".to_string(), |v| format!("{v:.6}"));
            let max = o
                .max_abs_diff
                .map_or("-".to_string(), |v| format!("{v:.6}"));
            let mut line = format!("    {:<24} mean={mean} max={max}", o.name);
            if let Some(iou) = o.mask_iou_vs_ort_cpu {
                line += &format!(" iou_vs_ort_cpu={iou:.4}");
            }
            if let Some(note) = &o.note {
                line += &format!(" ({note})");
            }
            println!("{line}");
        }
    }
}
