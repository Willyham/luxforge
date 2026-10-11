//! EfficientViT-SAM L0: a 512² encoder and SAM 1's prompt decoder, one mask per prompt, prompts
//! batched along the decoder's first dimension. Conventions are in `frame.rs`.

use crate::image::Rgb8;
use crate::runtime::{self, Session};
use crate::tensor::Tensor;

use super::{
    Candidate, Embedding, Family, Frame, Prompt, box_corners, candidates, check_embedding,
    check_prompt, encoder_input,
};

const EMBEDDING: &str = "image_embeddings";
/// Prompts per decoder call; bounds the `(B, 1, 256, 256)` output at 16 MB.
const MAX_BATCH: usize = 64;

pub struct EfficientVit {
    encoder: Box<dyn Session>,
    decoder: Box<dyn Session>,
}

impl EfficientVit {
    pub fn new(encoder: Box<dyn Session>, decoder: Box<dyn Session>) -> Self {
        Self { encoder, decoder }
    }

    /// Decodes prompts that all have `tokens` points in one call.
    fn decode_batch(
        &mut self,
        embedding: &Embedding,
        prompts: &[&Prompt],
        tokens: usize,
    ) -> Result<Vec<Vec<Candidate>>, String> {
        let frame = Frame::of(Family::EfficientVitL0, embedding.source);
        let mut coords = Vec::with_capacity(prompts.len() * tokens * 2);
        let mut labels = Vec::with_capacity(prompts.len() * tokens);
        for p in prompts {
            let (c, l) = prompt_tokens(&frame, p);
            debug_assert_eq!(l.len(), tokens);
            coords.extend(c);
            labels.extend(l);
        }
        let b = prompts.len();
        let coords = Tensor::f32([b, tokens, 2], coords);
        let labels = Tensor::f32([b, tokens], labels);
        let mut out = self.decoder.run(&[
            (EMBEDDING, embedding.tensor(EMBEDDING)?),
            ("point_coords", &coords),
            ("point_labels", &labels),
        ])?;
        let masks = runtime::take(&mut out, "masks")?;
        let iou = runtime::take(&mut out, "iou_predictions")?;
        candidates(masks, iou, &frame, b)
    }
}

/// Points first (positive 1, negative 0), then the box corners (2 and 3), as
/// `SamDecoder.run` concatenates them.
pub(super) fn prompt_tokens(frame: &Frame, p: &Prompt) -> (Vec<f32>, Vec<f32>) {
    let mut coords = Vec::new();
    let mut labels = Vec::new();
    for (points, label) in [(&p.positive, 1.0), (&p.negative, 0.0)] {
        for &q in points {
            coords.extend(frame.prompt_point(q));
            labels.push(label);
        }
    }
    if let Some((a, b)) = p.bbox {
        let (tl, br) = box_corners(a, b);
        coords.extend(frame.prompt_point(tl));
        coords.extend(frame.prompt_point(br));
        labels.extend([2.0, 3.0]);
    }
    (coords, labels)
}

fn token_count(p: &Prompt) -> usize {
    p.positive.len() + p.negative.len() + if p.bbox.is_some() { 2 } else { 0 }
}

impl super::Segmenter for EfficientVit {
    fn family(&self) -> Family {
        Family::EfficientVitL0
    }

    fn encode(&mut self, image: &Rgb8) -> Result<Embedding, String> {
        let input = encoder_input(Family::EfficientVitL0, image);
        let mut out = self.encoder.run(&[("input_image", &input)])?;
        let embedding = runtime::take(&mut out, EMBEDDING)?;
        if embedding.shape != [1, 256, 64, 64] {
            return Err(format!(
                "EfficientViT-SAM encoder returned {:?}, expected [1, 256, 64, 64]",
                embedding.shape
            ));
        }
        Ok(Embedding {
            family: Family::EfficientVitL0,
            source: (image.width, image.height),
            tensors: vec![(EMBEDDING.into(), embedding)],
        })
    }

    fn decode(&mut self, embedding: &Embedding, prompt: &Prompt) -> Result<Vec<Candidate>, String> {
        check_embedding(embedding, Family::EfficientVitL0)?;
        check_prompt(prompt)?;
        let mut out = self.decode_batch(embedding, &[prompt], token_count(prompt))?;
        Ok(out.remove(0))
    }

    /// Batches prompts with the same number of points into one call each. Prompts of different
    /// lengths are not padded together: a padding token changes the decoder's attention, so the
    /// result would differ from decoding the prompt alone.
    fn decode_many(
        &mut self,
        embedding: &Embedding,
        prompts: &[Prompt],
    ) -> Result<Vec<Vec<Candidate>>, String> {
        check_embedding(embedding, Family::EfficientVitL0)?;
        for p in prompts {
            check_prompt(p)?;
        }
        let mut results: Vec<Option<Vec<Candidate>>> = vec![None; prompts.len()];
        let mut lengths: Vec<usize> = prompts.iter().map(token_count).collect();
        lengths.sort_unstable();
        lengths.dedup();
        for tokens in lengths {
            let group: Vec<usize> = (0..prompts.len())
                .filter(|&i| token_count(&prompts[i]) == tokens)
                .collect();
            for chunk in group.chunks(MAX_BATCH) {
                let batch: Vec<&Prompt> = chunk.iter().map(|&i| &prompts[i]).collect();
                let decoded = self.decode_batch(embedding, &batch, tokens)?;
                for (&i, c) in chunk.iter().zip(decoded) {
                    results[i] = Some(c);
                }
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.expect("every prompt is in one group"))
            .collect())
    }
}
