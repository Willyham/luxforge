//! SAM 2.1 Small as exported by darktable-ai: a 1024² Hiera encoder with two high-resolution
//! feature maps, and a decoder returning SAM 2's three multimask outputs for one prompt per call.
//! Conventions are in `frame.rs`.

use crate::image::Rgb8;
use crate::mask::{Logits, Point};
use crate::runtime::{self, Session};
use crate::tensor::Tensor;

use super::{
    Candidate, Embedding, Family, Frame, Prompt, box_corners, candidates, check_embedding,
    check_prompt, encoder_input,
};

/// The encoder's outputs, which the decoder takes by the same names.
const FEATURES: [&str; 3] = ["image_embed", "high_res_feats_0", "high_res_feats_1"];
/// `mask_input`'s side: the decoder's own low-resolution mask size.
const MASK_SIDE: usize = 256;

pub struct Sam21 {
    encoder: Box<dyn Session>,
    decoder: Box<dyn Session>,
}

impl Sam21 {
    pub fn new(encoder: Box<dyn Session>, decoder: Box<dyn Session>) -> Self {
        Self { encoder, decoder }
    }
}

/// Box corners first (labels 2 and 3), then points (positive 1, negative 0), as
/// `SAM2ImagePredictor._predict` concatenates them. No padding point: the export appends it.
pub(super) fn prompt_tokens(frame: &Frame, p: &Prompt) -> (Vec<f32>, Vec<f32>) {
    let mut coords = Vec::new();
    let mut labels = Vec::new();
    if let Some((a, b)) = p.bbox {
        let (tl, br) = box_corners(a, b);
        coords.extend(frame.prompt_point(tl));
        coords.extend(frame.prompt_point(br));
        labels.extend([2.0, 3.0]);
    }
    for (points, label) in [(&p.positive, 1.0), (&p.negative, 0.0)] {
        for &q in points {
            coords.extend(frame.prompt_point(q));
            labels.push(label);
        }
    }
    (coords, labels)
}

/// A previous result as `mask_input`: its logits on this decoder's 256² grid. Logits already on
/// that grid pass through unchanged; others are resampled at each cell's centre.
pub(super) fn mask_input(previous: &Logits, frame: &Frame) -> Result<Vec<f32>, String> {
    if previous.source != frame.source {
        return Err(format!(
            "the previous mask describes a {}×{} image, not this {}×{} one",
            previous.source.0, previous.source.1, frame.source.0, frame.source.1
        ));
    }
    let cells = frame.cells_per_px((MASK_SIDE, MASK_SIDE));
    if previous.width == MASK_SIDE && previous.height == MASK_SIDE && previous.cells_per_px == cells
    {
        return Ok(previous.data.clone());
    }
    let mut out = Vec::with_capacity(MASK_SIDE * MASK_SIDE);
    for v in 0..MASK_SIDE {
        for u in 0..MASK_SIDE {
            let p = Point::new((u as f32 + 0.5) / cells.0, (v as f32 + 0.5) / cells.1);
            out.push(previous.sample(p));
        }
    }
    Ok(out)
}

impl super::Segmenter for Sam21 {
    fn family(&self) -> Family {
        Family::Sam21Small
    }

    fn encode(&mut self, image: &Rgb8) -> Result<Embedding, String> {
        let input = encoder_input(Family::Sam21Small, image);
        let mut out = self.encoder.run(&[("image", &input)])?;
        let mut tensors = Vec::with_capacity(FEATURES.len());
        for (name, shape) in
            FEATURES
                .iter()
                .zip([[1, 256, 64, 64], [1, 32, 256, 256], [1, 64, 128, 128]])
        {
            let t = runtime::take(&mut out, name)?;
            if t.shape != shape {
                return Err(format!(
                    "SAM 2.1 encoder returned {name} {:?}, expected {shape:?}",
                    t.shape
                ));
            }
            tensors.push((name.to_string(), t));
        }
        Ok(Embedding {
            family: Family::Sam21Small,
            source: (image.width, image.height),
            tensors,
        })
    }

    fn decode(&mut self, embedding: &Embedding, prompt: &Prompt) -> Result<Vec<Candidate>, String> {
        check_embedding(embedding, Family::Sam21Small)?;
        check_prompt(prompt)?;
        let frame = Frame::of(Family::Sam21Small, embedding.source);
        let (coords, labels) = prompt_tokens(&frame, prompt);
        let n = labels.len();
        let coords = Tensor::f32([1, n, 2], coords);
        let labels = Tensor::f32([1, n], labels);
        let (mask, has_mask) = match &prompt.previous {
            Some(previous) => (mask_input(previous, &frame)?, 1.0),
            None => (vec![0.0; MASK_SIDE * MASK_SIDE], 0.0),
        };
        let mask = Tensor::f32([1, 1, MASK_SIDE, MASK_SIDE], mask);
        let has_mask = Tensor::f32([1], vec![has_mask]);
        let mut out = self.decoder.run(&[
            (FEATURES[0], embedding.tensor(FEATURES[0])?),
            (FEATURES[1], embedding.tensor(FEATURES[1])?),
            (FEATURES[2], embedding.tensor(FEATURES[2])?),
            ("point_coords", &coords),
            ("point_labels", &labels),
            ("mask_input", &mask),
            ("has_mask_input", &has_mask),
        ])?;
        let masks = runtime::take(&mut out, "masks")?;
        let iou = runtime::take(&mut out, "iou_predictions")?;
        Ok(candidates(masks, iou, &frame, 1)?.remove(0))
    }
}
