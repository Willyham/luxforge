//! How a render samples a compiled mask.
//!
//! A mask's geometry is stored normalized to the content stage, so the mask compiled against a
//! proxy stage is the *same* coverage field at a smaller scale. That is what keeps a masked recipe
//! [proxy eligible](../../docs/design/instant-preview.md) by construction: nothing about the
//! equation changes with the stage, and a proxy render of a masked stack is the exact recipe at
//! proxy size.
//!
//! The one thing that does change with the stage is **sampling**. Coverage is read once per pixel,
//! at that pixel's centre, so a feature narrower than a pixel falls between the samples and aliases
//! — a hard-edged radial, whose ramp is zero pixels wide at every stage, is the extreme case. At
//! full resolution that is the frozen behaviour the [mask study](../../docs/design/mask-study.md)
//! settled and `render.sample` agrees with byte for byte. At proxy size the same field is sampled
//! on a grid two to four times coarser, and the recorded default (proposal P5 of
//! [masking](../../docs/design/masking.md#point-queries-and-proxies)) is to supersample **the mask
//! field only** — never the effect — 2 × 2 per pixel and say the frame is approximate.
//!
//! [`MaskField`] is that decision, made once when a layer's mask is compiled and read per pixel by
//! the colour run and the spatial tiling. It holds the compiled mask the render samples and, when
//! the thin-feature rule fired, the *same mask compiled against a stage of twice the width and
//! height*. That second compilation is not a second equation: pixel `(2x, 2y)` of a doubled stage
//! has its centre at `u = (2x + 0.5) / 2H = (x + 0.25) / H`, so the four doubled-stage pixels over
//! one stage pixel are exactly the four 2 × 2 subsample positions of that pixel in the mask's own
//! space. The mathematics [`CompiledMask`] froze is untouched; only where it is asked changes.
//!
//! Cost: one extra `O(components)` compilation per thin mask at plan time, and four coverage
//! evaluations per covered pixel instead of one, on the proxy frame alone — which is at most eight
//! megapixels by the proxy's own clamp, and only inside the mask's bounds.

use std::sync::Arc;

use crate::{
    Error, Mask,
    mask::CompiledMask,
    modules::{Region, Stage},
};

/// The narrowest feature, in stage pixels, a mask may draw before the pixel grid it is sampled on
/// can no longer resolve it. Two pixels is the Nyquist reading of "a feature a pixel grid can
/// carry", and it is the number [masking](../../docs/design/masking.md#point-queries-and-proxies)
/// records.
pub(crate) const MIN_FEATURE_PX: f32 = 2.0;

/// How the render that is being compiled samples the masks in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskSampling {
    /// One coverage evaluation at the pixel's centre: the frozen field, exactly as the study
    /// settled it and exactly as `render.sample` answers. Every exact render takes this path,
    /// whatever its masks look like.
    Point,
    /// The proxy phase's rule: a mask that draws a feature narrower than [`MIN_FEATURE_PX`] at the
    /// stage it is compiled against is evaluated with a 2 × 2 supersample per pixel, and the frame
    /// it produces is reported approximate. A mask whose features the grid resolves is sampled at
    /// the pixel centre exactly as [`MaskSampling::Point`] does, so a proxy frame is byte for byte
    /// the exact recipe over the exact downscale whenever the rule did not fire.
    ThinFeature,
}

/// One layer's compiled mask together with the way this render samples it.
///
/// Cheap to clone: both compilations are behind an `Arc`, and a compiled mask is bounded by its
/// component count and never by the stage — no mask plane exists at any size.
#[derive(Clone, Debug)]
pub(crate) struct MaskField {
    /// The mask compiled against the stage its layer receives. This is the field `bounds`, `stage`
    /// and the point-sampled path all read, and it is what a description names.
    mask: Arc<CompiledMask>,
    /// The same mask compiled against `2 × stage`, present only when the thin-feature rule fired.
    /// Its pixel centres are this stage's 2 × 2 subsample positions.
    fine: Option<Arc<CompiledMask>>,
    /// `fine`'s bounds brought back to this stage, so a supersampled mask skips the same spans a
    /// point-sampled one does without ever under-covering: the doubled rectangle is conservative
    /// for the doubled grid, and halving it outwards is conservative for this one.
    bounds: Region,
}

impl MaskField {
    /// Compile `mask` against `stage` and decide how this render will sample it.
    ///
    /// `O(components)`, twice at most, and reads no pixels. A mask with **no components** draws no
    /// feature at all and [`CompiledMask::min_feature_px`] answers `f32::INFINITY`, which is not
    /// less than [`MIN_FEATURE_PX`]: an empty mask is point sampled and reports no approximation,
    /// which is the right answer for a field that is exactly zero everywhere.
    pub(crate) fn compile(
        mask: &Mask,
        stage: Stage,
        sampling: MaskSampling,
    ) -> Result<Self, Error> {
        let compiled = CompiledMask::new(mask, stage)?;
        let thin = sampling == MaskSampling::ThinFeature
            && compiled.min_feature_px(stage) < MIN_FEATURE_PX;
        if !thin {
            let bounds = compiled.bounds();
            return Ok(Self {
                mask: Arc::new(compiled),
                fine: None,
                bounds,
            });
        }
        // A doubled stage is where the subsample positions live. The clamp is arithmetic hygiene
        // rather than a reachable case: a proxy stage is at most 4096 px on a side by the proxy's
        // own bound, and a stage that cannot be doubled is simply sampled at its pixel centres.
        let Some(doubled) = doubled(stage) else {
            let bounds = compiled.bounds();
            return Ok(Self {
                mask: Arc::new(compiled),
                fine: None,
                bounds,
            });
        };
        let fine = CompiledMask::new(mask, doubled)?;
        let bounds = halved(fine.bounds(), stage);
        Ok(Self {
            mask: Arc::new(compiled),
            fine: Some(Arc::new(fine)),
            bounds,
        })
    }

    /// The coverage this render applies at stage pixel `(x, y)`.
    ///
    /// Point sampled, this *is* [`CompiledMask::evaluate`] — the same call the rasterizing pass and
    /// `render.sample` have always shared, so a sampled byte still equals the rendered byte.
    /// Supersampled, it is the mean of the frozen field at the pixel's four quarter positions,
    /// averaged in `f64` and narrowed once at the end, exactly as one evaluation is.
    #[inline]
    pub(crate) fn evaluate(&self, x: u32, y: u32) -> f32 {
        let Some(fine) = &self.fine else {
            return self.mask.evaluate(x, y);
        };
        // `2x` and `2x + 1` are in range because the fine mask was compiled against a stage twice
        // this one's size; the field is total anyway, so an overflowing coordinate is saturated
        // rather than wrapped into a different pixel.
        let x0 = x.saturating_mul(2);
        let y0 = y.saturating_mul(2);
        let x1 = x0.saturating_add(1);
        let y1 = y0.saturating_add(1);
        let sum = fine.coverage(x0, y0)
            + fine.coverage(x1, y0)
            + fine.coverage(x0, y1)
            + fine.coverage(x1, y1);
        (sum * 0.25) as f32
    }

    /// A conservative rectangle of the compiled stage outside which this render's coverage is
    /// exactly zero. Outside it the blend is the identity, so no unit is evaluated at all.
    pub(crate) fn bounds(&self) -> Region {
        self.bounds
    }

    /// The stage the mask was compiled against — the stage this field answers about, never the
    /// doubled one the supersample reads.
    pub(crate) fn stage(&self) -> Stage {
        self.mask.stage()
    }

    /// How many components the mask composes, which is the whole of its per-pixel cost.
    pub(crate) fn components(&self) -> usize {
        self.mask.components()
    }

    /// Whether the thin-feature rule fired for this mask, which is what makes the frame it is part
    /// of approximate.
    pub(crate) fn supersampled(&self) -> bool {
        self.fine.is_some()
    }

    /// Two fields are the same when they were compiled from the same allocation and sampled the
    /// same way. Comparing by allocation is conservative — two separate compilations of equal
    /// payloads report unequal — which is what the operation equality this feeds wants.
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mask, &other.mask)
            && match (&self.fine, &other.fine) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

/// The stage whose pixel centres are `stage`'s 2 × 2 subsample positions, or `None` when it is not
/// addressable.
fn doubled(stage: Stage) -> Option<Stage> {
    Some(Stage {
        width: stage.width.checked_mul(2)?,
        height: stage.height.checked_mul(2)?,
    })
}

/// A rectangle of the doubled stage brought back to `stage`, growing outwards: a stage pixel is
/// kept when any of its four subsamples is inside. Clipped to the stage, because a rectangle that
/// runs past it describes pixels no pass visits.
fn halved(region: Region, stage: Stage) -> Region {
    if region.is_empty() {
        return Region {
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
        };
    }
    let x0 = (region.x0 / 2).min(stage.width);
    let y0 = (region.y0 / 2).min(stage.height);
    let x1 = region.x1().div_ceil(2).min(stage.width);
    let y1 = region.y1().div_ceil(2).min(stage.height);
    Region {
        x0,
        y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, ComponentMode};
    use serde_json::json;

    fn stage(width: u32, height: u32) -> Stage {
        Stage { width, height }
    }

    fn mask_of(kind: &str, payload: serde_json::Value) -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name(kind);
        mask.components
            .push(Component::new(name, ComponentMode::Add, kind, payload));
        mask
    }

    /// A gradient whose ramp is a good fraction of the frame resolves at any of these stages, so
    /// the thin-feature rule does not fire and the field is the frozen one, evaluation for
    /// evaluation.
    #[test]
    fn a_resolvable_mask_is_point_sampled_even_under_the_thin_feature_rule() {
        let mask = mask_of(
            "linear",
            json!({"x0": 0.2, "y0": 0.0, "x1": 0.8, "y1": 0.0}),
        );
        let stage = stage(64, 48);
        let field = MaskField::compile(&mask, stage, MaskSampling::ThinFeature).unwrap();
        assert!(!field.supersampled());
        let compiled = CompiledMask::new(&mask, stage).unwrap();
        for y in 0..stage.height {
            for x in 0..stage.width {
                assert_eq!(field.evaluate(x, y), compiled.evaluate(x, y));
            }
        }
        assert_eq!(field.bounds(), compiled.bounds());
    }

    /// A hard-edged radial has no ramp at all — `feature_px` is `0.0` at every stage — so it is the
    /// case the rule exists for. The supersampled field is the mean of the frozen field at the four
    /// quarter positions, which is what the doubled compilation reads.
    #[test]
    fn a_hard_edge_is_supersampled_at_the_four_quarter_positions() {
        let mask = mask_of(
            "radial",
            json!({"x": 0.5, "y": 0.5, "radius_x": 0.2, "radius_y": 0.2, "angle": 0.0, "feather": 0.0}),
        );
        let coarse = stage(40, 30);
        let field = MaskField::compile(&mask, coarse, MaskSampling::ThinFeature).unwrap();
        assert!(field.supersampled(), "a hard edge draws a zero-wide ramp");
        let fine = CompiledMask::new(&mask, stage(80, 60)).unwrap();
        for y in 0..coarse.height {
            for x in 0..coarse.width {
                let expected = (fine.coverage(2 * x, 2 * y)
                    + fine.coverage(2 * x + 1, 2 * y)
                    + fine.coverage(2 * x, 2 * y + 1)
                    + fine.coverage(2 * x + 1, 2 * y + 1))
                    * 0.25;
                assert_eq!(field.evaluate(x, y), expected as f32);
            }
        }
        // The same mask on an exact render is the frozen point sample, untouched.
        let point = MaskField::compile(&mask, coarse, MaskSampling::Point).unwrap();
        assert!(!point.supersampled());
    }

    /// The rule reads a mask with no components correctly: `min_feature_px` is `f32::INFINITY`,
    /// which is not below two, so an empty mask is point sampled and never trips the supersample.
    #[test]
    fn a_mask_with_no_components_is_never_supersampled() {
        let mask = Mask::new("Mask 1");
        let stage = stage(64, 48);
        let compiled = CompiledMask::new(&mask, stage).unwrap();
        assert_eq!(compiled.min_feature_px(stage), f32::INFINITY);
        assert!(
            compiled.min_feature_px(stage) >= MIN_FEATURE_PX,
            "infinity must not read as thinner than two pixels"
        );
        let field = MaskField::compile(&mask, stage, MaskSampling::ThinFeature).unwrap();
        assert!(!field.supersampled());
        assert!(
            field.bounds().is_empty(),
            "an empty mask selects nothing, so there is nothing to blend"
        );
    }

    /// The bounds a supersampled field reports cover every pixel whose coverage is not exactly
    /// zero, which is what lets a masked pass skip the rest.
    #[test]
    fn supersampled_bounds_cover_every_pixel_the_field_reaches() {
        let mask = mask_of(
            "radial",
            json!({"x": 0.4, "y": 0.6, "radius_x": 0.15, "radius_y": 0.25, "angle": 0.0, "feather": 0.0}),
        );
        let stage = stage(37, 29);
        let field = MaskField::compile(&mask, stage, MaskSampling::ThinFeature).unwrap();
        assert!(field.supersampled());
        let bounds = field.bounds();
        for y in 0..stage.height {
            for x in 0..stage.width {
                if field.evaluate(x, y) != 0.0 {
                    assert!(
                        x >= bounds.x0 && x < bounds.x1() && y >= bounds.y0 && y < bounds.y1(),
                        "({x}, {y}) has coverage and is outside {bounds:?}"
                    );
                }
            }
        }
    }
}
