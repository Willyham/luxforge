//! Where source pixels land in each family's encoder input, prompt frame and logits.
//!
//! EfficientViT-SAM L0, following mit-han-lab/efficientvit (`applications/efficientvit_sam/
//! run_efficientvit_sam_onnx.py`, `deployment/onnx/export_{encoder,decoder}.py` and
//! `efficientvit/models/efficientvit/sam.py`, where the L models have `image_size = (1024, 512)`):
//! - the encoder sees the image with its long side resized to 512 (`SamResize`: antialiased
//!   bilinear, size from `get_preprocess_shape`, rounding half up), scaled to [0, 1], normalized
//!   with ImageNet mean and standard deviation, and zero-padded bottom and right to 512²;
//! - prompts live in the long-side-**1024** frame: `x · new_w / w` and `y · new_h / h` with
//!   `(new_h, new_w) = get_preprocess_shape(h, w, 1024)`; the decoder adds half a pixel and
//!   divides by 1024; points are labelled 1 (positive) and 0 (negative), a box is its two corners
//!   labelled 2 and 3 and follows the points; prompts batch along the first dimension;
//! - the decoder returns one 256² low-resolution mask per prompt, over the padded 1024² frame:
//!   the export (`return_single_mask`) keeps the best of SAM's four mask tokens by predicted IoU.
//!
//! SAM 2.1 Small, as exported by darktable-ai (`models/mask-object-sam21-small/convert.py`,
//! release-5.6.0) and following SAM 2's `SAM2Transforms` and `SAM2ImagePredictor`:
//! - the encoder sees the image squashed to exactly 1024² (`Resize((1024, 1024))`, antialiased
//!   bilinear), scaled to [0, 1] and normalized with ImageNet mean and standard deviation;
//! - prompts are `x / w · 1024` and `y / h · 1024`; the decoder adds half a pixel, appends SAM 2's
//!   padding point (label −1) itself and divides by 1024; a box is its two corners labelled 2
//!   and 3 and precedes the points; the batch dimension is fixed at 1;
//! - `mask_input` is a previous 256² logit mask and `has_mask_input` is 1 when it is used;
//! - the decoder returns SAM 2's three multimask outputs (`masks[:, 1:]`, so none is the
//!   single-mask token), 256² over the squashed frame, clamped to ±32.
//!
//! darktable 5.6's own driver (`src/common/ai/segmentation.c`) letterboxes this model like SAM 1
//! (long side 1024, zero padding) and appends a second padding point; the probe follows SAM 2's
//! and darktable-ai's own squash, which is how the model was trained.
//!
//! The probe's points are continuous (pixel `i` spans `[i, i + 1)`), while the references pass
//! pixel indices that the decoder shifts by half a pixel. A source position `p` therefore becomes
//! the prompt coordinate `p · scale − 0.5`, which puts it exactly at `p · scale` in the model.

use crate::mask::Point;
use crate::tensor::Tensor;

use super::Family;

/// ImageNet mean and standard deviation in 8-bit codes (`[0.485, 0.456, 0.406] · 255` and
/// `[0.229, 0.224, 0.225] · 255`), as both references normalize.
const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
const STD: [f32; 3] = [58.395, 57.12, 57.375];

/// The side of both families' square prompt frame.
pub const PROMPT_SIDE: f32 = 1024.0;

/// How a family maps one source size onto its model frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub family: Family,
    /// The source image's size.
    pub source: (u32, u32),
    /// The resampled image's size inside the encoder input, anchored top-left.
    pub content: (u32, u32),
    /// The encoder input's square side; the rest of the input is zero padding.
    pub input: u32,
    /// Prompt-frame pixels per source pixel along x and y.
    pub prompt_scale: (f32, f32),
}

impl Frame {
    pub fn of(family: Family, source: (u32, u32)) -> Frame {
        let (w, h) = source;
        match family {
            Family::EfficientVitL0 => {
                let (pw, ph) = long_side_size(source, 1024);
                Frame {
                    family,
                    source,
                    content: long_side_size(source, 512),
                    input: 512,
                    prompt_scale: (pw as f32 / w as f32, ph as f32 / h as f32),
                }
            }
            Family::Sam21Small => Frame {
                family,
                source,
                content: (1024, 1024),
                input: 1024,
                prompt_scale: (PROMPT_SIDE / w as f32, PROMPT_SIDE / h as f32),
            },
        }
    }

    /// A source position as the decoder's prompt coordinate.
    pub fn prompt_point(&self, p: Point) -> [f32; 2] {
        [
            p.x * self.prompt_scale.0 - 0.5,
            p.y * self.prompt_scale.1 - 0.5,
        ]
    }

    /// Logit cells per source pixel for a low-resolution mask of `size` cells over the square
    /// prompt frame.
    pub fn cells_per_px(&self, size: (usize, usize)) -> (f32, f32) {
        (
            self.prompt_scale.0 * size.0 as f32 / PROMPT_SIDE,
            self.prompt_scale.1 * size.1 as f32 / PROMPT_SIDE,
        )
    }
}

/// SAM's `get_preprocess_shape`: the long side becomes `long`, each side rounding half up.
fn long_side_size((w, h): (u32, u32), long: u32) -> (u32, u32) {
    let scale = long as f64 / w.max(h) as f64;
    let side = |s: u32| ((s as f64 * scale + 0.5) as u32).max(1);
    (side(w), side(h))
}

/// Interleaved RGB codes in `0..=255` of `content` size to a normalized `(1, 3, side, side)`
/// tensor, zero outside the content (padding after normalization, as both references pad).
pub fn normalized_nchw(rgb: &[f32], content: (u32, u32), side: u32) -> Tensor {
    let (w, h, s) = (content.0 as usize, content.1 as usize, side as usize);
    assert!(w <= s && h <= s, "content larger than the encoder input");
    assert_eq!(rgb.len(), w * h * 3, "content size and buffer disagree");
    let mut out = vec![0.0f32; 3 * s * s];
    for (c, plane) in out.chunks_exact_mut(s * s).enumerate() {
        for y in 0..h {
            let src = &rgb[y * w * 3..(y + 1) * w * 3];
            for (x, v) in plane[y * s..y * s + w].iter_mut().enumerate() {
                *v = (src[x * 3 + c] - MEAN[c]) / STD[c];
            }
        }
    }
    Tensor::f32([1, 3, s, s], out)
}
