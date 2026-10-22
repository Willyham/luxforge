//! Where analysis pixels land in the segmenter's encoder input, prompt frame and logits.
//!
//! EfficientViT-SAM L0, following mit-han-lab/efficientvit (`applications/efficientvit_sam/
//! run_efficientvit_sam_onnx.py`, `deployment/onnx/export_{encoder,decoder}.py` and
//! `efficientvit/models/efficientvit/sam.py`, where the L models have `image_size = (1024, 512)`),
//! as the prototype transcribed it (`probes/object-removal/src/segment/frame.rs`):
//!
//! - the encoder sees the image with its long side resized to 512 (antialiased bilinear, sizes from
//!   `get_preprocess_shape`, rounding half up), scaled to `[0, 1]`, normalized with ImageNet mean
//!   and standard deviation, and zero-padded bottom and right to 512²;
//! - prompts live in the long-side-**1024** frame: `x · new_w / w` and `y · new_h / h`; the decoder
//!   adds half a pixel and divides by 1024; points are labelled 1 (positive) and 0 (negative), a
//!   box is its two corners labelled 2 and 3 and follows the points; prompts batch along the first
//!   dimension;
//! - the decoder answers one 256² low-resolution mask per prompt over the padded 1024² frame.
//!
//! A continuous position `p` becomes the prompt coordinate `p · scale − 0.5`, which puts it exactly
//! at `p · scale` in the model once the decoder adds its half pixel.
use super::Point;

/// ImageNet mean and standard deviation in 8-bit codes.
const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
const STD: [f32; 3] = [58.395, 57.12, 57.375];

/// The side of the square prompt frame.
pub(crate) const PROMPT_SIDE: f32 = 1024.0;
/// The encoder input's square side.
pub(crate) const ENCODER_SIDE: u32 = 512;

/// How one analysis-input size maps onto the segmenter's frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    /// The analysis input's size.
    pub(crate) source: (u32, u32),
    /// The resampled image's size inside the encoder input, anchored top-left.
    pub(crate) content: (u32, u32),
    /// Prompt-frame pixels per source pixel along x and y.
    pub(crate) prompt_scale: (f32, f32),
}

impl Frame {
    pub(crate) fn of(source: (u32, u32)) -> Self {
        let (w, h) = source;
        let (pw, ph) = long_side_size(source, PROMPT_SIDE as u32);
        Self {
            source,
            content: long_side_size(source, ENCODER_SIDE),
            prompt_scale: (pw as f32 / w as f32, ph as f32 / h as f32),
        }
    }

    /// A source position as the decoder's prompt coordinate.
    pub(crate) fn prompt_point(&self, p: Point) -> [f32; 2] {
        [
            p.x * self.prompt_scale.0 - 0.5,
            p.y * self.prompt_scale.1 - 0.5,
        ]
    }

    /// Logit cells per source pixel for a low-resolution mask of `size` cells over the square
    /// prompt frame.
    pub(crate) fn cells_per_px(&self, size: (usize, usize)) -> (f32, f32) {
        (
            self.prompt_scale.0 * size.0 as f32 / PROMPT_SIDE,
            self.prompt_scale.1 * size.1 as f32 / PROMPT_SIDE,
        )
    }
}

/// SAM's `get_preprocess_shape`: the long side becomes `long`, each side rounding half up.
pub(crate) fn long_side_size((w, h): (u32, u32), long: u32) -> (u32, u32) {
    let scale = f64::from(long) / f64::from(w.max(h));
    let side = |s: u32| ((f64::from(s) * scale + 0.5) as u32).max(1);
    (side(w), side(h))
}

/// Interleaved RGB codes in `0..=255` of `content` size as a normalized `(1, 3, side, side)` plane
/// set, zero outside the content (padding after normalization, as the reference pads).
pub(crate) fn normalized_nchw(rgb: &[f32], content: (u32, u32), side: u32) -> Vec<f32> {
    let (w, h, s) = (content.0 as usize, content.1 as usize, side as usize);
    debug_assert!(w <= s && h <= s, "content larger than the encoder input");
    debug_assert_eq!(rgb.len(), w * h * 3, "content size and buffer disagree");
    let mut out = vec![0.0_f32; 3 * s * s];
    for (c, plane) in out.chunks_exact_mut(s * s).enumerate() {
        for y in 0..h {
            let src = &rgb[y * w * 3..(y + 1) * w * 3];
            for (x, v) in plane[y * s..y * s + w].iter_mut().enumerate() {
                *v = (src[x * 3 + c] - MEAN[c]) / STD[c];
            }
        }
    }
    out
}
