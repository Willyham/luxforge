//! The public locate and transform types, and the walks that answer them from a compilation
//! without reading a pixel.

use super::Compiled;
use crate::{Error, ModuleRegistry, Recipe, modules::ExactGeometry};
use serde::{Deserialize, Serialize};

/// One evaluated pixel of a recipe's output stage, with that stage's dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub width: u32,
    pub height: u32,
    /// `None` when the coordinate lies outside the output stage.
    pub rgba: Option<[u8; 4]>,
}

/// One located pixel of a recipe's content stage: the source after EXIF orientation, which is the
/// stage the first layer receives and the stage a pixel-stage edit addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPoint {
    pub content_x: u32,
    pub content_y: u32,
    /// The content stage's dimensions.
    pub width: u32,
    pub height: u32,
}

/// One stage's dimensions, as a mapping result names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSize {
    pub width: u32,
    pub height: u32,
}

/// The whole geometry tail of one recipe as a single affine map, both ways, between the content
/// stage and the output stage.
///
/// [`ContentPoint`] answers one pixel at a time, which is what a pick needs. A gesture that must
/// follow the pointer cannot pay that call per move ([performance rule
/// 12](../../../../docs/engineering/performance-rules.md#rules)), and it does not have to: the tail is
/// exact integer transforms plus at most one crop resample, so the map is affine and one matrix
/// answers every position a gesture will ask about.
///
/// **Coordinates are continuous and pixel-center based, the convention `Resample` already fixes:**
/// pixel index `n` has its center at `n + 0.5`, so a coordinate `c` lies in pixel `c.floor()` and
/// the content stage spans `0.0..width` by `0.0..height`. The
/// [crop spec](../../../../docs/specs/single-image.md#sampling) states the same thing about the crop's own
/// sampling, and `Resample::inverse` maps output pixel centers to exactly these input
/// coordinates, so nothing here introduces a second convention.
///
/// Both matrices are `[m0, m1, m2, m3, m4, m5]`, again the coefficient order of
/// `Resample::inverse`: `x' = m0·x + m1·y + m2` and `y' = m3·x + m4·y + m5`. `forward` maps a
/// content coordinate to an output coordinate and `inverse` is its exact inverse; a coordinate that
/// lands outside the output stage was cropped away, which the caller sees from `output` and this
/// type does not hide by clamping.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageTransform {
    pub content: StageSize,
    pub output: StageSize,
    pub forward: [f64; 6],
    pub inverse: [f64; 6],
}

/// One affine map in the continuous, pixel-center coordinates [`StageTransform`] documents, in the
/// coefficient order [`Resample::inverse`](crate::modules::Resample::inverse) fixes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Affine(pub(super) [f64; 6]);

impl Affine {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    /// The continuous form of one exact step. [`ExactGeometry`] maps pixel *indices*; this is the
    /// same mapping over pixel *centers*, so the half-pixel of the convention is conjugated in on
    /// both sides: `out = A·(in − ½) + t + ½`. An exact step is a signed permutation with an integer
    /// translation, so every coefficient stays exact in `f64` and the center of an input pixel lands
    /// exactly on the center of an output pixel.
    fn from_exact(step: ExactGeometry) -> Self {
        let (a, b, c, d) = (step.a as f64, step.b as f64, step.c as f64, step.d as f64);
        Self([
            a,
            b,
            step.tx as f64 + 0.5 - 0.5 * (a + b),
            c,
            d,
            step.ty as f64 + 0.5 - 0.5 * (c + d),
        ])
    }

    /// `self` followed by `next`. Composing the tail this way is what keeps the cost `O(layers)`:
    /// one matrix multiply per layer, and no walk per point afterwards.
    pub(super) fn then(self, next: Self) -> Self {
        let [a0, a1, a2, a3, a4, a5] = self.0;
        let [b0, b1, b2, b3, b4, b5] = next.0;
        Self([
            b0 * a0 + b1 * a3,
            b0 * a1 + b1 * a4,
            b0 * a2 + b1 * a5 + b2,
            b3 * a0 + b4 * a3,
            b3 * a1 + b4 * a4,
            b3 * a2 + b4 * a5 + b5,
        ])
    }

    /// The exact inverse map, or a refusal. A step that does not invert collapses its stage onto a
    /// line or a point, so there is no mapping to report and reporting the identity instead would
    /// put a gesture's pointer somewhere the recipe never puts it. The compiler already refuses a
    /// resample whose mapping is not finite or whose output stage is empty; this is the remaining
    /// degenerate case, refused in the same voice.
    pub(super) fn invert(self) -> Result<Self, Error> {
        let [m0, m1, m2, m3, m4, m5] = self.0;
        let determinant = m0 * m4 - m1 * m3;
        let inverted = Self([
            m4 / determinant,
            -m1 / determinant,
            (m1 * m5 - m4 * m2) / determinant,
            -m3 / determinant,
            m0 / determinant,
            (m3 * m2 - m0 * m5) / determinant,
        ]);
        if determinant == 0.0 || !inverted.0.iter().all(|value| value.is_finite()) {
            return Err(Error::validation(
                "a geometry layer declares a mapping that cannot be inverted",
            ));
        }
        Ok(inverted)
    }
}

/// Map one pixel of the output stage of a stack compiled against a `width` × `height` content stage
/// back to the content-stage pixel it shows: the source after EXIF orientation, the stage the first
/// layer receives. A point outside the output stage is a validation error naming that stage. Cost is
/// linear in the layer count, reusing the caller's compilation, and no frame is allocated, so the
/// canvas pick and the API query share one implementation. Source pixels are never needed.
pub(crate) fn locate(
    compiled: &Compiled,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    /// Walk one segment backwards, the same walk `pixel_in` makes to fetch a colour: the composed
    /// exact geometry unmaps to the segment's input frame by its integer inverse, and the boundary
    /// entering it maps that pixel back to the previous stage ([`super::Entry::locate`]): a
    /// resample takes the nearest pixel to the input coordinate its output pixel centre samples.
    fn walk(compiled: &Compiled, index: usize, x: u32, y: u32) -> Option<(u32, u32)> {
        let segment = &compiled.segments[index];
        if x >= segment.width || y >= segment.height {
            return None;
        }
        let (input_x, input_y) = segment.geometry.unmap(x, y);
        let Some(entry) = &segment.entry else {
            // The first segment reads the source, which is the content stage.
            return Some((input_x, input_y));
        };
        let (x, y) = entry.locate(input_x, input_y, compiled.segments[index - 1].stage());
        walk(compiled, index - 1, x, y)
    }
    let stage = compiled.stage();
    let (content_x, content_y) =
        walk(compiled, compiled.segments.len() - 1, x, y).ok_or_else(|| {
            Error::validation(format!(
                "point ({x}, {y}) is outside the {}x{} rendered image",
                stage.width, stage.height
            ))
        })?;
    Ok(ContentPoint {
        content_x,
        content_y,
        width,
        height,
    })
}

/// The whole geometry tail of one recipe as one affine map between the content stage and the output
/// stage, in both directions.
///
/// This is [`locate`] in closed form. That walks a point back through the compiled
/// segments, which is right for a pick and wrong for a gesture that follows the pointer, so the
/// composition happens once here and the caller maps positions itself. Every step of the tail
/// composes: an exact step is an integer signed permutation with an integer translation, a spatial
/// boundary keeps every coordinate of the stage it receives, and the one crop resample declares its
/// output-to-input mapping, which inverts. Nothing else can appear in the tail.
///
/// The dimensions are the whole input, not a source: no pixel is read on any path here, so there is
/// none to pass. Cost is one matrix multiply per layer on top of compiling the stack, and like
/// `super::Render::stage` it allocates no frame, which is what lets the catalog owner answer it
/// ([rules 4 and 5](../../../../docs/engineering/performance-rules.md#rules)).
///
/// A stack the compiler refuses has no output stage to map, and its reason is returned unchanged —
/// an unavailable effect is `Incompatible`, a stack the host cannot evaluate is `Validation`. There
/// is no identity fallback on any path.
pub fn stage_transform(
    registry: &ModuleRegistry,
    width: u32,
    height: u32,
    recipe: &Recipe,
) -> Result<StageTransform, Error> {
    transform_of(&registry.compile(width, height, recipe)?, width, height)
}

/// [`stage_transform`] of a stack already compiled against a `width` × `height` content stage.
pub(crate) fn transform_of(
    compiled: &Compiled,
    width: u32,
    height: u32,
) -> Result<StageTransform, Error> {
    let mut forward = Affine::IDENTITY;
    for segment in &compiled.segments {
        // Each boundary adds its own forward map (`Entry::forward`); a spatial boundary is at
        // the dimensions of the stage it receives and moves no coordinate.
        if let Some(entry) = &segment.entry {
            forward = entry.forward(forward)?;
        }
        forward = forward.then(Affine::from_exact(segment.geometry));
    }
    let inverse = forward.invert()?;
    let stage = compiled.stage();
    Ok(StageTransform {
        content: StageSize { width, height },
        output: StageSize {
            width: stage.width,
            height: stage.height,
        },
        forward: forward.0,
        inverse: inverse.0,
    })
}
