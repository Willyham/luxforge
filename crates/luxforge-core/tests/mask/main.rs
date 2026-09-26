//! Masking end to end, one module per area: the mask-space, composition and gradient study
//! (`study`), the brush study (`brush_study`) and the range study (`range_study`) against the
//! independent references in `luxforge-reference`; the compiled kinds against those references
//! (`unit`, `radial`, `brush`, `constrained_brush`, `range`, `combination`); the coverage grid
//! (`overlay`); geometry survival and reopen (`geometry_survival`); and masked edits on the colour
//! and spatial paths and through the JSON method table (`masked_colour`, `masked_spatial`,
//! `masked_edit_end_to_end`). The helpers below are the ones several of them share; the rest come
//! from `luxforge-testkit`.

mod brush;
mod brush_study;
mod combination;
mod constrained_brush;
mod geometry_survival;
mod masked_colour;
mod masked_edit_end_to_end;
mod masked_spatial;
mod overlay;
mod radial;
mod range;
mod range_study;
mod study;
mod unit;

use luxforge_core::{
    BASIC_EFFECT, Component, ComponentMode, Layer, LinearImage, Mask, Raster, Recipe, SourceImage,
    Stage,
};
use luxforge_reference::mask::{
    Component as RefComponent, Kind, Linear, Mask as RefMask, Mode, Stage as RefStage,
};
use luxforge_testkit::fixtures;
use serde_json::{Value, json};
use std::path::PathBuf;

use luxforge_reference::SplitMix64;

/// A pixel for the geometric kinds, which ignore the pixel they are handed.
const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

/// The small synthetic stage the rendered composition, range and masked-colour tests use.
const WIDTH: u32 = 24;
const HEIGHT: u32 = 16;

/// The exposure a masked layer applies, in EV. Large enough that a coverage difference of one part
/// in a thousand is visible in the output codes rather than lost in the quantizer.
const MASKED_EV: f64 = 2.0;

/// The study's two 24 MP stages.
const LANDSCAPE: RefStage = RefStage::new(6000, 4000);
const PORTRAIT: RefStage = RefStage::new(4000, 6000);

/// The stages the compiled gradients are held to the reference on: both 24 MP orientations, a
/// square and a panorama.
const STAGES: [(u32, u32); 4] = [(6000, 4000), (4000, 6000), (2048, 2048), (7000, 1400)];

/// The pointwise contract's relative band around a code threshold.
const CODE_BAND: f64 = 1e-6;

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

fn ref_stage(width: u32, height: u32) -> RefStage {
    RefStage::new(width, height)
}

/// The component mode and the reference's mode for one position in a randomized mask.
fn mode_of(index: usize) -> (ComponentMode, Mode) {
    match index {
        0 => (ComponentMode::Add, Mode::Add),
        1 => (ComponentMode::Subtract, Mode::Subtract),
        _ => (ComponentMode::Intersect, Mode::Intersect),
    }
}

/// A scratch directory for one journey's catalog.
fn temp(name: &str) -> PathBuf {
    fixtures::temp_dir(&format!("mask-{name}"))
}

/// The rendered code against the reference's, within [`CODE_BAND`].
fn assert_code(actual: u8, expected: u8, linear: f64, case: &str) {
    fixtures::assert_code_near_threshold(actual, expected, linear, CODE_BAND, case);
}

fn luma(raster: &Raster, x: u32, y: u32) -> f64 {
    let p = raster.pixel(x, y).expect("pixel inside the stage");
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

/// The [`WIDTH`] x [`HEIGHT`] gradient every channel of which varies, so a coverage difference
/// shows in some channel of every pixel.
fn byte_source() -> SourceImage {
    let pixels: Vec<[u8; 3]> = (0..HEIGHT)
        .flat_map(|y| {
            (0..WIDTH).map(move |x| {
                [
                    (x * 9 + 3) as u8,
                    (y * 13 + 40) as u8,
                    (x * 5 + y * 7 + 90) as u8,
                ]
            })
        })
        .collect();
    fixtures::source_of(WIDTH, HEIGHT, &pixels)
}

/// A byte source decoded to linear light, as a RAW development hands the linear path the same
/// picture.
fn decoded(source: &SourceImage) -> LinearImage {
    let pixels: Vec<[f64; 3]> = source
        .rgba
        .chunks_exact(4)
        .map(|pixel| {
            std::array::from_fn(|channel| luxforge_reference::srgb_to_linear(pixel[channel]))
        })
        .collect();
    fixtures::linear_source_of(source.width, source.height, &pixels)
}

/// A global Basic layer holding `payload`.
fn basic_layer(payload: Value) -> Layer {
    fixtures::layer(BASIC_EFFECT, payload)
}

/// `layer` bound to `mask`.
fn masked(layer: Layer, mask: &Mask) -> Layer {
    Layer {
        mask: Some(mask.id.clone()),
        ..layer
    }
}

/// A current-format recipe of these layers and masks.
fn recipe(layers: Vec<Layer>, masks: Vec<Mask>) -> Recipe {
    Recipe {
        masks,
        ..fixtures::recipe(layers)
    }
}

/// One linear-gradient mask in both spellings: the stored model the host compiles, and the
/// reference's own structure.
fn gradient_mask(x0: f64, y0: f64, x1: f64, y1: f64, amount: f64) -> (Mask, RefMask) {
    let mut mask = Mask::new("Mask 1");
    mask.amount = amount;
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1}),
    ));
    let oracle = RefMask {
        amount,
        invert: false,
        components: vec![RefComponent {
            mode: Mode::Add,
            invert: false,
            kind: Kind::Linear(Linear { x0, y0, x1, y1 }),
        }],
    };
    (mask, oracle)
}
