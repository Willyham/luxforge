//! The segmenter's two sessions: the encoder, run once per analysis input, and the prompt decoder,
//! run per prompt batch, each a [`Session`] the host's `local-runtime` slots hold. The tensors are
//! EfficientViT-SAM L0's ONNX export's (`probes/object-removal/src/segment/evit.rs`).
use super::{Candidate, Frame, Logits, Prompt, frame, resample};
use crate::{
    Cancel, Error,
    inference::{Session, Tensor, TensorData},
};

/// The encoder's input and output, and the decoder's inputs and outputs.
pub(crate) const ENCODER_INPUT: &str = "input_image";
pub(crate) const EMBEDDING: &str = "image_embeddings";
pub(crate) const POINT_COORDS: &str = "point_coords";
pub(crate) const POINT_LABELS: &str = "point_labels";
pub(crate) const MASKS: &str = "masks";
pub(crate) const IOU: &str = "iou_predictions";
/// The embedding's shape: 256 channels over a 64² grid.
pub(crate) const EMBEDDING_SHAPE: [u64; 4] = [1, 256, 64, 64];
/// Prompts per decoder call; bounds the `(B, 1, 256, 256)` output at 16 MiB.
const MAX_BATCH: usize = 64;

/// An analysis input's encoder output, reusable for every prompt on that input: what the analysis
/// cache holds for the segmenter. The tensor is held as the decoder takes it, so a decode moves it
/// in and takes it back rather than copying its 4 MiB.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Embedding {
    /// The analysis input's size.
    pub(crate) source: (u32, u32),
    /// The `image_embeddings` tensor of [`EMBEDDING_SHAPE`].
    tensor: Tensor,
}

impl Embedding {
    /// The embedding of an analysis input of `source` size, refused `incompatible` unless it is an
    /// `f32` tensor of [`EMBEDDING_SHAPE`] with finite values.
    pub(crate) fn new(source: (u32, u32), tensor: Tensor) -> Result<Self, Error> {
        let finite = tensor
            .f32()
            .is_some_and(|values| values.iter().all(|value| value.is_finite()));
        if tensor.shape() != EMBEDDING_SHAPE || !finite {
            return Err(Error::incompatible(format!(
                "the segmenter's encoder answered {:?}, not finite f32 {EMBEDDING_SHAPE:?}",
                tensor.shape()
            )));
        }
        let tensor = Tensor::new(EMBEDDING, EMBEDDING_SHAPE.to_vec(), tensor.into_data())?;
        Ok(Self { source, tensor })
    }

    /// Bytes held: four a value.
    pub(crate) fn bytes(&self) -> u64 {
        self.tensor.data().len() as u64 * 4
    }
}

/// The segmenter's two operations over the sessions a task or the hover worker holds.
pub(crate) struct Segmenter;

impl Segmenter {
    /// Run the encoder over an analysis input given as interleaved RGB codes in `0..=255`.
    pub(crate) fn encode(
        encoder: &dyn Session,
        rgb: &[f32],
        source: (u32, u32),
        cancel: &Cancel,
    ) -> Result<Embedding, Error> {
        let frame = Frame::of(source);
        let content = resample(rgb, source, frame.content);
        let side = u64::from(frame::ENCODER_SIDE);
        let input = Tensor::new(
            ENCODER_INPUT,
            vec![1, 3, side, side],
            TensorData::F32(frame::normalized_nchw(
                &content,
                frame.content,
                frame::ENCODER_SIDE,
            )),
        )?;
        let outputs = encoder.run(&[input], cancel)?;
        Embedding::new(source, take(outputs, EMBEDDING)?)
    }

    /// Decode prompts against an embedding, batching those with the same number of tokens into
    /// one call each: a padding token changes the decoder's attention, so prompts of different
    /// lengths are never padded together. Answers one candidate list per prompt, in order, each
    /// highest predicted IoU first.
    pub(crate) fn decode_many(
        decoder: &dyn Session,
        embedding: &mut Embedding,
        prompts: &[Prompt],
        cancel: &Cancel,
    ) -> Result<Vec<Vec<Candidate>>, Error> {
        for prompt in prompts {
            check_prompt(prompt)?;
        }
        let mut results: Vec<Option<Vec<Candidate>>> = vec![None; prompts.len()];
        let mut lengths: Vec<usize> = prompts.iter().map(Prompt::tokens).collect();
        lengths.sort_unstable();
        lengths.dedup();
        for tokens in lengths {
            let group: Vec<usize> = (0..prompts.len())
                .filter(|&i| prompts[i].tokens() == tokens)
                .collect();
            for chunk in group.chunks(MAX_BATCH) {
                cancel.check()?;
                let batch: Vec<&Prompt> = chunk.iter().map(|&i| &prompts[i]).collect();
                let decoded = decode_batch(decoder, embedding, &batch, tokens, cancel)?;
                for (&i, candidates) in chunk.iter().zip(decoded) {
                    results[i] = Some(candidates);
                }
            }
        }
        Ok(results.into_iter().map(Option::unwrap_or_default).collect())
    }
}

/// The named output of `outputs`, or `incompatible` naming it.
fn take(outputs: Vec<Tensor>, name: &str) -> Result<Tensor, Error> {
    outputs
        .into_iter()
        .find(|tensor| tensor.name() == name)
        .ok_or_else(|| Error::incompatible(format!("the segmenter answered no {name}")))
}

/// A prompt with neither a point nor a box, or with a non-finite coordinate, is refused.
fn check_prompt(p: &Prompt) -> Result<(), Error> {
    if p.positive.is_empty() && p.negative.is_empty() && p.bbox.is_none() {
        return Err(Error::validation(
            "a prompt needs at least one point or a box",
        ));
    }
    let corners = p.bbox.iter().flat_map(|(a, b)| [a, b]);
    if p.positive
        .iter()
        .chain(&p.negative)
        .chain(corners)
        .any(|q| !q.x.is_finite() || !q.y.is_finite())
    {
        return Err(Error::validation("prompt coordinates must be finite"));
    }
    Ok(())
}

/// Points first (positive 1, negative 0), then the box corners (2 and 3), as the reference's
/// `SamDecoder.run` concatenates them.
pub(crate) fn prompt_tokens(frame: &Frame, p: &Prompt) -> (Vec<f32>, Vec<f32>) {
    let mut coords = Vec::new();
    let mut labels = Vec::new();
    for (points, label) in [(&p.positive, 1.0), (&p.negative, 0.0)] {
        for &q in points {
            coords.extend(frame.prompt_point(q));
            labels.push(label);
        }
    }
    if let Some((a, b)) = p.bbox {
        let tl = super::Point::new(a.x.min(b.x), a.y.min(b.y));
        let br = super::Point::new(a.x.max(b.x), a.y.max(b.y));
        coords.extend(frame.prompt_point(tl));
        coords.extend(frame.prompt_point(br));
        labels.extend([2.0, 3.0]);
    }
    (coords, labels)
}

fn decode_batch(
    decoder: &dyn Session,
    embedding: &mut Embedding,
    prompts: &[&Prompt],
    tokens: usize,
    cancel: &Cancel,
) -> Result<Vec<Vec<Candidate>>, Error> {
    let source = embedding.source;
    let frame = Frame::of(source);
    let mut coords = Vec::with_capacity(prompts.len() * tokens * 2);
    let mut labels = Vec::with_capacity(prompts.len() * tokens);
    for p in prompts {
        let (c, l) = prompt_tokens(&frame, p);
        coords.extend(c);
        labels.extend(l);
    }
    let b = prompts.len() as u64;
    let coords = Tensor::new(
        POINT_COORDS,
        vec![b, tokens as u64, 2],
        TensorData::F32(coords),
    )?;
    let labels = Tensor::new(
        POINT_LABELS,
        vec![b, tokens as u64],
        TensorData::F32(labels),
    )?;
    // The embedding is moved in and taken back whatever the run answers.
    let placeholder = Tensor::new(EMBEDDING, vec![0], TensorData::F32(Vec::new()))?;
    let image = std::mem::replace(&mut embedding.tensor, placeholder);
    let inputs = [image, coords, labels];
    let answered = decoder.run(&inputs, cancel);
    let [image, _, _] = inputs;
    embedding.tensor = image;
    let mut outputs = answered?;
    let masks_at = outputs
        .iter()
        .position(|tensor| tensor.name() == MASKS)
        .ok_or_else(|| Error::incompatible(format!("the segmenter answered no {MASKS}")))?;
    let masks = outputs.swap_remove(masks_at);
    let iou = take(outputs, IOU)?;
    let [batch, m, h, w] = masks.shape()[..] else {
        return Err(Error::incompatible(format!(
            "the segmenter's masks have shape {:?}",
            masks.shape()
        )));
    };
    if batch != b || iou.shape() != [b, m] || m == 0 || h == 0 || w == 0 {
        return Err(Error::incompatible(format!(
            "the segmenter answered masks {:?} and scores {:?} for {b} prompts",
            masks.shape(),
            iou.shape()
        )));
    }
    let (Some(data), Some(scores)) = (masks.f32(), iou.f32()) else {
        return Err(Error::incompatible("the segmenter answered no f32 masks"));
    };
    if !data.iter().chain(scores).all(|value| value.is_finite()) {
        return Err(Error::incompatible(
            "the segmenter answered a non-finite mask or score",
        ));
    }
    let (m, h, w) = (m as usize, h as usize, w as usize);
    let cells = frame.cells_per_px((w, h));
    let per = w * h;
    Ok((0..prompts.len())
        .map(|i| {
            let mut out: Vec<Candidate> = (0..m)
                .map(|j| Candidate {
                    logits: Logits {
                        width: w,
                        height: h,
                        data: data[(i * m + j) * per..(i * m + j + 1) * per].to_vec(),
                        source,
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
