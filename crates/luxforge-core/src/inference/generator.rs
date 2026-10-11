//! The generative half of the port: a [`Generator`] fills a masked crop, one candidate per call.
use super::{ModelIdentity, Placement, Provenance, RuntimeIdentity, Tensor, prompt_hash};
use crate::{Cancel, Error};

/// The longest prompt a generation takes, in characters (`docs/design/ai-editing.md`,
/// "Prompt-free fill, prompted Replace").
pub const MAX_PROMPT_CHARS: usize = 256;
/// The most denoising steps one generation may ask for.
pub const MAX_STEPS: u32 = 1000;
/// The longest side of a crop a generator is handed: the pipeline reduces its crop to the model's
/// native size first, which no candidate model exceeds.
pub const MAX_GENERATION_SIDE: u32 = 4096;

/// What conditions a generation besides its crop and mask.
#[derive(Clone, Copy, Debug)]
pub enum Conditioning<'a> {
    /// Prompt-free: the model's own fixed "empty scene" conditioning, a removal.
    None,
    /// A prompt of at most [`MAX_PROMPT_CHARS`] characters, a Replace; the runtime encodes it.
    Prompt(&'a str),
    /// A precomputed `f32` embedding, named by the tensor's name.
    Embedding(&'a Tensor),
}

/// One generation: a `width` × `height` crop in the model's encoded domain (display-referred RGB,
/// `0..=1`, row-major, three values a pixel) with its mask (one coverage `0..=1` a pixel, 1 to be
/// filled), its conditioning, a seed and a step count.
#[derive(Clone, Copy, Debug)]
pub struct GenerateRequest<'a> {
    pub width: u32,
    pub height: u32,
    pub image: &'a [f32],
    pub mask: &'a [f32],
    pub conditioning: Conditioning<'a>,
    pub seed: u64,
    pub steps: u32,
}

impl GenerateRequest<'_> {
    /// Refuse a request no generator should run, by name: a side outside
    /// `1..=`[`MAX_GENERATION_SIDE`], an image or mask of the wrong length, a value that is not
    /// finite or a coverage outside `0..=1`, steps outside `1..=`[`MAX_STEPS`], an empty or
    /// over-long prompt, and an embedding that is not `f32`. Reads every value once.
    pub fn check(&self) -> Result<(), Error> {
        for (name, side) in [("width", self.width), ("height", self.height)] {
            if !(1..=MAX_GENERATION_SIDE).contains(&side) {
                return Err(Error::validation(format!(
                    "a generation's {name} is 1..={MAX_GENERATION_SIDE}, not {side}"
                )));
            }
        }
        let pixels = self.width as usize * self.height as usize;
        if self.image.len() != pixels * 3 || self.mask.len() != pixels {
            return Err(Error::validation(format!(
                "a {}x{} generation takes {} image values and {pixels} mask values, not {} and {}",
                self.width,
                self.height,
                pixels * 3,
                self.image.len(),
                self.mask.len()
            )));
        }
        if !self.image.iter().all(|value| value.is_finite()) {
            return Err(Error::validation("a generation's image values are finite"));
        }
        if !self.mask.iter().all(|value| (0.0..=1.0).contains(value)) {
            return Err(Error::validation("a generation's mask coverage is 0..=1"));
        }
        if !(1..=MAX_STEPS).contains(&self.steps) {
            return Err(Error::validation(format!(
                "a generation takes 1..={MAX_STEPS} steps, not {}",
                self.steps
            )));
        }
        match self.conditioning {
            Conditioning::None => {}
            Conditioning::Prompt(prompt) => {
                let characters = prompt.chars().count();
                if prompt.trim().is_empty() || characters > MAX_PROMPT_CHARS {
                    return Err(Error::validation(format!(
                        "a prompt is 1 to {MAX_PROMPT_CHARS} characters, not {characters}"
                    )));
                }
            }
            Conditioning::Embedding(tensor) => {
                if tensor.f32().is_none() {
                    return Err(Error::validation(format!(
                        "embedding {} is not f32",
                        tensor.name()
                    )));
                }
            }
        }
        Ok(())
    }

    /// The provenance's prompt hash: the prompt's, or the embedding's name's; `None` prompt-free.
    pub fn prompt_hash(&self) -> Option<String> {
        match self.conditioning {
            Conditioning::None => None,
            Conditioning::Prompt(prompt) => Some(prompt_hash(prompt)),
            Conditioning::Embedding(tensor) => Some(prompt_hash(tensor.name())),
        }
    }
}

/// How far one generation has come: `step` of `steps` finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepProgress {
    pub step: u32,
    pub steps: u32,
}

impl StepProgress {
    pub fn fraction(self) -> f64 {
        f64::from(self.step) / f64::from(self.steps.max(1))
    }
}

/// One candidate: the whole crop as the generator answered it, in the same encoded domain and
/// size as its request, and how it was made.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    pub width: u32,
    pub height: u32,
    pub image: Vec<f32>,
    pub provenance: Provenance,
}

impl Generated {
    /// Refuse an answer that is not the request's size or holds a value that is not finite.
    pub fn check(&self, request: &GenerateRequest<'_>) -> Result<(), Error> {
        if (self.width, self.height) != (request.width, request.height)
            || self.image.len() != request.image.len()
        {
            return Err(Error::render(format!(
                "the generator answered {}x{} with {} values for a {}x{} request",
                self.width,
                self.height,
                self.image.len(),
                request.width,
                request.height
            )));
        }
        if !self.image.iter().all(|value| value.is_finite()) {
            return Err(Error::render(
                "the generator answered a value that is not finite",
            ));
        }
        Ok(())
    }
}

/// One opened generative model. Each [`Self::generate`] call yields one candidate, so peak memory
/// stays at one candidate's however many a client asks for: a caller makes several by calling it
/// once per seed, in sequence.
pub trait Generator: Send + Sync {
    /// The model it was opened from.
    fn model(&self) -> &ModelIdentity;

    /// The runtime that opened it.
    fn runtime(&self) -> &RuntimeIdentity;

    /// Where it runs and at what precision.
    fn placement(&self) -> &Placement;

    /// The sampler it denoises with, as provenance names it.
    fn sampler(&self) -> &str;

    /// Fill `request`'s masked pixels: answer one candidate of the request's size, calling
    /// `progress` after each finished step and checking `cancel` at every step boundary, where a
    /// cancelled generation stops and answers `cancelled`, never a partial candidate. The request
    /// is refused by [`GenerateRequest::check`] before any work.
    fn generate(
        &self,
        request: &GenerateRequest<'_>,
        progress: &mut dyn FnMut(StepProgress),
        cancel: &Cancel,
    ) -> Result<Generated, Error>;

    /// The provenance a candidate of `request` records: this generator's model, runtime,
    /// placement and sampler with the request's seed, steps and prompt hash.
    fn provenance(&self, request: &GenerateRequest<'_>) -> Provenance {
        let placement = self.placement();
        Provenance {
            model: self.model().clone(),
            runtime: self.runtime().clone(),
            backend: placement.backend.clone(),
            precision: placement.precision,
            seed: request.seed,
            steps: request.steps,
            sampler: self.sampler().to_owned(),
            prompt_hash: request.prompt_hash(),
        }
    }
}
