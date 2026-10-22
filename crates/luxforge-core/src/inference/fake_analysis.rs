//! The fake runtime's analysis models: deterministic stand-ins for the segmenter's encoder and
//! decoder and for the scene and depth models, so the analysis tasks, the picker and the selection
//! task run end to end over [`super::fake::FakeInference`] in tests and developer runs.
//!
//! A model file whose first line is `LFFAKE1 <kind>` opens as that kind's session; any other file
//! opens as the generic fake session. Each kind answers the tensors the `luxforge.ai` module sends
//! (`crate::modules::ai`): the segmenter's are EfficientViT-SAM L0's, the scene and depth models'
//! are the contract the module reads until the qualified models replace the fakes.
//!
//! - **Segmenter encoder** (`input_image` `[1, 3, 512, 512]` → `image_embeddings`
//!   `[1, 256, 64, 64]`): channels 0 to 2 of each embedding cell are the mean of its 8 × 8 block of
//!   the normalized input; the rest are zero.
//! - **Segmenter decoder** (`image_embeddings`, `point_coords` `[B, N, 2]`, `point_labels` `[B, N]`
//!   → `masks` `[B, 3, 256, 256]`, `iou_predictions` `[B, 3]`): a colour-similarity segmentation.
//!   A prompt's reference colour is the mean embedding colour under its positive points, or under
//!   its box's centre without one; candidate `k` is inside where the colour lies within
//!   [`TOLERANCES`]`[k]` of it, outside any box grown by one cell, and nearer the reference than
//!   every negative point's colour. Logits are `±8` scaled by the margin, clamped to `±20`;
//!   predicted IoUs are [`IOUS`].
//! - **Semantic** (`pixel_values` `[1, 3, h, w]` → `class_logits` `[1, 4, ⌈h/8⌉, ⌈w/8⌉]`): per 8 × 8
//!   cell, sky where blue leads, vegetation where green leads, person where red leads, and a
//!   constant `other`.
//! - **Subject** (`pixel_values` → `saliency` `[1, 1, ⌈h/8⌉, ⌈w/8⌉]`): how far each cell's colour is
//!   from the frame's mean, through a logistic.
//! - **People** (`pixel_values` → `boxes` `[1, P, 4]`, `scores` `[1, P]`): one person at the bounds
//!   of the cells where red leads, when they are at least 1 % of the frame; none otherwise.
//! - **Depth** (`pixel_values` → `depth` `[1, 1, ⌈h/8⌉, ⌈w/8⌉]`): nearer toward the bottom of the
//!   frame, lifted slightly by luminance.
use super::{ElementType, Tensor, TensorData, TensorSpec};
use crate::{Cancel, Error};
use std::{fs::File, io::Read, path::Path};

/// The first line's prefix that names a fake analysis model's kind.
pub const FAKE_ANALYSIS_MAGIC: &str = "LFFAKE1 ";

/// The decoder's candidates' colour tolerances, in normalized units, tightest first.
pub const TOLERANCES: [f32; 3] = [0.25, 0.5, 0.9];
/// The decoder's predicted IoU of each candidate.
pub const IOUS: [f32; 3] = [0.93, 0.9, 0.82];
/// The cell side the scene and depth models answer at, in input pixels.
const CELL: usize = 8;
/// The embedding grid's side and the decoder's mask side.
const GRID: usize = 64;
const MASK: usize = 256;
const CHANNELS: usize = 256;

/// A fake analysis model's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    SegmenterEncoder,
    SegmenterDecoder,
    Semantic,
    Subject,
    People,
    Depth,
}

impl Kind {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "segmenter-encoder" => Self::SegmenterEncoder,
            "segmenter-decoder" => Self::SegmenterDecoder,
            "semantic" => Self::Semantic,
            "subject" => Self::Subject,
            "people" => Self::People,
            "depth" => Self::Depth,
            _ => return None,
        })
    }

    /// The kind a model file's first line names, reading at most 64 bytes; `None` for any other
    /// file.
    pub(super) fn of(path: &Path) -> Option<Self> {
        let mut head = [0_u8; 64];
        let mut file = File::open(path).ok()?;
        let mut filled = 0;
        while filled < head.len() {
            match file.read(&mut head[filled..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => filled += n,
            }
        }
        let line = std::str::from_utf8(&head[..filled]).ok()?.lines().next()?;
        Self::named(line.strip_prefix(FAKE_ANALYSIS_MAGIC)?)
    }

    /// The tensors it takes and answers.
    pub(super) fn specs(self) -> (Vec<TensorSpec>, Vec<TensorSpec>) {
        let spec = |name: &str, shape: Vec<Option<u64>>| TensorSpec {
            name: name.into(),
            element: ElementType::F32,
            shape,
        };
        let free = |rank: usize| vec![None; rank];
        let scene = vec![spec("pixel_values", free(4))];
        match self {
            Self::SegmenterEncoder => (
                vec![spec(
                    "input_image",
                    vec![Some(1), Some(3), Some(512), Some(512)],
                )],
                vec![spec(
                    "image_embeddings",
                    vec![Some(1), Some(256), Some(64), Some(64)],
                )],
            ),
            Self::SegmenterDecoder => (
                vec![
                    spec(
                        "image_embeddings",
                        vec![Some(1), Some(256), Some(64), Some(64)],
                    ),
                    spec("point_coords", vec![None, None, Some(2)]),
                    spec("point_labels", free(2)),
                ],
                vec![
                    spec("masks", vec![None, Some(3), Some(256), Some(256)]),
                    spec("iou_predictions", vec![None, Some(3)]),
                ],
            ),
            Self::Semantic => (scene, vec![spec("class_logits", free(4))]),
            Self::Subject => (scene, vec![spec("saliency", free(4))]),
            Self::People => (scene, vec![spec("boxes", free(3)), spec("scores", free(2))]),
            Self::Depth => (scene, vec![spec("depth", free(4))]),
        }
    }

    /// Answer `inputs`, which the session has checked against [`Self::specs`].
    pub(super) fn run(self, inputs: &[Tensor], cancel: &Cancel) -> Result<Vec<Tensor>, Error> {
        cancel.check()?;
        match self {
            Self::SegmenterEncoder => encode(&inputs[0]),
            Self::SegmenterDecoder => decode(&inputs[0], &inputs[1], &inputs[2], cancel),
            Self::Semantic | Self::Subject | Self::People | Self::Depth => {
                scene(self, &inputs[0], cancel)
            }
        }
    }
}

fn values(tensor: &Tensor) -> &[f32] {
    tensor.f32().unwrap_or_default()
}

fn encode(input: &Tensor) -> Result<Vec<Tensor>, Error> {
    let image = values(input);
    let side = 512;
    let mut embedding = vec![0.0_f32; CHANNELS * GRID * GRID];
    for c in 0..3 {
        let plane = &image[c * side * side..(c + 1) * side * side];
        for v in 0..GRID {
            for u in 0..GRID {
                let mut sum = 0.0_f32;
                for y in v * 8..v * 8 + 8 {
                    sum += plane[y * side + u * 8..y * side + u * 8 + 8]
                        .iter()
                        .sum::<f32>();
                }
                embedding[c * GRID * GRID + v * GRID + u] = sum / 64.0;
            }
        }
    }
    Ok(vec![Tensor::new(
        "image_embeddings",
        vec![1, CHANNELS as u64, GRID as u64, GRID as u64],
        TensorData::F32(embedding),
    )?])
}

/// The embedding's colour at a prompt-frame position.
fn colour_at(embedding: &[f32], x: f32, y: f32) -> [f32; 3] {
    let cell = |p: f32| (((p + 0.5) / 16.0).floor().max(0.0) as usize).min(GRID - 1);
    let (u, v) = (cell(x), cell(y));
    std::array::from_fn(|c| embedding[c * GRID * GRID + v * GRID + u])
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b) * (a - b))
        .sum::<f32>()
        .sqrt()
}

fn decode(
    embedding: &Tensor,
    coords: &Tensor,
    labels: &Tensor,
    cancel: &Cancel,
) -> Result<Vec<Tensor>, Error> {
    let (batch, tokens) = (coords.shape()[0] as usize, coords.shape()[1] as usize);
    if labels.shape() != [batch as u64, tokens as u64] {
        return Err(Error::validation(format!(
            "point_labels {:?} do not match point_coords {:?}",
            labels.shape(),
            coords.shape()
        )));
    }
    let (embedding, coords, labels) = (values(embedding), values(coords), values(labels));
    let candidates = TOLERANCES.len();
    let mut masks = Vec::with_capacity(batch * candidates * MASK * MASK);
    let mut ious = Vec::with_capacity(batch * candidates);
    for b in 0..batch {
        cancel.check()?;
        let point = |t: usize| {
            (
                coords[(b * tokens + t) * 2],
                coords[(b * tokens + t) * 2 + 1],
            )
        };
        let label = |t: usize| labels[b * tokens + t];
        let positives: Vec<[f32; 3]> = (0..tokens)
            .filter(|&t| label(t) == 1.0)
            .map(|t| colour_at(embedding, point(t).0, point(t).1))
            .collect();
        let negatives: Vec<[f32; 3]> = (0..tokens)
            .filter(|&t| label(t) == 0.0)
            .map(|t| colour_at(embedding, point(t).0, point(t).1))
            .collect();
        let corner = |l: f32| (0..tokens).find(|&t| label(t) == l).map(point);
        let bbox = corner(2.0).zip(corner(3.0));
        let reference = if positives.is_empty() {
            match bbox {
                Some((a, z)) => colour_at(embedding, (a.0 + z.0) / 2.0, (a.1 + z.1) / 2.0),
                None => [f32::INFINITY; 3],
            }
        } else {
            let n = positives.len() as f32;
            std::array::from_fn(|c| positives.iter().map(|p| p[c]).sum::<f32>() / n)
        };
        for tolerance in TOLERANCES {
            for v in 0..MASK {
                for u in 0..MASK {
                    // A mask cell spans 4 prompt-frame pixels, a quarter of an embedding cell.
                    let (x, y) = (u as f32 * 4.0 + 1.5, v as f32 * 4.0 + 1.5);
                    let colour = colour_at(embedding, x, y);
                    let near = distance(colour, reference);
                    let outside_box = bbox.is_some_and(|(a, z)| {
                        let grow = 16.0;
                        x < a.0.min(z.0) - grow
                            || x > a.0.max(z.0) + grow
                            || y < a.1.min(z.1) - grow
                            || y > a.1.max(z.1) + grow
                    });
                    let rejected = negatives.iter().any(|n| distance(colour, *n) < near);
                    let logit = if !near.is_finite() || outside_box || rejected {
                        -8.0
                    } else {
                        (8.0 * (tolerance - near) / tolerance).clamp(-20.0, 20.0)
                    };
                    masks.push(logit);
                }
            }
        }
        ious.extend(IOUS);
    }
    Ok(vec![
        Tensor::new(
            "masks",
            vec![batch as u64, candidates as u64, MASK as u64, MASK as u64],
            TensorData::F32(masks),
        )?,
        Tensor::new(
            "iou_predictions",
            vec![batch as u64, candidates as u64],
            TensorData::F32(ious),
        )?,
    ])
}

/// Each 8 × 8 cell's mean colour of a `[1, 3, h, w]` input, with the grid's size.
fn cells(input: &Tensor, cancel: &Cancel) -> Result<((usize, usize), Vec<[f32; 3]>), Error> {
    let shape = input.shape();
    if shape.len() != 4 || shape[0] != 1 || shape[1] != 3 || shape[2] == 0 || shape[3] == 0 {
        return Err(Error::validation(format!(
            "pixel_values is [1, 3, h, w], not {shape:?}"
        )));
    }
    let (h, w) = (shape[2] as usize, shape[3] as usize);
    let (ch, cw) = (h.div_ceil(CELL), w.div_ceil(CELL));
    let image = values(input);
    let mut out = Vec::with_capacity(ch * cw);
    for v in 0..ch {
        cancel.check()?;
        for u in 0..cw {
            let (y0, y1) = (v * CELL, (v * CELL + CELL).min(h));
            let (x0, x1) = (u * CELL, (u * CELL + CELL).min(w));
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            out.push(std::array::from_fn(|c| {
                let mut sum = 0.0;
                for y in y0..y1 {
                    sum += image[c * h * w + y * w + x0..c * h * w + y * w + x1]
                        .iter()
                        .sum::<f32>();
                }
                sum / n
            }));
        }
    }
    Ok(((cw, ch), out))
}

/// Whether red leads a colour: the fake's "person".
fn reddish(c: [f32; 3]) -> bool {
    c[0] - c[1].max(c[2]) > 0.1
}

fn scene(kind: Kind, input: &Tensor, cancel: &Cancel) -> Result<Vec<Tensor>, Error> {
    let ((cw, ch), cells) = cells(input, cancel)?;
    let grid = |name: &str, planes: usize, data: Vec<f32>| {
        Tensor::new(
            name,
            vec![1, planes as u64, ch as u64, cw as u64],
            TensorData::F32(data),
        )
    };
    match kind {
        Kind::Semantic => {
            let n = cells.len();
            let mut logits = vec![0.0_f32; 4 * n];
            for (i, c) in cells.iter().enumerate() {
                logits[i] = 10.0 * (c[2] - c[0].max(c[1]));
                logits[n + i] = 10.0 * (c[1] - c[0].max(c[2]));
                logits[2 * n + i] = 10.0 * (c[0] - c[1].max(c[2]));
                logits[3 * n + i] = 0.5;
            }
            Ok(vec![grid("class_logits", 4, logits)?])
        }
        Kind::Subject => {
            let n = cells.len() as f32;
            let mean: [f32; 3] =
                std::array::from_fn(|k| cells.iter().map(|c| c[k]).sum::<f32>() / n);
            let saliency = cells
                .iter()
                .map(|c| 1.0 / (1.0 + (-12.0 * (distance(*c, mean) - 0.25)).exp()))
                .collect();
            Ok(vec![grid("saliency", 1, saliency)?])
        }
        Kind::People => {
            let mut bounds: Option<[usize; 4]> = None;
            let mut count = 0;
            for (i, c) in cells.iter().enumerate() {
                if reddish(*c) {
                    let (u, v) = (i % cw, i / cw);
                    count += 1;
                    bounds = Some(match bounds {
                        None => [u, v, u + 1, v + 1],
                        Some([x0, y0, x1, y1]) => {
                            [x0.min(u), y0.min(v), x1.max(u + 1), y1.max(v + 1)]
                        }
                    });
                }
            }
            let found = bounds.filter(|_| count * 100 >= cells.len());
            let (boxes, scores) = match found {
                Some([x0, y0, x1, y1]) => (
                    vec![
                        x0 as f32 / cw as f32,
                        y0 as f32 / ch as f32,
                        x1 as f32 / cw as f32,
                        y1 as f32 / ch as f32,
                    ],
                    vec![0.9],
                ),
                None => (Vec::new(), Vec::new()),
            };
            let people = scores.len() as u64;
            Ok(vec![
                Tensor::new("boxes", vec![1, people, 4], TensorData::F32(boxes))?,
                Tensor::new("scores", vec![1, people], TensorData::F32(scores))?,
            ])
        }
        Kind::Depth => {
            let depth = cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let row = (i / cw) as f32 + 0.5;
                    let luminance = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                    (0.9 * row / ch as f32 + 0.1 * luminance).clamp(0.0, 1.0)
                })
                .collect();
            Ok(vec![grid("depth", 1, depth)?])
        }
        Kind::SegmenterEncoder | Kind::SegmenterDecoder => {
            Err(Error::internal("a segmenter is not a scene model"))
        }
    }
}
