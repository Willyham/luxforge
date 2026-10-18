//! FLUX.2 klein 4B behind [`Generator`]: the qualified removal (fal's LoRA, the crop outlined in
//! red as the reference, the shipped conditioning in place of the text encoder) and Replace (the
//! encoder's encoding of "Replace the highlighted object with …", no LoRA), each four flow steps
//! at CFG 1 and strength 1 through stable-diffusion.cpp's masked path, which restores the known
//! latents after every step. The inputs are built exactly as the qualification harness built them
//! (`probes/object-removal/src/generative.rs`), so the same crop, mask and seed give the harness's
//! candidate bytes.
use crate::layout::Files;
use luxforge_core::{
    Cancel, Error,
    inference::{
        Conditioning, GenerateRequest, Generated, Generator, ModelFile, ModelIdentity, Placement,
        RuntimeIdentity, StepProgress,
    },
};
use serde_json::json;
use std::sync::Mutex;

/// fal's LoRA is trained on this instruction with the object outlined in red; its encoding is the
/// shipped conditioning, so a removal runs with no text encoder.
pub const LORA_PROMPT: &str = "Remove the highlighted object from the scene";
/// What Replace's prompt follows: the instruction the qualification's Replace used.
pub const REPLACE_PREFIX: &str = "Replace the highlighted object with ";
/// The largest side a generation takes: the working size its memory and time were measured at.
/// The runtime reserves its compute buffers for the size it first meets.
pub const MAX_SIDE: u32 = 1024;
/// The LoRA's strength, as qualified.
const LORA_MULTIPLIER: f32 = 1.0;

/// One generation at a time in a process: upstream's progress callback and the GPU are shared.
static GENERATING: Mutex<()> = Mutex::new(());

/// Replace's whole prompt for what the person typed.
pub fn replace_prompt(prompt: &str) -> String {
    format!("{REPLACE_PREFIX}{}", prompt.trim())
}

/// `width × height` RGB bytes with a red rectangle around the hole's bounds, grown by `side / 64`
/// and `max(side / 256, 2)` pixels thick (`side` the shorter side), and the object left inside:
/// the "highlighted object" fal's LoRA was trained on.
pub fn outlined(rgb: &[u8], hole: &[bool], width: usize, height: usize) -> Vec<u8> {
    let mut out = rgb.to_vec();
    let side = width.min(height);
    let (pad, thickness) = (side / 64, (side / 256).max(2));
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    for (i, _) in hole.iter().enumerate().filter(|(_, h)| **h) {
        let (y, x) = (i / width, i % width);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    if x0 > x1 {
        return out;
    }
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(width - 1), (y1 + pad).min(height - 1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let edge = x < x0 + thickness
                || x + thickness > x1
                || y < y0 + thickness
                || y + thickness > y1;
            if edge {
                out[(y * width + x) * 3..(y * width + x) * 3 + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
    }
    out
}

/// One opened klein model.
pub struct KleinGenerator {
    model: ModelIdentity,
    runtime: RuntimeIdentity,
    placement: Placement,
    sampler: String,
    files: Files,
    #[cfg(luxforge_sdcpp)]
    context: Mutex<crate::native::Context>,
}

impl std::fmt::Debug for KleinGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KleinGenerator")
            .field("model", &self.model)
            .field("placement", &self.placement)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

impl KleinGenerator {
    /// Open `model` (a generator resource, or a text encoder resource with the generator as its
    /// companion) on `placement`'s backend. The runtime maps the weights now and loads them at the
    /// first generation.
    pub fn open(
        model: &ModelFile,
        placement: Placement,
        runtime: RuntimeIdentity,
        cancel: &Cancel,
    ) -> Result<Self, Error> {
        let files = Files::of(model)?;
        cancel.check()?;
        // The model a candidate names is the generator, whichever resource the slot holds.
        let identity = model
            .companions()
            .first()
            .unwrap_or(model)
            .identity()
            .clone();
        #[cfg(luxforge_sdcpp)]
        {
            use crate::native;
            native::validate_condition(&files.conditioning).map_err(Error::incompatible)?;
            if let Some(level) = std::env::var("LUXFORGE_SD_LOG")
                .ok()
                .and_then(|value| value.parse().ok())
            {
                native::set_log_level(level);
            }
            let (llm, tokenizer) = match &files.encoder {
                Some((encoder, tokenizer)) => (Some(encoder.as_path()), Some(tokenizer.as_path())),
                None => (None, None),
            };
            let mut context = native::Context::open(&native::OpenOptions {
                diffusion_model: &files.transformer,
                vae: &files.vae,
                llm,
                tokenizer,
                threads: 0,
                flash_attn: true,
            })
            .map_err(|detail| {
                Error::incompatible(detail).with_data(json!({"model": identity.id}))
            })?;
            context
                .supply_condition(LORA_PROMPT, Some(&files.conditioning))
                .map_err(Error::incompatible)?;
            let sampler = format!(
                "{}/{}",
                context.default_sampler(),
                context.default_scheduler()
            );
            Ok(Self {
                model: identity,
                runtime,
                placement,
                sampler,
                files,
                context: Mutex::new(context),
            })
        }
        #[cfg(not(luxforge_sdcpp))]
        {
            let _ = (identity, placement, runtime);
            Err(Error::not_ready(
                "this build of luxforge-diffusion compiles no runtime for this platform",
            ))
        }
    }

    /// Whether this generator loaded the text encoder, so it takes a prompt.
    pub fn encodes_prompts(&self) -> bool {
        self.files.encoder.is_some()
    }

    /// The files it opened.
    pub fn files(&self) -> &Files {
        &self.files
    }
}

/// Refuse what a klein generator cannot run, before anything is allocated: a request the port
/// refuses, a side past [`MAX_SIDE`] (`resource-limit`, data `limit: generation-side`), a side
/// that is not a multiple of 16, a prompt to a generator opened without its text encoder, and an
/// embedding.
pub(crate) fn admit(
    request: &GenerateRequest<'_>,
    encodes_prompts: bool,
    model: &str,
) -> Result<(), Error> {
    request.check()?;
    let (width, height) = (request.width, request.height);
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::resource_limit(format!(
            "a klein generation is at most {MAX_SIDE} px a side, the size its memory budget was measured at, not {width}x{height}"
        ))
        .with_data(json!({"limit": "generation-side", "max_side": MAX_SIDE})));
    }
    if width % 16 != 0 || height % 16 != 0 {
        return Err(Error::validation(format!(
            "klein generates sizes that are multiples of 16, not {width}x{height}"
        )));
    }
    match request.conditioning {
        Conditioning::Prompt(_) if !encodes_prompts => Err(Error::incompatible(format!(
            "model {model} was opened without its text encoder and takes no prompt"
        ))),
        Conditioning::Embedding(tensor) => Err(Error::incompatible(format!(
            "klein takes the removal conditioning it was installed with or a prompt, not embedding {}",
            tensor.name()
        ))),
        _ => Ok(()),
    }
}

/// The request's inputs as the harness built them: the crop's 8-bit codes with the hole mid-grey
/// (upstream's masked-image convention), the mask at 255 in the hole, and the crop with the object
/// outlined in red.
pub(crate) struct Inputs {
    pub(crate) init: Vec<u8>,
    pub(crate) mask: Vec<u8>,
    pub(crate) reference: Vec<u8>,
}

pub(crate) fn inputs(request: &GenerateRequest<'_>) -> Inputs {
    let (width, height) = (request.width as usize, request.height as usize);
    let rgb: Vec<u8> = request
        .image
        .iter()
        .map(|value| (value * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    let hole: Vec<bool> = request
        .mask
        .iter()
        .map(|coverage| *coverage > 0.0)
        .collect();
    let reference = outlined(&rgb, &hole, width, height);
    let mut init = rgb;
    for (i, _) in hole.iter().enumerate().filter(|(_, h)| **h) {
        init[i * 3..i * 3 + 3].copy_from_slice(&[128, 128, 128]);
    }
    let mask = hole.iter().map(|h| if *h { 255 } else { 0 }).collect();
    Inputs {
        init,
        mask,
        reference,
    }
}

impl Generator for KleinGenerator {
    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn runtime(&self) -> &RuntimeIdentity {
        &self.runtime
    }

    fn placement(&self) -> &Placement {
        &self.placement
    }

    fn sampler(&self) -> &str {
        &self.sampler
    }

    fn generate(
        &self,
        request: &GenerateRequest<'_>,
        progress: &mut dyn FnMut(StepProgress),
        cancel: &Cancel,
    ) -> Result<Generated, Error> {
        admit(request, self.encodes_prompts(), &self.model.id)?;
        cancel.check()?;
        #[cfg(luxforge_sdcpp)]
        {
            use crate::native;
            let inputs = inputs(request);
            let (prompt, lora) = match request.conditioning {
                Conditioning::Prompt(prompt) => (replace_prompt(prompt), None),
                _ => (
                    LORA_PROMPT.to_owned(),
                    Some((self.files.lora.as_path(), LORA_MULTIPLIER)),
                ),
            };
            let steps = request.steps;
            let _one = GENERATING
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            cancel.check()?;
            let mut context = self
                .context
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut report = |step: i32, of: i32, _seconds: f32| {
                // Upstream reports step 0 as sampling starts and each finished step after.
                if of == steps as i32 && (1..=of).contains(&step) {
                    progress(StepProgress {
                        step: step as u32,
                        steps,
                    });
                }
            };
            let cancelled = || cancel.check().is_err();
            let answer = context.generate_watched(
                &native::Request {
                    init: &inputs.init,
                    mask: Some(&inputs.mask),
                    width: request.width,
                    height: request.height,
                    reference: Some((&inputs.reference, request.width, request.height)),
                    prompt: &prompt,
                    lora,
                    steps: steps as i32,
                    cfg_scale: 1.0,
                    strength: 1.0,
                    seed: request.seed as i64,
                    sample_method: None,
                },
                &mut report,
                &cancelled,
            );
            drop(context);
            // A cancelled generation answers cancelled, never a partial or late candidate.
            cancel.check()?;
            let rgb = answer.map_err(Error::render)?;
            let generated = Generated {
                width: request.width,
                height: request.height,
                image: rgb.iter().map(|&v| f32::from(v) / 255.0).collect(),
                provenance: self.provenance(request),
            };
            generated.check(request)?;
            Ok(generated)
        }
        #[cfg(not(luxforge_sdcpp))]
        {
            let _ = progress;
            Err(Error::not_ready(
                "this build of luxforge-diffusion compiles no runtime",
            ))
        }
    }
}
