//! The deterministic fake runtime: a [`InferenceRuntime`] that opens any model file as a
//! [`Session`] or a [`Generator`] whose answers are pure functions of their inputs, the model's
//! hash and the seed. It is what developer runs and the core's tests are given in place of a native
//! runtime, so the inference port, the `local-runtime` capability and the generative workflow are
//! proven without a model, a GPU or a native crate. A shipped, non-developer run never installs it.
//!
//! - A session declares one `f32` input, `input`, of rank four with every dimension free, and one
//!   output of the same shape, `output`, each element `tanh(w x + b)` with `w` and `b` taken from
//!   the model's SHA-256, rounded through half floats when it was opened at `fp16`. A model file
//!   whose first line is `LFFAKE1 <kind>` opens instead as that analysis model
//!   ([`super::fake_analysis`]): the segmenter's encoder or decoder, the semantic, salient-subject
//!   or people model, or the depth model, each with its real counterpart's tensor names and
//!   shapes.
//! - A generator fills a crop's masked pixels with 8-pixel blocks, each block's colour half the
//!   unmasked context's mean colour and half a colour drawn from a SplitMix64 stream keyed by the
//!   model's hash, the seed, the step count and the prompt or embedding, so different seeds give
//!   visibly different fills and the same inputs always give the same one. Unmasked pixels are
//!   answered as they came; a partly covered pixel is blended by its coverage.
//!
//! Its knobs ([`FakeControls`]) make each step slow and make an open, a run or a given step fail,
//! and it counts what it opened, ran and generated and how many models are alive, so a test can
//! prove a path loaded nothing or that an unload released a model.
use super::fake_analysis::Kind;
use super::{
    Conditioning, Device, GenerateRequest, Generated, Generator, InferenceRuntime, ModelFile,
    ModelIdentity, OpenOptions, Placement, Precision, RuntimeIdentity, Session, StepProgress,
    Tensor, TensorData, TensorSpec,
};
use crate::{Cancel, Error, ErrorKind};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

/// The fake runtime's name, as provenance records it.
pub const FAKE_RUNTIME: &str = "luxforge-fake";
/// The fake runtime's version.
pub const FAKE_VERSION: &str = "1";
/// The one backend the fake offers: every model it opens runs on the CPU.
pub const FAKE_BACKEND: &str = "cpu";
/// The backend name a developer run of the desktop or `luxforge-json` serves the fake under, beside
/// the native runtime's own `cpu` ([`super::CompositeInference`]): a developer module that wants
/// the fake names this backend.
pub const FAKE_HOST_BACKEND: &str = "fake";
/// The fake generator's sampler, as provenance records it.
pub const FAKE_SAMPLER: &str = "fake-blocks";
/// The side of the blocks the fake generator fills with one colour each.
pub const FAKE_BLOCK: u32 = 8;
/// The fake session's input and output names.
pub const FAKE_INPUT: &str = "input";
pub const FAKE_OUTPUT: &str = "output";

/// What a test steers of a fake runtime and what it counts. Shared by the runtime and every model
/// it opened.
#[derive(Debug, Default)]
pub struct FakeControls {
    step_delay_us: AtomicU64,
    fail_open: AtomicBool,
    fail_run: AtomicBool,
    /// 0 never; otherwise the step a generation fails at.
    fail_at_step: AtomicU32,
    opens: AtomicUsize,
    runs: AtomicUsize,
    generations: AtomicUsize,
    /// How many of those were asked to share the GPU.
    shared_generations: AtomicUsize,
    live: AtomicUsize,
}

impl FakeControls {
    /// Sleep `delay` in each generation step, before the step's cancellation check.
    pub fn set_step_delay(&self, delay: Duration) {
        self.step_delay_us.store(
            u64::try_from(delay.as_micros()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }

    /// Make every open fail, or stop.
    pub fn fail_opens(&self, fail: bool) {
        self.fail_open.store(fail, Ordering::SeqCst);
    }

    /// Make every session run fail, or stop.
    pub fn fail_runs(&self, fail: bool) {
        self.fail_run.store(fail, Ordering::SeqCst);
    }

    /// Make every generation fail at `step` (1-based), or never.
    pub fn fail_at_step(&self, step: Option<u32>) {
        self.fail_at_step.store(step.unwrap_or(0), Ordering::SeqCst);
    }

    /// How many candidates were generated asked to share the GPU
    /// ([`GenerateRequest::share_gpu`]), which the fake otherwise ignores.
    pub fn shared_generations(&self) -> usize {
        self.shared_generations.load(Ordering::SeqCst)
    }

    /// How many models were opened.
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    /// How many session runs were made.
    pub fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    /// How many candidates were generated.
    pub fn generations(&self) -> usize {
        self.generations.load(Ordering::SeqCst)
    }

    /// How many opened models are still alive: opened and not yet dropped.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn step_delay(&self) -> Duration {
        Duration::from_micros(self.step_delay_us.load(Ordering::SeqCst))
    }
}

/// The deterministic fake runtime. See the [module documentation](self).
#[derive(Debug, Default)]
pub struct FakeInference {
    controls: Arc<FakeControls>,
}

impl FakeInference {
    pub fn new() -> Self {
        Self::default()
    }

    /// The runtime as a host is handed it ([`crate::HostConfig::inference`]).
    pub fn shared() -> Arc<dyn InferenceRuntime> {
        Arc::new(Self::new())
    }

    /// Its knobs and counters.
    pub fn controls(&self) -> Arc<FakeControls> {
        self.controls.clone()
    }

    fn identity_value() -> RuntimeIdentity {
        RuntimeIdentity {
            name: FAKE_RUNTIME.into(),
            version: FAKE_VERSION.into(),
        }
    }

    /// The checks every open makes: a backend this runtime offers, the knob, the file.
    fn open(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Live, Error> {
        cancel.check()?;
        let backend = options.choose(&self.backends())?.to_owned();
        if self.controls.fail_open.load(Ordering::SeqCst) {
            return Err(Error::render(format!(
                "the fake runtime was told to fail opening {}",
                model.identity().id
            )));
        }
        for file in std::iter::once(model).chain(model.companions()) {
            if !file.path().is_file() {
                return Err(Error::new(
                    ErrorKind::SourceUnavailable,
                    format!("model file of {} is missing", file.identity().id),
                ));
            }
        }
        self.controls.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Live::new(
            self.controls.clone(),
            Placement {
                backend,
                device: Device::Cpu,
                precision: options.precision,
                partitions: None,
            },
        ))
    }
}

impl InferenceRuntime for FakeInference {
    fn identity(&self) -> RuntimeIdentity {
        Self::identity_value()
    }

    fn backends(&self) -> Vec<String> {
        vec![FAKE_BACKEND.into()]
    }

    fn open_session(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Session>, Error> {
        let live = self.open(model, options, cancel)?;
        let spec = |name: &str| TensorSpec {
            name: name.into(),
            element: super::ElementType::F32,
            shape: vec![None; 4],
        };
        let kind = Kind::of(model.path());
        let (inputs, outputs) = match kind {
            Some(kind) => kind.specs(),
            None => (vec![spec(FAKE_INPUT)], vec![spec(FAKE_OUTPUT)]),
        };
        Ok(Box::new(FakeSession {
            key: model_key(model.identity()),
            kind,
            model: model.identity().clone(),
            inputs,
            outputs,
            live,
        }))
    }

    fn open_generator(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Generator>, Error> {
        let live = self.open(model, options, cancel)?;
        // Opened with companions, the model is a text encoder and its first companion the
        // generator, which is what a candidate names, as `luxforge-diffusion` does.
        let generator = model.companions().first().unwrap_or(model);
        Ok(Box::new(FakeGenerator {
            key: model_key(generator.identity()),
            model: generator.identity().clone(),
            runtime: Self::identity_value(),
            live,
        }))
    }
}

/// One opened model's share of the controls, counted alive until it is dropped.
#[derive(Debug)]
struct Live {
    controls: Arc<FakeControls>,
    placement: Placement,
}

impl Live {
    fn new(controls: Arc<FakeControls>, placement: Placement) -> Self {
        controls.live.fetch_add(1, Ordering::SeqCst);
        Self {
            controls,
            placement,
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.controls.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The SplitMix64 step: a well-mixed 64-bit value of `state`.
fn mix(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A key of `bytes`, folded eight at a time.
fn fold(bytes: &[u8]) -> u64 {
    bytes.chunks(8).fold(0x6c75_7866_6f72_6765, |key, chunk| {
        let mut word = [0_u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        mix(key ^ u64::from_le_bytes(word))
    })
}

fn model_key(model: &ModelIdentity) -> u64 {
    fold(model.sha256.as_bytes())
}

/// The fake session. See the [module documentation](self).
struct FakeSession {
    key: u64,
    /// The analysis model the file names, or `None` for the generic session.
    kind: Option<Kind>,
    model: ModelIdentity,
    inputs: Vec<TensorSpec>,
    outputs: Vec<TensorSpec>,
    live: Live,
}

impl Session for FakeSession {
    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn placement(&self) -> &Placement {
        &self.live.placement
    }

    fn inputs(&self) -> &[TensorSpec] {
        &self.inputs
    }

    fn outputs(&self) -> &[TensorSpec] {
        &self.outputs
    }

    fn run(&self, inputs: &[Tensor], cancel: &Cancel) -> Result<Vec<Tensor>, Error> {
        cancel.check()?;
        if let Some(kind) = self.kind {
            let admitted = inputs.len() == self.inputs.len()
                && inputs
                    .iter()
                    .zip(&self.inputs)
                    .all(|(input, spec)| input.name() == spec.name && spec.admits(input));
            if !admitted {
                let names: Vec<_> = self.inputs.iter().map(|spec| spec.name.as_str()).collect();
                return Err(Error::validation(format!(
                    "the fake {kind:?} model takes {names:?}, not {:?}",
                    inputs
                        .iter()
                        .map(|input| (input.name(), input.shape()))
                        .collect::<Vec<_>>()
                )));
            }
            if self.live.controls.fail_run.load(Ordering::SeqCst) {
                return Err(Error::render("the fake session was told to fail"));
            }
            self.live.controls.runs.fetch_add(1, Ordering::SeqCst);
            return kind.run(inputs, cancel);
        }
        let [input] = inputs else {
            return Err(Error::validation(format!(
                "the fake session takes one input, {FAKE_INPUT}, not {}",
                inputs.len()
            )));
        };
        if input.name() != FAKE_INPUT || !self.inputs[0].admits(input) {
            return Err(Error::validation(format!(
                "the fake session takes {FAKE_INPUT} as rank-4 f32, not {} {:?}",
                input.name(),
                input.shape()
            )));
        }
        if self.live.controls.fail_run.load(Ordering::SeqCst) {
            return Err(Error::render("the fake session was told to fail"));
        }
        self.live.controls.runs.fetch_add(1, Ordering::SeqCst);
        let weight = 0.5 + (self.key & 0xff) as f32 / 255.0;
        let bias = ((self.key >> 8) & 0xff) as f32 / 255.0 - 0.5;
        let half = self.live.placement.precision == Precision::Fp16;
        let values = input.f32().unwrap_or_default();
        let mut output = Vec::with_capacity(values.len());
        for chunk in values.chunks(1 << 16) {
            cancel.check()?;
            output.extend(chunk.iter().map(|value| {
                let answer = (weight * value + bias).tanh();
                if half {
                    half::f16::from_f32(answer).to_f32()
                } else {
                    answer
                }
            }));
        }
        Ok(vec![Tensor::new(
            FAKE_OUTPUT,
            input.shape().to_vec(),
            TensorData::F32(output),
        )?])
    }
}

/// The fake generator. See the [module documentation](self).
struct FakeGenerator {
    key: u64,
    model: ModelIdentity,
    runtime: RuntimeIdentity,
    live: Live,
}

impl FakeGenerator {
    /// The key of one generation's pattern: the model, the seed, the steps and the conditioning.
    fn generation_key(&self, request: &GenerateRequest<'_>) -> u64 {
        let conditioning = match request.conditioning {
            Conditioning::None => 0,
            Conditioning::Prompt(prompt) => fold(prompt.as_bytes()),
            Conditioning::Embedding(tensor) => tensor
                .f32()
                .unwrap_or_default()
                .iter()
                .fold(fold(tensor.name().as_bytes()), |key, value| {
                    mix(key ^ u64::from(value.to_bits()))
                }),
        };
        mix(mix(mix(self.key ^ request.seed) ^ u64::from(request.steps)) ^ conditioning)
    }
}

impl Generator for FakeGenerator {
    fn model(&self) -> &ModelIdentity {
        &self.model
    }

    fn runtime(&self) -> &RuntimeIdentity {
        &self.runtime
    }

    fn placement(&self) -> &Placement {
        &self.live.placement
    }

    fn sampler(&self) -> &str {
        FAKE_SAMPLER
    }

    fn generate(
        &self,
        request: &GenerateRequest<'_>,
        progress: &mut dyn FnMut(StepProgress),
        cancel: &Cancel,
    ) -> Result<Generated, Error> {
        request.check()?;
        cancel.check()?;
        let controls = &self.live.controls;
        let delay = controls.step_delay();
        let fail_at = controls.fail_at_step.load(Ordering::SeqCst);
        for step in 1..=request.steps {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            cancel.check()?;
            if step == fail_at {
                return Err(Error::render(format!(
                    "the fake generator was told to fail at step {step}"
                )));
            }
            progress(StepProgress {
                step,
                steps: request.steps,
            });
        }
        let key = self.generation_key(request);
        let (width, height) = (request.width as usize, request.height as usize);
        // The context's colour: the mean of the unmasked share of every pixel.
        let mut sum = [0.0_f64; 3];
        let mut weight = 0.0_f64;
        for (pixel, coverage) in request.mask.iter().enumerate() {
            let keep = f64::from(1.0 - coverage);
            for (channel, total) in sum.iter_mut().enumerate() {
                *total += keep * f64::from(request.image[pixel * 3 + channel]);
            }
            weight += keep;
        }
        let mean = if weight > 0.0 {
            sum.map(|total| (total / weight) as f32)
        } else {
            [0.5; 3]
        };
        let mut image = request.image.to_vec();
        for y in 0..height {
            if y % FAKE_BLOCK as usize == 0 {
                cancel.check()?;
            }
            for x in 0..width {
                let pixel = y * width + x;
                let coverage = request.mask[pixel];
                if coverage == 0.0 {
                    continue;
                }
                let block =
                    ((y as u64 / u64::from(FAKE_BLOCK)) << 32) | (x as u64 / u64::from(FAKE_BLOCK));
                let colour = mix(key ^ block);
                for channel in 0..3 {
                    let drawn = ((colour >> (channel * 8)) & 0xff) as f32 / 255.0;
                    let fill = 0.5 * mean[channel] + 0.5 * drawn;
                    let held = &mut image[pixel * 3 + channel];
                    *held = *held * (1.0 - coverage) + fill * coverage;
                }
            }
        }
        controls.generations.fetch_add(1, Ordering::SeqCst);
        if request.share_gpu {
            controls.shared_generations.fetch_add(1, Ordering::SeqCst);
        }
        let generated = Generated {
            width: request.width,
            height: request.height,
            image,
            provenance: self.provenance(request),
        };
        generated.check(request)?;
        Ok(generated)
    }
}
