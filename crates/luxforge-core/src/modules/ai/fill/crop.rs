//! Where a fill reads and works: the square crop around the region, the windows the later stages
//! work over, and the reflection that pads the square past the stage's edges.
use crate::modules::{Region, Stage};

/// Context around the region's bounds, as a share of their longer side: one region-length on
/// every side, so the region spans about a third of the model's input.
pub(crate) const CONTEXT_SHARE: f64 = 1.0;
/// Context never smaller than this, in stage pixels.
pub(crate) const MIN_CONTEXT: u32 = 48;

/// Where the model's square sits in the stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CropPlan {
    /// The region's bounds: every pixel whose coverage may be above zero.
    pub(crate) bounds: Region,
    /// Context added on every side of the bounds before squaring, in stage pixels.
    pub(crate) context: u32,
    /// The square's top-left corner; negative where it is padded past the top or left edge.
    pub(crate) origin: (i64, i64),
    /// The square's side in stage pixels.
    pub(crate) side: u32,
    /// The part of the square inside the stage: everything the fill reads.
    pub(crate) inside: Region,
}

/// The square crop for a region's bounds in a stage: context of `max(MIN_CONTEXT, CONTEXT_SHARE ×
/// longer side)`, squared to the grown longer side, centred on the region and slid inside the stage
/// where it fits; where it does not, it spans the axis and is padded past both edges by
/// reflection, never stretched, so resizing it to the model's square keeps geometry undistorted.
/// `bounds` is not empty.
pub(crate) fn plan_crop(bounds: Region, stage: Stage) -> CropPlan {
    let longer = bounds.width.max(bounds.height);
    let context = MIN_CONTEXT.max((CONTEXT_SHARE * f64::from(longer)).round() as u32);
    let side = longer + 2 * context;
    let axis = |start: u32, len: u32, extent: u32| -> i64 {
        let (s, e) = (i64::from(side), i64::from(extent));
        let centred = i64::from(start) + (i64::from(len) - s).div_euclid(2);
        if s <= e {
            centred.clamp(0, e - s)
        } else {
            centred.clamp(e - s, 0)
        }
    };
    let origin = (
        axis(bounds.x0, bounds.width, stage.width),
        axis(bounds.y0, bounds.height, stage.height),
    );
    let x0 = origin.0.max(0) as u32;
    let y0 = origin.1.max(0) as u32;
    let x1 = (origin.0 + i64::from(side)).min(i64::from(stage.width)) as u32;
    let y1 = (origin.1 + i64::from(side)).min(i64::from(stage.height)) as u32;
    CropPlan {
        bounds,
        context,
        origin,
        side,
        inside: Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        },
    }
}

/// Reflects an index into `0..n` (`… 2 1 | 0 1 2 … n−1 | n−2 …`, OpenCV's `BORDER_REFLECT_101`),
/// repeating for pads longer than the axis.
pub(crate) fn reflect(i: i64, n: u32) -> u32 {
    let n = i64::from(n);
    if (0..n).contains(&i) {
        return i as u32;
    }
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let m = i.rem_euclid(period);
    (if m < n { m } else { period - m }) as u32
}

/// `r` grown by `by` on every side, clamped to `within`.
pub(crate) fn grow_within(r: Region, by: u32, within: Region) -> Region {
    let x0 = r.x0.saturating_sub(by).max(within.x0);
    let y0 = r.y0.saturating_sub(by).max(within.y0);
    let x1 = r.x1().saturating_add(by).min(within.x1());
    let y1 = r.y1().saturating_add(by).min(within.y1());
    Region {
        x0,
        y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

/// The smallest region holding both.
pub(crate) fn union(a: Region, b: Region) -> Region {
    let x0 = a.x0.min(b.x0);
    let y0 = a.y0.min(b.y0);
    Region {
        x0,
        y0,
        width: a.x1().max(b.x1()) - x0,
        height: a.y1().max(b.y1()) - y0,
    }
}

/// Laplacian pyramid levels for a fill enlarged by `scale`: the coarsest band spans a few model
/// pixels, so the mismatch the model's resolution carries fades out over its own scale.
pub(crate) fn blend_levels(scale: f64) -> usize {
    (scale.max(1.0).log2().round() as usize + 3).clamp(3, 7)
}

/// How far the blend reaches past the region's bounds at `levels`.
pub(crate) fn blend_reach(levels: usize) -> u32 {
    4 << levels
}

/// Ring width for the grain estimate: `max(16, 0.25 × region size)`, the size being the geometric
/// mean of the bounds' sides, so a long thin region does not take a huge ring.
pub(crate) fn ring_width(bounds: Region) -> u32 {
    let size = (f64::from(bounds.width) * f64::from(bounds.height)).sqrt();
    ((0.25 * size).round() as u32).max(16)
}
