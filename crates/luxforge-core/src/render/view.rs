//! A finished frame reduced to the view's size: what the reference job hands the desktop where the
//! view draws the stage smaller than it is — at Fit and at a percentage below 100% — for the
//! reference renderer to draw when the GPU cannot, and behind the GPU's own picture at rest
//! (`docs/design/gpu-first.md`, stage 2).
//!
//! It is not a cache or a proxy: it reduces pixels the stack has already rendered at full
//! resolution, by the area-weighted average of their linear light the reference frame is held to
//! at those views, and nothing is rendered at the view's size. A view-size change reduces the same
//! retained frame again, with no render.
use crate::{Cancel, Error, ProxyBounds, ProxyPlan, Raster};

/// `raster`, a finished frame, reduced to fit `bounds` by the linear-light area average, with
/// every pixel re-quantized through the output's thresholds; `None` when it already fits them.
/// `O(source pixels)`, in bands of the source's rows, on the caller's thread; the source's pixels
/// are borrowed through their allocation and only the reduced frame is allocated, at most 8 MP.
pub(crate) fn reduce_to_view(
    raster: &Raster,
    bounds: ProxyBounds,
    cancel: &Cancel,
) -> Result<Option<Raster>, Error> {
    let dimensions = (raster.width, raster.height);
    ProxyPlan::fit(dimensions, dimensions, bounds)
        .map(|plan| crate::proxy::reduce_raster(raster, plan, cancel))
        .transpose()
}
