//! The generative candidates behind [`fill::Inpaint`]: Moebius over ONNX Runtime, and FLUX.2
//! klein 4B over stable-diffusion.cpp (`luxforge-removal-diffusion`). Each fills the square crop
//! `fill::remove_with` plans at its native size, so the crop, upscale, grain, blend and the
//! byte-identical composite outside the coverage are the Fast fill's own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use luxforge_removal_diffusion as sd;

use crate::fill::{self, Inpaint, ModelInput, ModelOutput};
use crate::models;
use crate::runtime::{self, Backend, Session};
use crate::tensor::Tensor;

/// The prompt a removal is conditioned on: the "empty scene" whose encoding is shipped with the
/// model so a removal needs no text encoder.
pub const REMOVAL_PROMPT: &str =
    "an empty scene: only the background, continuing the surrounding surfaces, textures and light";

/// fal's LoRA is trained on this instruction with the object outlined in red.
pub const LORA_PROMPT: &str = "Remove the highlighted object from the scene";

/// A generative candidate the harness can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Candidate {
    /// Moebius `ft_places2`, 512² only.
    Moebius,
    /// klein 4B, Q4_0 transformer: masked denoising with the removal prompt.
    KleinQ4,
    /// klein 4B, Q8_0 transformer: the same.
    KleinQ8,
    /// klein 4B Q4_0 with fal's removal LoRA: the crop with the region outlined in red as the
    /// reference, the LoRA's instruction, and masked denoising.
    KleinQ4Lora,
    /// klein 4B Q8_0 with the LoRA.
    KleinQ8Lora,
}

impl Candidate {
    pub const ALL: &[Candidate] = &[
        Candidate::Moebius,
        Candidate::KleinQ4,
        Candidate::KleinQ8,
        Candidate::KleinQ4Lora,
        Candidate::KleinQ8Lora,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Candidate::Moebius => "moebius",
            Candidate::KleinQ4 => "klein-q4",
            Candidate::KleinQ8 => "klein-q8",
            Candidate::KleinQ4Lora => "klein-q4-lora",
            Candidate::KleinQ8Lora => "klein-q8-lora",
        }
    }

    /// The pinned files a run loads, the text encoder excluded.
    pub fn files(self) -> Vec<&'static models::Model> {
        match self {
            Candidate::Moebius => vec![
                &models::MOEBIUS_UNET,
                &models::MOEBIUS_VAE_ENCODER,
                &models::MOEBIUS_VAE_DECODER,
            ],
            Candidate::KleinQ4 => vec![&models::KLEIN_Q4, &models::KLEIN_VAE],
            Candidate::KleinQ8 => vec![&models::KLEIN_Q8, &models::KLEIN_VAE],
            Candidate::KleinQ4Lora => vec![
                &models::KLEIN_Q4,
                &models::KLEIN_VAE,
                &models::KLEIN_REMOVE_LORA,
            ],
            Candidate::KleinQ8Lora => vec![
                &models::KLEIN_Q8,
                &models::KLEIN_VAE,
                &models::KLEIN_REMOVE_LORA,
            ],
        }
    }

    pub fn is_klein(self) -> bool {
        self != Candidate::Moebius
    }

    /// The model sizes the candidate takes.
    pub fn sizes(self) -> &'static [u32] {
        match self {
            Candidate::Moebius => &[512],
            _ => &[512, 768, 1024],
        }
    }
}

impl std::str::FromStr for Candidate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Candidate::ALL
            .iter()
            .copied()
            .find(|c| c.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Candidate::ALL.iter().map(|c| c.name()).collect();
                format!(
                    "unknown candidate {s:?}; expected lama or one of {}",
                    names.join(", ")
                )
            })
    }
}

/// The sum of a candidate's pinned files, the text encoder excluded.
pub fn download_bytes(files: &[&models::Model]) -> u64 {
    files.iter().map(|m| m.bytes).sum()
}

// --- Moebius ---------------------------------------------------------------------------------

/// The VAE's latent scale (PixelHacker's VAE, not SD's 0.18215).
const MOEBIUS_SCALE: f32 = 0.13025;
/// The DDIM schedule the reference pipeline uses: 20 steps at strength 0.99, which drops the first.
const MOEBIUS_STEPS: usize = 20;
const MOEBIUS_GUIDANCE: f32 = 2.0;
const MOEBIUS_NOISE_OFFSET: f64 = 0.0357;

/// Moebius: a VAE encoder, a 9-channel denoiser conditioned on a learned 20-row embedding (rows
/// 0–9 conditional, 10–19 unconditional), DDIM with classifier-free guidance, and a VAE decoder.
pub struct Moebius {
    encoder: Box<dyn Session>,
    unet: Box<dyn Session>,
    decoder: Box<dyn Session>,
    /// Run the guidance pair as two batch-1 calls, for backends that fix the batch at 1.
    split_batch: bool,
    pub seed: u64,
    /// Milliseconds per denoiser step of the last fill.
    pub step_ms: Vec<f64>,
}

impl Moebius {
    pub fn open(dir: &Path, backend: Backend) -> Result<(Self, Duration), String> {
        let start = Instant::now();
        let encoder = runtime::open(backend, &models::MOEBIUS_VAE_ENCODER.verified(dir)?)?.0;
        let unet = runtime::open(backend, &models::MOEBIUS_UNET.verified(dir)?)?.0;
        let decoder = runtime::open(backend, &models::MOEBIUS_VAE_DECODER.verified(dir)?)?.0;
        Ok((
            Self {
                encoder,
                unet,
                decoder,
                split_batch: matches!(backend, Backend::OrtCoreMlFixed(_)),
                seed: 0,
                step_ms: Vec::new(),
            },
            start.elapsed(),
        ))
    }
}

/// `alphas_cumprod` for the scaled-linear schedule (β from 0.00085 to 0.012 over 1000 steps) and
/// the 19 timesteps the reference runs: 950 … 0 at a stride of 50, the first dropped.
pub fn moebius_schedule() -> (Vec<f64>, Vec<i64>) {
    let n = 1000;
    let (b0, b1) = (0.00085f64.sqrt(), 0.012f64.sqrt());
    let mut cumprod = Vec::with_capacity(n);
    let mut acc = 1.0;
    for i in 0..n {
        let beta = (b0 + (b1 - b0) * i as f64 / (n - 1) as f64).powi(2);
        acc *= 1.0 - beta;
        cumprod.push(acc);
    }
    let ratio = (n / MOEBIUS_STEPS) as i64;
    let mut timesteps: Vec<i64> = (0..MOEBIUS_STEPS as i64).map(|i| i * ratio).rev().collect();
    timesteps.remove(0);
    (cumprod, timesteps)
}

/// One DDIM step with η = 0 and no clipping.
pub fn ddim_step(eps: &[f32], t: i64, prev: i64, sample: &[f32], cumprod: &[f64]) -> Vec<f32> {
    let at = cumprod[t as usize];
    let ap = if prev >= 0 {
        cumprod[prev as usize]
    } else {
        1.0
    };
    let (sa, sb) = (at.sqrt(), (1.0 - at).sqrt());
    let (pa, pb) = (ap.sqrt(), (1.0 - ap).sqrt());
    sample
        .iter()
        .zip(eps)
        .map(|(&x, &e)| {
            let x0 = (x as f64 - sb * e as f64) / sa;
            (pa * x0 + pb * e as f64) as f32
        })
        .collect()
}

impl Inpaint for Moebius {
    fn inpaint(&mut self, input: &ModelInput) -> Result<ModelOutput, String> {
        let n = input.size;
        if n != 512 {
            return Err(format!("Moebius takes 512² only, not {n}²"));
        }
        let l = n / 8;
        // The masked image in [-1, 1]: the hole is 0, mid-grey, as the reference zeroes it.
        let mut masked = Vec::with_capacity(3 * n * n);
        for c in 0..3 {
            masked.extend(
                input.image[c]
                    .iter()
                    .zip(input.hole)
                    .map(|(&v, &h)| if h != 0 { 0.0 } else { v / 255.0 * 2.0 - 1.0 }),
            );
        }
        let mut outputs = self
            .encoder
            .run(&[("image", &Tensor::f32([1, 3, n, n], masked))])?;
        let moments = runtime::take(&mut outputs, "moments")?.into_f32()?;
        if moments.len() != 8 * l * l {
            return Err("the Moebius encoder returned an unexpected shape".into());
        }
        let masked_latent: Vec<f32> = moments[..4 * l * l]
            .iter()
            .map(|v| v * MOEBIUS_SCALE)
            .collect();
        // A latent cell is in the hole when any of its 8 × 8 pixels is.
        let mut mask_latent = vec![0f32; l * l];
        for (i, m) in mask_latent.iter_mut().enumerate() {
            let (cy, cx) = (i / l, i % l);
            let any =
                (0..8).any(|dy| (0..8).any(|dx| input.hole[(cy * 8 + dy) * n + cx * 8 + dx] != 0));
            *m = if any { 1.0 } else { 0.0 };
        }
        // Seeded Gaussian noise with the reference's per-channel offset.
        let mut latents = Vec::with_capacity(4 * l * l);
        for c in 0..4u64 {
            let offset = MOEBIUS_NOISE_OFFSET * fill::gaussian(self.seed, 1000 + c, 0, 0);
            for y in 0..l as i64 {
                for x in 0..l as i64 {
                    latents.push((fill::gaussian(self.seed, c, x, y) + offset) as f32);
                }
            }
        }
        let (cumprod, timesteps) = moebius_schedule();
        let uncond: Vec<i64> = (10..20).collect();
        let cond: Vec<i64> = (0..10).collect();
        self.step_ms.clear();
        for (i, &t) in timesteps.iter().enumerate() {
            let step = Instant::now();
            let prev = timesteps.get(i + 1).copied().unwrap_or(-1);
            let mut nine = Vec::with_capacity(9 * l * l);
            nine.extend_from_slice(&latents);
            nine.extend_from_slice(&mask_latent);
            nine.extend_from_slice(&masked_latent);
            let (eps_u, eps_c) = if self.split_batch {
                let x = Tensor::f32([1, 9, l, l], nine);
                let ts = Tensor::i64([1], vec![t]);
                let mut run = |ids: &[i64]| -> Result<Vec<f32>, String> {
                    let ids = Tensor::i64([1, 10], ids.to_vec());
                    let mut o = self.unet.run(&[
                        ("latent", &x),
                        ("timesteps", &ts),
                        ("input_ids", &ids),
                    ])?;
                    runtime::take(&mut o, "noise")?.into_f32()
                };
                (run(&uncond)?, run(&cond)?)
            } else {
                let mut pair = nine.clone();
                pair.extend_from_slice(&nine);
                let x = Tensor::f32([2, 9, l, l], pair);
                let ts = Tensor::i64([2], vec![t, t]);
                let mut ids = uncond.clone();
                ids.extend_from_slice(&cond);
                let ids = Tensor::i64([2, 10], ids);
                let mut o =
                    self.unet
                        .run(&[("latent", &x), ("timesteps", &ts), ("input_ids", &ids)])?;
                let eps = runtime::take(&mut o, "noise")?.into_f32()?;
                let half = 4 * l * l;
                (eps[..half].to_vec(), eps[half..].to_vec())
            };
            let eps: Vec<f32> = eps_u
                .iter()
                .zip(&eps_c)
                .map(|(u, c)| u + MOEBIUS_GUIDANCE * (c - u))
                .collect();
            latents = ddim_step(&eps, t, prev, &latents, &cumprod);
            self.step_ms.push(step.elapsed().as_secs_f64() * 1e3);
        }
        let scaled: Vec<f32> = latents.iter().map(|v| v / MOEBIUS_SCALE).collect();
        let mut o = self
            .decoder
            .run(&[("latent", &Tensor::f32([1, 4, l, l], scaled))])?;
        let image = runtime::take(&mut o, "image")?.into_f32()?;
        if image.len() != 3 * n * n {
            return Err("the Moebius decoder returned an unexpected shape".into());
        }
        let mut out: ModelOutput = Default::default();
        for (c, plane) in out.iter_mut().enumerate() {
            *plane = image[c * n * n..(c + 1) * n * n]
                .iter()
                .map(|v| (v + 1.0) / 2.0 * 255.0)
                .collect();
        }
        Ok(out)
    }
}

// --- klein over stable-diffusion.cpp ---------------------------------------------------------

/// How a klein run is conditioned.
#[derive(Clone, Debug)]
pub enum Conditioning {
    /// The text encoder encodes the prompt.
    Encoder { llm: PathBuf, tokenizer: PathBuf },
    /// A conditioning recorded earlier is supplied for the prompt; no text encoder is loaded.
    Supplied(PathBuf),
}

/// The reference image a klein run is given (klein edits its references in context).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// No reference: the region is filled only from the known latents around it.
    None,
    /// The crop with the region outlined in red, the "highlighted object" fal's LoRA was trained on.
    Outlined,
    /// The crop with the region mid-grey, so the model sees the whole context but not the object.
    Masked,
}

impl std::str::FromStr for Reference {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "outlined" => Ok(Self::Outlined),
            "masked" => Ok(Self::Masked),
            other => Err(format!(
                "--reference {other:?}: expected none, outlined or masked"
            )),
        }
    }
}

impl Reference {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Outlined => "outlined",
            Self::Masked => "masked",
        }
    }
}

/// FLUX.2 klein 4B: four flow steps over the crop with stable-diffusion.cpp's masked denoising
/// (the known latents are restored after every step), optionally with fal's removal LoRA and a
/// reference image.
pub struct Klein {
    ctx: sd::Context,
    pub prompt: String,
    pub lora: Option<(PathBuf, f32)>,
    pub reference: Reference,
    /// The reference's side in pixels (a multiple of 16), when smaller than the crop's: fewer
    /// reference tokens, so each step is cheaper.
    pub reference_size: Option<usize>,
    pub steps: i32,
    pub seed: u64,
    pub step_ms: Vec<f64>,
}

impl Klein {
    pub fn open(
        transformer: &Path,
        vae: &Path,
        conditioning: &Conditioning,
        prompt: &str,
        threads: i32,
    ) -> Result<(Self, Duration), String> {
        // `LUXFORGE_SD_LOG` sets stable-diffusion.cpp's log level (0 debug … 4 error; default 3).
        if let Some(level) = std::env::var("LUXFORGE_SD_LOG")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            sd::set_log_level(level);
        }
        let start = Instant::now();
        let (llm, tokenizer) = match conditioning {
            Conditioning::Encoder { llm, tokenizer } => {
                (Some(llm.as_path()), Some(tokenizer.as_path()))
            }
            Conditioning::Supplied(_) => (None, None),
        };
        let mut ctx = sd::Context::open(&sd::OpenOptions {
            diffusion_model: transformer,
            vae,
            llm,
            tokenizer,
            threads,
            flash_attn: true,
        })?;
        if let Conditioning::Supplied(path) = conditioning {
            ctx.supply_condition(prompt, Some(path))?;
        }
        Ok((
            Self {
                ctx,
                prompt: prompt.to_string(),
                lora: None,
                reference: Reference::None,
                reference_size: None,
                steps: 4,
                seed: 0,
                step_ms: Vec::new(),
            },
            start.elapsed(),
        ))
    }

    pub fn model_version(&self) -> String {
        self.ctx.model_version()
    }
}

/// The crop as RGB bytes, with the hole replaced by mid-grey (stable-diffusion.cpp's own
/// masked-image convention), so nothing of the object reaches the known latents.
pub fn init_rgb(input: &ModelInput, grey_hole: bool) -> Vec<u8> {
    let n = input.size;
    let mut rgb = Vec::with_capacity(n * n * 3);
    for i in 0..n * n {
        for c in 0..3 {
            let v = if grey_hole && input.hole[i] != 0 {
                128.0
            } else {
                input.image[c][i]
            };
            rgb.push(v.round().clamp(0.0, 255.0) as u8);
        }
    }
    rgb
}

/// The crop with a red rectangle, `thickness` pixels wide, around the hole's bounds grown by
/// `pad`: the "highlighted object" fal's LoRA was trained on.
pub fn outlined(input: &ModelInput, pad: usize, thickness: usize) -> Vec<u8> {
    let n = input.size;
    let mut rgb = init_rgb(input, false);
    let (mut x0, mut y0, mut x1, mut y1) = (n, n, 0, 0);
    for (i, &h) in input.hole.iter().enumerate() {
        if h != 0 {
            let (y, x) = (i / n, i % n);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 {
        return rgb;
    }
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(n - 1), (y1 + pad).min(n - 1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let edge = x < x0 + thickness
                || x + thickness > x1
                || y < y0 + thickness
                || y + thickness > y1;
            if edge {
                rgb[(y * n + x) * 3..(y * n + x) * 3 + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
    }
    rgb
}

impl Inpaint for Klein {
    fn inpaint(&mut self, input: &ModelInput) -> Result<ModelOutput, String> {
        let n = input.size;
        if n % 16 != 0 {
            return Err(format!(
                "klein takes sizes that are multiples of 16, not {n}"
            ));
        }
        let init = init_rgb(input, true);
        let mask: Vec<u8> = input
            .hole
            .iter()
            .map(|&h| if h != 0 { 255 } else { 0 })
            .collect();
        let reference = match self.reference {
            Reference::None => None,
            Reference::Outlined => Some(outlined(input, n / 64, (n / 256).max(2))),
            Reference::Masked => Some(init.clone()),
        };
        let ref_side = self.reference_size.filter(|&s| s < n).unwrap_or(n);
        if ref_side % 16 != 0 {
            return Err(format!(
                "the reference side must be a multiple of 16, not {ref_side}"
            ));
        }
        let reference = reference.map(|r| {
            if ref_side == n {
                r
            } else {
                crate::image::Rgb8::new(n as u32, n as u32, r)
                    .resized(ref_side as u32, ref_side as u32)
                    .data
            }
        });
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = steps.clone();
        sd::set_progress(Some(Box::new(move |_step, _steps, seconds| {
            sink.lock().unwrap().push(seconds as f64 * 1e3);
        })));
        let result = self.ctx.generate(&sd::Request {
            init: &init,
            mask: Some(&mask),
            width: n as u32,
            height: n as u32,
            reference: reference
                .as_deref()
                .map(|r| (r, ref_side as u32, ref_side as u32)),
            prompt: &self.prompt,
            lora: self.lora.as_ref().map(|(p, m)| (p.as_path(), *m)),
            steps: self.steps,
            cfg_scale: 1.0,
            strength: 1.0,
            seed: self.seed as i64,
            sample_method: None,
        });
        sd::set_progress(None);
        self.step_ms = steps.lock().unwrap().clone();
        let rgb = result?;
        let mut out: ModelOutput = Default::default();
        for (c, plane) in out.iter_mut().enumerate() {
            *plane = (0..n * n).map(|i| rgb[i * 3 + c] as f32).collect();
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moebius_schedule_is_the_references() {
        let (cumprod, timesteps) = moebius_schedule();
        assert_eq!(timesteps.len(), 19);
        assert_eq!(timesteps[0], 900);
        assert_eq!(*timesteps.last().unwrap(), 0);
        assert!(timesteps.windows(2).all(|w| w[0] - w[1] == 50));
        // β₀ = 0.00085 and β₉₉₉ = 0.012 exactly.
        assert!((cumprod[0] - (1.0 - 0.00085)).abs() < 1e-12);
        let last_beta = 1.0 - cumprod[999] / cumprod[998];
        assert!((last_beta - 0.012).abs() < 1e-9, "{last_beta}");
    }

    #[test]
    fn a_ddim_step_with_the_true_noise_lands_on_the_clean_sample() {
        let (cumprod, _) = moebius_schedule();
        let clean = [0.5f32, -0.25, 1.0];
        let eps = [0.1f32, -0.3, 0.7];
        let t = 500;
        let at = cumprod[t];
        let noisy: Vec<f32> = clean
            .iter()
            .zip(&eps)
            .map(|(&x, &e)| (at.sqrt() * x as f64 + (1.0 - at).sqrt() * e as f64) as f32)
            .collect();
        let out = ddim_step(&eps, t as i64, -1, &noisy, &cumprod);
        for (o, c) in out.iter().zip(&clean) {
            assert!((o - c).abs() < 1e-5, "{o} vs {c}");
        }
    }

    fn input(n: usize, hole: &[(usize, usize)]) -> (Vec<u8>, [Vec<f32>; 3]) {
        let mut h = vec![0u8; n * n];
        for &(x, y) in hole {
            h[y * n + x] = 1;
        }
        let image = [vec![10.0; n * n], vec![20.0; n * n], vec![30.0; n * n]];
        (h, image)
    }

    #[test]
    fn the_init_image_greys_the_hole_and_keeps_the_rest() {
        let (hole, image) = input(16, &[(3, 4)]);
        let rgb = init_rgb(
            &ModelInput {
                size: 16,
                image: &image,
                hole: &hole,
            },
            true,
        );
        let at = |x: usize, y: usize| &rgb[(y * 16 + x) * 3..(y * 16 + x) * 3 + 3];
        assert_eq!(at(3, 4), &[128, 128, 128]);
        assert_eq!(at(0, 0), &[10, 20, 30]);
    }

    #[test]
    fn the_outline_surrounds_the_hole_and_leaves_its_inside() {
        let (hole, image) = input(32, &[(10, 10), (14, 12)]);
        let rgb = outlined(
            &ModelInput {
                size: 32,
                image: &image,
                hole: &hole,
            },
            2,
            1,
        );
        let at = |x: usize, y: usize| &rgb[(y * 32 + x) * 3..(y * 32 + x) * 3 + 3];
        assert_eq!(at(8, 8), &[255, 0, 0]);
        assert_eq!(at(16, 14), &[255, 0, 0]);
        assert_eq!(at(12, 11), &[10, 20, 30]);
        assert_eq!(at(20, 20), &[10, 20, 30]);
    }

    #[test]
    fn every_candidate_parses_by_name_and_pins_its_files() {
        for &c in Candidate::ALL {
            assert_eq!(c.name().parse::<Candidate>().unwrap(), c);
            assert!(!c.files().is_empty());
            assert!(download_bytes(&c.files()) > 100_000_000);
        }
        assert!("nonsense".parse::<Candidate>().is_err());
    }
}
