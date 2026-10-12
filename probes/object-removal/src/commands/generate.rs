//! `removal-probe generate`: fill ground-truth cases (or one image and mask) with a generative
//! candidate or the LaMa baseline, candidates in sequence from consecutive seeds, scoring each and
//! recording load, per-candidate time and the host's load.
//!
//! Every candidate runs inside the Fast fill's pipeline (`fill::remove_with`): the crop with
//! context, the model at its native size, the upscale, grain, blend and the composite that copies
//! every pixel with coverage 0 from the input. The command fails if any such pixel changed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::args::Args;
use crate::commands::fill::write_json;
use crate::corpus::{Dirs, Manifest};
use crate::fill::{self, Inpaint, Params};
use crate::generative::{self, Candidate, Conditioning, Klein, Moebius};
use crate::image::Rgb8;
use crate::mask::{Mask, RectPx};
use crate::models;
use crate::runtime::{self, Backend};
use crate::score;

pub const USAGE: &str = "  generate --candidate lama|moebius|klein-q4|klein-q8|klein-q4-lora|klein-q8-lora
           (--case ID[,ID...]|all | --image IMG --mask MASK.png) --out DIR [--size N] [--seeds N] [--seed S]
           [--prompt TEXT] [--encoder q4|q8 | --condition FILE] [--record-condition FILE] [--steps N]
           [--lora-strength F] [--reference none|outlined|masked] [--reference-size N] [--tag T] [--backend B] [--threads N] [--margin F] [--no-blend]
           [--models DIR] [--gen-models DIR] [--corpus DIR] [--public DIR] [--dump-input]
                                           fill each case with N candidates in sequence (seeds S, S+1, ...)
                                           inside the Fast fill's crop, grain, blend and composite; score
                                           each against the clean image and write the crops, the scores,
                                           the load and per-candidate times and the host's load to DIR.
                                           klein needs --encoder (Qwen3-4B, recording with
                                           --record-condition) or --condition (a recorded encoding, no
                                           text encoder loaded); --dump-input writes klein's model
                                           input beside each candidate (NAME-input.png and
                                           NAME-input-hole.png)";

/// The host's load, as `uptime` reports it.
pub fn host_load() -> String {
    std::process::Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("uptime failed: {e}"))
}

/// One input to fill: an image, its region and, for a ground-truth case, the clean image.
pub struct Job {
    pub id: String,
    pub image: Rgb8,
    pub region: Mask,
    pub truth: Option<Rgb8>,
}

/// The square around a region the contact sheets show: the region's bounds with context equal to
/// its longer side, as the fill's own crop, clipped to the image.
pub fn view(region: &Mask) -> Option<RectPx> {
    let bounds = region.bounds()?;
    Some(fill::plan_crop(bounds, region.image, &Params::default()).inside)
}

/// `image` cropped to `view` and scaled so its longer side is at most `long`.
pub fn thumbnail(image: &Rgb8, view: RectPx, long: u32) -> Rgb8 {
    let crop = image.crop(view);
    let scale = long as f64 / crop.width.max(crop.height) as f64;
    if scale >= 1.0 {
        return crop;
    }
    let w = ((crop.width as f64 * scale).round() as u32).max(1);
    let h = ((crop.height as f64 * scale).round() as u32).max(1);
    crop.resized(w, h)
}

/// The cases a `--case` value names: a comma-separated list, or `all`.
pub fn case_ids(value: &str, manifest: &Manifest) -> Vec<String> {
    if value == "all" {
        manifest
            .ground_truth
            .cases
            .iter()
            .map(|c| c.id.clone())
            .collect()
    } else {
        value.split(',').map(|s| s.trim().to_string()).collect()
    }
}

enum Model {
    Lama(Box<dyn runtime::Session>, bool),
    Moebius(Moebius),
    Klein(Box<Klein>),
}

impl Model {
    fn set_dump(&mut self, path: Option<PathBuf>) {
        if let Model::Klein(k) = self {
            k.dump = path;
        }
    }

    fn set_seed(&mut self, seed: u64) {
        match self {
            Model::Lama(..) => {}
            Model::Moebius(m) => m.seed = seed,
            Model::Klein(k) => k.seed = seed,
        }
    }

    fn step_ms(&self) -> Vec<f64> {
        match self {
            Model::Lama(..) => Vec::new(),
            Model::Moebius(m) => m.step_ms.clone(),
            Model::Klein(k) => k.step_ms.clone(),
        }
    }

    fn fill(
        &mut self,
        image: &Rgb8,
        region: &Mask,
        params: &Params,
    ) -> Result<(Rgb8, fill::Report, fill::Details), String> {
        match self {
            Model::Lama(session, bgr) => {
                let mut lama = fill::Lama {
                    session: session.as_mut(),
                    bgr: *bgr,
                };
                fill::remove_with(image, region, &mut lama, params)
            }
            Model::Moebius(m) => fill::remove_with(image, region, m as &mut dyn Inpaint, params),
            Model::Klein(k) => {
                fill::remove_with(image, region, k.as_mut() as &mut dyn Inpaint, params)
            }
        }
    }
}

pub fn run(mut a: Args) -> Result<(), String> {
    let started = Instant::now();
    let candidate_name = a.required("--candidate")?;
    let case = a.opt("--case")?;
    let image_path = a.path("--image")?;
    let mask_path = a.path("--mask")?;
    let out = a.path("--out")?.ok_or("--out is required")?;
    let size: Option<u32> = a.parsed("--size")?;
    let seeds: u64 = a.parsed("--seeds")?.unwrap_or(1);
    let first_seed: u64 = a.parsed("--seed")?.unwrap_or(0);
    let prompt = a.opt("--prompt")?;
    let encoder = a.opt("--encoder")?;
    let condition = a.path("--condition")?;
    let record = a.path("--record-condition")?;
    let steps: Option<i32> = a.parsed("--steps")?;
    let lora_strength: f32 = a.parsed("--lora-strength")?.unwrap_or(1.0);
    let reference: Option<generative::Reference> = a.parsed("--reference")?;
    let tag = a.opt("--tag")?;
    let reference_size: Option<usize> = a.parsed("--reference-size")?;
    let backend: Option<Backend> = a.parsed("--backend")?;
    let threads: i32 = a.parsed("--threads")?.unwrap_or(0);
    let margin: Option<f32> = a.parsed("--margin")?;
    let no_blend = a.flag("--no-blend");
    let dump_input = a.flag("--dump-input");
    let models_dir = models::directory(a.path("--models")?);
    let gen_dir = models::generative_directory(a.path("--gen-models")?);
    let dirs = Dirs::new(a.path("--corpus")?, a.path("--public")?);
    a.done()?;
    if seeds == 0 {
        return Err("--seeds must be at least 1".into());
    }
    let load_before = host_load();

    // The inputs, decoded before any model loads so the load is timed alone.
    let manifest = Manifest::embedded();
    let mut jobs = Vec::new();
    match (case, image_path, mask_path) {
        (Some(value), None, None) => {
            for id in case_ids(&value, &manifest) {
                let case = manifest.case(&id)?.clone();
                let clean = Rgb8::load(&dirs.resolve(&case.image)?)?;
                let pasted = case.paste(&clean);
                jobs.push(Job {
                    id,
                    image: pasted.image,
                    region: pasted.region,
                    truth: Some(clean),
                });
            }
        }
        (None, Some(image), Some(mask)) => jobs.push(Job {
            id: image
                .file_stem()
                .map_or("image".into(), |s| s.to_string_lossy().into_owned()),
            image: Rgb8::load(&image)?,
            region: Mask::load_png(&mask)?,
            truth: None,
        }),
        _ => return Err("give --case, or --image with --mask".into()),
    }

    let lama = candidate_name == "lama";
    let candidate: Option<Candidate> = if lama {
        None
    } else {
        Some(candidate_name.parse()?)
    };
    let size = size.unwrap_or(512);
    if let Some(c) = candidate
        && !c.sizes().contains(&size)
    {
        return Err(format!(
            "{} takes sizes {:?}, not {size}",
            c.name(),
            c.sizes()
        ));
    }
    if lama && size != 512 {
        return Err("the pinned LaMa takes 512 only".into());
    }
    let mut params = Params {
        model_size: size,
        ..Params::default()
    };
    if let Some(m) = margin {
        params.margin = m;
    }
    // The pyramid blend's seam correction carries the difference between the image and the
    // regenerated content just outside the region into it; --no-blend composites over the
    // region's own feathered edge instead.
    params.pyramid_blend = !no_blend;

    // Load.
    let mut files: Vec<&models::Model> = Vec::new();
    let mut conditioning_json = Value::Null;
    let prompt_text;
    let (mut model, load) = match candidate {
        None => {
            let path = models::LAMA.verified(&models_dir)?;
            files.push(&models::LAMA);
            prompt_text = String::new();
            let (session, load) = runtime::open(backend.unwrap_or(Backend::OrtCpu), &path)?;
            (Model::Lama(session, false), load)
        }
        Some(Candidate::Moebius) => {
            files = Candidate::Moebius.files();
            prompt_text = String::new();
            let (m, load) = Moebius::open(&gen_dir, backend.unwrap_or(Backend::OrtCpu))?;
            (Model::Moebius(m), load)
        }
        Some(c) => {
            files = c.files();
            let lora = matches!(c, Candidate::KleinQ4Lora | Candidate::KleinQ8Lora);
            prompt_text = prompt.unwrap_or_else(|| {
                if lora {
                    generative::LORA_PROMPT.to_string()
                } else {
                    generative::REMOVAL_PROMPT.to_string()
                }
            });
            let transformer = if matches!(c, Candidate::KleinQ4 | Candidate::KleinQ4Lora) {
                models::KLEIN_Q4.verified(&gen_dir)?
            } else {
                models::KLEIN_Q8.verified(&gen_dir)?
            };
            let vae = models::KLEIN_VAE.verified(&gen_dir)?;
            let conditioning = match (encoder.as_deref(), &condition) {
                (Some(q), None) => {
                    let llm = match q {
                        "q4" => &models::QWEN3_Q4,
                        "q8" => &models::QWEN3_Q8,
                        other => return Err(format!("--encoder {other:?}: expected q4 or q8")),
                    };
                    files.push(llm);
                    files.push(&models::QWEN3_TOKENIZER);
                    conditioning_json = json!({"text_encoder": llm.file, "prompt": prompt_text});
                    Conditioning::Encoder {
                        llm: llm.verified(&gen_dir)?,
                        tokenizer: models::QWEN3_TOKENIZER.verified(&gen_dir)?,
                    }
                }
                (None, Some(path)) => {
                    conditioning_json = json!({
                        "supplied": path.display().to_string(),
                        "bytes": std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                        "sha256": models::sha256(path)?,
                        "prompt": prompt_text,
                    });
                    Conditioning::Supplied(path.clone())
                }
                _ => return Err("klein needs exactly one of --encoder and --condition".into()),
            };
            let (mut k, load) =
                Klein::open(&transformer, &vae, &conditioning, &prompt_text, threads)?;
            if let Some(s) = steps {
                k.steps = s;
            }
            if lora {
                k.lora = Some((models::KLEIN_REMOVE_LORA.verified(&gen_dir)?, lora_strength));
                k.reference = generative::Reference::Outlined;
            }
            if let Some(r) = reference {
                k.reference = r;
            }
            k.reference_size = reference_size;
            (Model::Klein(Box::new(k)), load)
        }
    };
    let load_ms = load.as_secs_f64() * 1e3;
    let label = match &tag {
        Some(t) => format!("{candidate_name}-{t}-{size}"),
        None => format!("{candidate_name}-{size}"),
    };
    println!("{label}: loaded in {load_ms:.0} ms");

    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut cases = Vec::new();
    let mut first_fill_ms = None;
    for job in &jobs {
        let dir = out.join(&job.id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let view = view(&job.region).ok_or("the region is empty")?;
        let input_tile = dir.join("input.png");
        if !input_tile.exists() {
            thumbnail(&job.image, view, 768).save_png(&input_tile)?;
            if let Some(t) = &job.truth {
                thumbnail(t, view, 768).save_png(&dir.join("truth.png"))?;
            }
        }
        let mut runs = Vec::new();
        let case_started = Instant::now();
        for seed in first_seed..first_seed + seeds {
            model.set_seed(seed);
            params.seed = seed;
            model.set_dump(
                dump_input.then(|| dir.join(format!("{label}-s{seed}-input.png"))),
            );
            let t = Instant::now();
            let (result, report, details) = model.fill(&job.image, &job.region, &params)?;
            let total_ms = t.elapsed().as_secs_f64() * 1e3;
            first_fill_ms.get_or_insert(total_ms);
            let s = score::score(&job.image, &result, &job.region, job.truth.as_ref(), 8)?;
            if !s.outside_unchanged() {
                return Err(format!(
                    "{} seed {seed}: {} pixels outside the region changed",
                    job.id, s.changed_outside
                ));
            }
            let name = format!("{label}-s{seed}");
            thumbnail(&result, view, 768).save_png(&dir.join(format!("{name}.png")))?;
            details
                .model_output
                .save_png(&dir.join(format!("{name}-model.png")))?;
            let model_ms = report
                .timings_ms
                .iter()
                .find(|(n, _)| n == "model")
                .map_or(0.0, |(_, ms)| *ms);
            let line = match (&s.region, &s.ring) {
                (Some(r), Some(g)) => format!(
                    "region {:.2} dB / {:.3}, ring {:.2} dB / {:.3}",
                    r.psnr_db, r.ssim, g.psnr_db, g.ssim
                ),
                _ => "no truth".into(),
            };
            println!(
                "{} {name}: model {model_ms:.0} ms, fill {total_ms:.0} ms, {line}",
                job.id
            );
            if let (Some(path), Model::Klein(k)) = (&record, &model)
                && seed == first_seed
                && job.id == jobs[0].id
            {
                luxforge_removal_diffusion::save_condition(&k.prompt, path)?;
                println!(
                    "recorded the conditioning for {:?} to {}",
                    k.prompt,
                    path.display()
                );
            }
            runs.push(json!({
                "seed": seed,
                "model_ms": model_ms,
                "fill_ms": total_ms,
                "step_ms": model.step_ms(),
                "stages_ms": report.timings_ms.iter().map(|(n, ms)| json!([n, ms])).collect::<Vec<_>>(),
                "crop": [report.crop.x, report.crop.y, report.crop.width, report.crop.height],
                "enlargement": report.scale,
                "score": s.to_json(),
            }));
        }
        cases.push(json!({
            "case": job.id,
            "candidates_in_sequence_ms": case_started.elapsed().as_secs_f64() * 1e3,
            "runs": runs,
        }));
    }
    let report = json!({
        "candidate": candidate_name,
        "tag": tag,
        "size": size,
        "stages": {
            "crop": true,
            "grain": params.grain,
            "pyramid_blend": params.pyramid_blend,
            "composite": true,
        },
        "klein": match &model {
            Model::Klein(k) => json!({
                "steps": k.steps,
                "reference": k.reference.name(),
                "reference_size": k.reference_size,
                "lora": k.lora.as_ref().map(|(p, m)| json!({"file": p.file_name().map(|f| f.to_string_lossy().into_owned()), "multiplier": m})),
            }),
            _ => Value::Null,
        },
        "seeds": [first_seed, first_seed + seeds - 1],
        "prompt": prompt_text,
        "conditioning": conditioning_json,
        "backend": match &model {
            Model::Lama(s, _) => s.backend().name().to_string(),
            Model::Moebius(_) => backend.unwrap_or(Backend::OrtCpu).name().to_string(),
            Model::Klein(k) => format!("stable-diffusion.cpp {}", k.model_version()),
        },
        "files": files.iter().map(|m| json!({"file": m.file, "bytes": m.bytes, "sha256": m.sha256})).collect::<Vec<_>>(),
        "download_bytes": generative::download_bytes(&files),
        "load_ms": load_ms,
        "first_fill_ms": first_fill_ms,
        "host_load_before": load_before,
        "host_load_after": host_load(),
        "total_ms": started.elapsed().as_secs_f64() * 1e3,
        "cases": cases,
    });
    let path = out.join(format!("{label}.json"));
    write_json(&path, &report)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Where `generate`'s evidence goes by default: the probe's `target/evidence/task-005/`.
pub fn evidence_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/evidence/task-005")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::tests::mask_from;

    #[test]
    fn the_view_holds_the_region_with_context_and_stays_inside_the_image() {
        let size = (400, 300);
        let region = mask_from(size, |x, y| {
            if (100..140).contains(&x) && (100..120).contains(&y) {
                255
            } else {
                0
            }
        });
        let v = view(&region).unwrap();
        assert!(v.x <= 100 && v.y <= 100);
        assert!(v.x + v.width >= 140 && v.y + v.height >= 120);
        assert!(v.x + v.width <= 400 && v.y + v.height <= 300);
        // Context of the longer side (40 px) on every side, at least the fill's 48 px minimum.
        assert!(v.width >= 40 + 2 * 48);
    }

    #[test]
    fn a_thumbnail_is_never_enlarged_and_keeps_the_aspect() {
        let image = Rgb8::new(100, 50, vec![7; 100 * 50 * 3]);
        let whole = RectPx {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        let small = thumbnail(&image, whole, 40);
        assert_eq!((small.width, small.height), (40, 20));
        let same = thumbnail(&image, whole, 400);
        assert_eq!((same.width, same.height), (100, 50));
    }

    #[test]
    fn all_names_every_case_in_the_manifest() {
        let manifest = Manifest::embedded();
        assert_eq!(
            case_ids("all", &manifest).len(),
            manifest.ground_truth.cases.len()
        );
        assert_eq!(case_ids("a, b", &manifest), vec!["a", "b"]);
    }
}
