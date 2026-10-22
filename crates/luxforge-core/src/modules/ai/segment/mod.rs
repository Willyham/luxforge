//! Promptable segmentation (`docs/design/ai-editing.md`, "Picking: hover, click, box and brush"):
//! an image encoder run once per analysis input and a prompt decoder run per hover, click, box or
//! stroke, over the EfficientViT-SAM L0 contract the prototype pinned
//! (`probes/object-removal/src/segment`), which the developer fake implements too, so the editor
//! drives one path whichever runtime answers.
//!
//! Coordinates are continuous pixels of the **analysis input** (the repair stage's output reduced
//! to at most [`super::analysis::ANALYSIS_SIDE`] on its long side): the origin is the top-left
//! corner of the top-left pixel, and pixel `(i, j)` covers `[i, i + 1) × [j, j + 1)`. A request's
//! positions are the content stage's normalized coordinates, which [`Point::from_normalized`]
//! scales by the analysis input's size.
mod frame;
mod resample;
mod session;
#[cfg(test)]
mod tests;

pub(crate) use frame::{Frame, long_side_size};
pub(crate) use resample::resample;
pub(crate) use session::{Embedding, Segmenter};

/// A position in analysis-input pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl Point {
    pub(crate) fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// The position a normalized `[x, y]` of the content stage names in a raster of `size`.
    pub(crate) fn from_normalized([x, y]: [f64; 2], size: (u32, u32)) -> Self {
        Self::new(
            (x * f64::from(size.0)) as f32,
            (y * f64::from(size.1)) as f32,
        )
    }
}

/// Low-resolution mask logits as a SAM-family decoder answers them, with the map from analysis
/// pixels. A logit above 0 is inside the object.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Logits {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) data: Vec<f32>,
    /// The analysis input the logits describe.
    pub(crate) source: (u32, u32),
    /// Logit cells per source pixel along x and y. A source pixel centre `(i + 0.5, j + 0.5)` maps
    /// to the cell coordinate `((i + 0.5) · cx − 0.5, (j + 0.5) · cy − 0.5)`, where cell `(u, v)`
    /// has its centre at integer `(u, v)` (the `align_corners = false` convention).
    pub(crate) cells_per_px: (f32, f32),
}

impl Logits {
    /// The bilinearly interpolated logit at a continuous source position, clamping at the edges.
    pub(crate) fn sample(&self, p: Point) -> f32 {
        let u = (p.x * self.cells_per_px.0 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let v = (p.y * self.cells_per_px.1 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (u0, v0) = (u.floor() as usize, v.floor() as usize);
        let (u1, v1) = ((u0 + 1).min(self.width - 1), (v0 + 1).min(self.height - 1));
        let (fu, fv) = (u - u0 as f32, v - v0 as f32);
        let at = |x: usize, y: usize| self.data[y * self.width + x];
        let top = at(u0, v0) * (1.0 - fu) + at(u1, v0) * fu;
        let bottom = at(u0, v1) * (1.0 - fu) + at(u1, v1) * fu;
        top * (1.0 - fv) + bottom * fv
    }
}

/// One prompt in analysis pixels: at least one point or a box.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Prompt {
    pub(crate) positive: Vec<Point>,
    pub(crate) negative: Vec<Point>,
    /// Top-left and bottom-right corners.
    pub(crate) bbox: Option<(Point, Point)>,
}

impl Prompt {
    /// How many tokens the decoder receives for it: every point, and two corners for a box.
    pub(crate) fn tokens(&self) -> usize {
        self.positive.len() + self.negative.len() + if self.bbox.is_some() { 2 } else { 0 }
    }
}

/// One decoded mask and the decoder's own estimate of its IoU with the intended object.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) logits: Logits,
    pub(crate) predicted_iou: f32,
}
