//! Promptable segmentation: an image encoder run once per image and a prompt decoder run per
//! hover, click or stroke.
//!
//! Each family follows its reference implementation's conventions exactly; `frame.rs` holds the
//! geometry and cites the sources.

mod evit;
mod frame;
mod resample;
mod sam21;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub use frame::Frame;
pub use resample::{resample, resample_rgb8};

use crate::image::Rgb8;
use crate::mask::{Logits, Point};
use crate::models;
use crate::runtime::{self, Backend};
use crate::tensor::Tensor;

/// A pinned segmenter model pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// EfficientViT-SAM L0 (Apache-2.0): 512² encoder, SAM 1 prompt decoder, one mask per prompt.
    EfficientVitL0,
    /// SAM 2.1 Hiera Small as exported by darktable-ai (Apache-2.0 weights): 1024² encoder with
    /// high-resolution features, multimask decoder.
    Sam21Small,
}

impl Family {
    pub const ALL: &[Family] = &[Family::EfficientVitL0, Family::Sam21Small];

    pub fn name(self) -> &'static str {
        match self {
            Family::EfficientVitL0 => "evit-l0",
            Family::Sam21Small => "sam21-small",
        }
    }

    /// The pinned encoder and decoder files.
    pub fn models(self) -> (&'static models::Model, &'static models::Model) {
        match self {
            Family::EfficientVitL0 => (&models::EVIT_L0_ENCODER, &models::EVIT_L0_DECODER),
            Family::Sam21Small => (&models::SAM21_SMALL_ENCODER, &models::SAM21_SMALL_DECODER),
        }
    }
}

impl FromStr for Family {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Family::ALL
            .iter()
            .copied()
            .find(|f| f.name() == s)
            .ok_or_else(|| format!("unknown segmenter {s:?}; expected evit-l0 or sam21-small"))
    }
}

/// One prompt in source pixels. At least one point or a box is required.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub positive: Vec<Point>,
    pub negative: Vec<Point>,
    /// Top-left and bottom-right corners.
    pub bbox: Option<(Point, Point)>,
    /// A previous result fed back to refine it (SAM's `mask_input`); decoders that take none
    /// ignore it.
    pub previous: Option<Logits>,
}

/// One decoded mask and the decoder's own estimate of its IoU with the intended object.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub logits: Logits,
    pub predicted_iou: f32,
}

/// An image's encoder output, reusable for every prompt on that image.
#[derive(Clone, Debug)]
pub struct Embedding {
    pub family: Family,
    /// The source image's size.
    pub source: (u32, u32),
    pub tensors: Vec<(String, Tensor)>,
}

impl Embedding {
    /// Bytes held by the cached tensors.
    pub fn bytes(&self) -> usize {
        self.tensors.iter().map(|(_, t)| t.len() * 4).sum()
    }

    /// The cached tensor of this name.
    pub fn tensor(&self, name: &str) -> Result<&Tensor, String> {
        self.tensors
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t)
            .ok_or_else(|| format!("{} embedding has no tensor {name:?}", self.family.name()))
    }
}

pub trait Segmenter: Send {
    fn family(&self) -> Family;

    /// Runs the image encoder over the whole image.
    fn encode(&mut self, image: &Rgb8) -> Result<Embedding, String>;

    /// Decodes one prompt against an embedding. Returns every mask the decoder produces for it
    /// (one for EfficientViT-SAM; up to three for SAM 2.1), highest predicted IoU first.
    fn decode(&mut self, embedding: &Embedding, prompt: &Prompt) -> Result<Vec<Candidate>, String>;

    /// Decodes several prompts. Implementations may batch them into one decoder call.
    fn decode_many(
        &mut self,
        embedding: &Embedding,
        prompts: &[Prompt],
    ) -> Result<Vec<Vec<Candidate>>, String> {
        prompts.iter().map(|p| self.decode(embedding, p)).collect()
    }
}

/// How long each model took to load (runtime load only, after verification; for Core ML this
/// includes compiling the model or reading its compiled cache).
#[derive(Clone, Copy, Debug)]
pub struct LoadTimes {
    pub encoder: Duration,
    pub decoder: Duration,
}

/// Loads a segmenter's encoder and decoder from the verified models directory, each on its own
/// backend.
pub fn open(
    family: Family,
    encoder: Backend,
    decoder: Backend,
    models: &Path,
) -> Result<Box<dyn Segmenter>, String> {
    open_timed(family, encoder, decoder, models).map(|(s, _)| s)
}

/// `open`, also reporting each model's load time.
pub fn open_timed(
    family: Family,
    encoder: Backend,
    decoder: Backend,
    models: &Path,
) -> Result<(Box<dyn Segmenter>, LoadTimes), String> {
    let (enc_model, dec_model) = family.models();
    let enc_path = enc_model.verified(models)?;
    let dec_path = dec_model.verified(models)?;
    let (enc, enc_load) = runtime::open(encoder, &enc_path)?;
    let (dec, dec_load) = runtime::open(decoder, &dec_path)?;
    let times = LoadTimes {
        encoder: enc_load,
        decoder: dec_load,
    };
    let segmenter: Box<dyn Segmenter> = match family {
        Family::EfficientVitL0 => Box::new(evit::EfficientVit::new(enc, dec)),
        Family::Sam21Small => Box::new(sam21::Sam21::new(enc, dec)),
    };
    Ok((segmenter, times))
}

/// The encoder's input tensor for an image: resampled, normalized and padded as the family's
/// reference does. `encode` runs this first; it is public so its cost can be measured alone.
pub fn encoder_input(family: Family, image: &Rgb8) -> Tensor {
    let frame = Frame::of(family, (image.width, image.height));
    let (w, h) = frame.content;
    let rgb = resample(image, w, h);
    frame::normalized_nchw(&rgb, frame.content, frame.input)
}

/// Rejects a prompt with neither a point nor a box, or with a non-finite coordinate.
fn check_prompt(p: &Prompt) -> Result<(), String> {
    if p.positive.is_empty() && p.negative.is_empty() && p.bbox.is_none() {
        return Err("a prompt needs at least one point or a box".into());
    }
    let corners = p.bbox.iter().flat_map(|(a, b)| [a, b]);
    if p.positive
        .iter()
        .chain(&p.negative)
        .chain(corners)
        .any(|q| !q.x.is_finite() || !q.y.is_finite())
    {
        return Err("prompt coordinates must be finite".into());
    }
    Ok(())
}

/// A box's top-left and bottom-right corners, whichever way round it was given.
fn box_corners(a: Point, b: Point) -> (Point, Point) {
    (
        Point::new(a.x.min(b.x), a.y.min(b.y)),
        Point::new(a.x.max(b.x), a.y.max(b.y)),
    )
}

/// Rejects an embedding made by another family.
fn check_embedding(e: &Embedding, family: Family) -> Result<(), String> {
    if e.family != family {
        return Err(format!(
            "a {} embedding cannot be decoded by {}",
            e.family.name(),
            family.name()
        ));
    }
    Ok(())
}

/// Splits a decoder's `(B, M, H, W)` masks and `(B, M)` scores into candidates, each prompt's
/// sorted by predicted IoU, highest first.
fn candidates(
    masks: Tensor,
    iou: Tensor,
    frame: &Frame,
    batch: usize,
) -> Result<Vec<Vec<Candidate>>, String> {
    let [b, m, h, w] = masks.shape[..] else {
        return Err(format!("decoder masks have shape {:?}", masks.shape));
    };
    if b != batch || iou.shape != [b, m] {
        return Err(format!(
            "decoder returned masks {:?} and scores {:?} for {batch} prompts",
            masks.shape, iou.shape
        ));
    }
    let data = masks.into_f32()?;
    let scores = iou.into_f32()?;
    let cells = frame.cells_per_px((w, h));
    let per = w * h;
    Ok((0..b)
        .map(|i| {
            let mut out: Vec<Candidate> = (0..m)
                .map(|j| Candidate {
                    logits: Logits {
                        width: w,
                        height: h,
                        data: data[(i * m + j) * per..(i * m + j + 1) * per].to_vec(),
                        source: frame.source,
                        cells_per_px: cells,
                    },
                    predicted_iou: scores[i * m + j],
                })
                .collect();
            out.sort_by(|a, b| b.predicted_iou.total_cmp(&a.predicted_iou));
            out
        })
        .collect())
}

#[cfg(test)]
mod tests;
