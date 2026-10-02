//! Qualification only: the Detail CPU kernels and units over whole frames, for the desktop's
//! per-kernel readback tests, which hold each GPU kernel to the CPU kernel it transcribes on
//! synthetic planes (`docs/design/gpu-preview.md`, "Qualifying a program").
//!
//! Each function runs the production code in `filters.rs`, `denoise.rs` and `sharpen.rs` over one
//! whole frame as one tile, serially. Built only with the core's `qualification` feature, which
//! only a `[dev-dependencies]` table may turn on (`cargo xtask check-repository`), so no build of a
//! binary has it.
use super::{
    DetailModule,
    denoise::{self, Denoise},
    filters::{self, Geometry, Kernel},
    gpu,
    sharpen::Sharpen,
};
use crate::{
    Cancel,
    colour::oklab,
    modules::{
        CompileStage, Parallelism, Planes, PlanesMut, Processing, Region, SamplingScale, Stage,
        ToolModule,
    },
};

/// A kernel the units smooth with.
#[derive(Clone, Copy, Debug)]
pub enum Smoothing {
    /// The B3 a-trous kernel at this spacing, noise reduction's at full resolution.
    B3(u32),
    /// The sampled Gaussian of this sigma, sharpening's and noise reduction's at a proxy's scale.
    Gaussian(f64),
}

impl Smoothing {
    fn kernel(self) -> Kernel {
        match self {
            Self::B3(spacing) => Kernel::b3(spacing),
            Self::Gaussian(sigma) => Kernel::gaussian(sigma),
        }
    }

    /// How many taps it has, its centre left out.
    pub fn taps(self) -> usize {
        self.kernel().taps.len()
    }
}

/// The words of `kernel`'s slot, as the units' descriptions lay them out.
pub fn slot_words(kernel: Smoothing) -> Vec<u32> {
    gpu::slot_words(&kernel.kernel())
}

/// The whole-frame geometry of a `width × height` stage.
fn frame(width: u32, height: u32) -> (Stage, Region, Geometry) {
    let stage = Stage { width, height };
    let held = Region::whole(stage);
    (stage, held, Geometry { stage, held })
}

/// The separable smoothing of one `width × height` plane, the horizontal pass by `x` and the
/// vertical by `y`, edge-clamped, in difference form.
pub fn smooth(width: u32, height: u32, plane: &[f32], x: Smoothing, y: Smoothing) -> Vec<f32> {
    let (_, held, geometry) = frame(width, height);
    let mut out = vec![0.0; plane.len()];
    let mut temporary = vec![0.0; plane.len()];
    filters::smooth(
        plane,
        &mut out,
        &mut temporary,
        geometry,
        held,
        &[x.kernel(), y.kernel()],
        Parallelism::Serial,
        &Cancel::never(),
    )
    .expect("the smoothing's planes");
    out
}

/// Linear sRGB in Oklab, as the units convert their input.
pub fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    let lab = oklab::to_oklab(rgb);
    [lab.l, lab.a, lab.b]
}

/// A unit's output from its input, the input's Oklab and the change in Oklab.
pub fn reconstruct(input: [f32; 3], lab: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
    filters::reconstruct(input, lab, delta)
}

/// Noise reduction's per-level thresholds `[luminance, chroma]` at these strengths.
pub fn thresholds(luminance: f64, colour: f64) -> Vec<[f32; 2]> {
    Denoise::new(
        luminance,
        50.0,
        colour,
        50.0,
        SamplingScale { x: 1.0, y: 1.0 },
    )
    .thresholds()
    .to_vec()
}

/// One level's soft shrinkage over a `width × height` frame: `band` the level's detail band and
/// `delta` the change the levels before it accumulated, each planar L, a and b. The change after
/// this level.
pub fn shrink(
    width: u32,
    height: u32,
    band: &[f32],
    delta: &[f32],
    thresholds: [f32; 2],
    details: [f32; 2],
) -> Vec<f32> {
    let (_, held, geometry) = frame(width, height);
    let mut out = delta.to_vec();
    denoise::shrink(
        band,
        &mut out,
        geometry,
        held,
        thresholds,
        details,
        Parallelism::Serial,
        &Cancel::never(),
    )
    .expect("the shrinkage's planes");
    out
}

/// Sharpening's coefficients at these settings: the gain, the coring threshold squared and the
/// masking scale squared, the words its change of lightness reads.
pub fn sharpen_coefficients(amount: f64, detail: f64, masking: f64) -> [f32; 3] {
    Sharpen::new(
        amount,
        1.0,
        detail,
        masking,
        SamplingScale { x: 1.0, y: 1.0 },
    )
    .coefficients()
}

/// Sharpening's change of lightness over a `width × height` frame from its planes: the input's
/// lightness `l`, its blur and its guide. The limited lightness less `l`, per pixel.
#[allow(clippy::too_many_arguments)]
pub fn sharpen_change(
    width: u32,
    height: u32,
    l: &[f32],
    blurred: &[f32],
    guide: &[f32],
    amount: f64,
    detail: f64,
    masking: f64,
) -> Vec<f32> {
    let (_, _, geometry) = frame(width, height);
    let unit = Sharpen::new(
        amount,
        1.0,
        detail,
        masking,
        SamplingScale { x: 1.0, y: 1.0 },
    );
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let i = (y * width + x) as usize;
            unit.limited(l, blurred, guide, geometry, i64::from(x), i64::from(y)) - l[i]
        })
        .collect()
}

/// A Detail layer of `payload` over a whole `width × height` frame of linear pixels, compiled at
/// that stage against a full-resolution stage of `full`, as the host compiles a proxy: every unit
/// run in order over the whole frame. The CPU's output.
pub fn detail(
    width: u32,
    height: u32,
    full: (u32, u32),
    pixels: &[[f32; 3]],
    payload: &serde_json::Value,
) -> Vec<[f32; 3]> {
    let (stage, held, _) = frame(width, height);
    let at = CompileStage::sampled(
        stage,
        Stage {
            width: full.0,
            height: full.1,
        },
    );
    let Processing::Spatial(operation) = DetailModule::new()
        .compile(super::DETAIL_EFFECT, 1, payload, at)
        .expect("a Detail payload")
    else {
        panic!("Detail is spatial");
    };
    let len = pixels.len();
    let mut current: Vec<f32> = (0..3)
        .flat_map(|c| pixels.iter().map(move |p| p[c]))
        .collect();
    for unit in operation.units() {
        let mut next = vec![0.0; current.len()];
        let mut scratch = vec![0.0; (unit.scratch_bytes(stage) / 4) as usize];
        unit.apply(
            &Planes::new(stage, held, &current).expect("the input planes"),
            &mut PlanesMut::new(stage, held, &mut next).expect("the output planes"),
            None,
            &mut scratch,
            Parallelism::Serial,
        )
        .expect("the unit's output");
        current = next;
    }
    (0..len)
        .map(|i| [current[i], current[len + i], current[2 * len + i]])
        .collect()
}
