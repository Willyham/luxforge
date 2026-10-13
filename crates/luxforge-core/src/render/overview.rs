//! The high-zoom minimap's overview of a whole stage (`docs/design/minimap.md`, "Overview
//! rendering and lifetime"): a navigation thumbnail of the selected edited image, processed first
//! at full stage resolution, then area-averaged in linear light and quantized by the output
//! encoding.
//!
//! - **Its size.** Aspect-fitted within [`OVERVIEW_BOUNDS`], 256 × 256 physical pixels, never
//!   larger than the stage ([`overview_size`]); a smaller view may ask for smaller bounds.
//! - **On the GPU.** The picture at rest's tiles already cover the whole output stage at full
//!   resolution; [`RestTiles::with_overview`] adds a second, independent reduction of the same
//!   tiles to the overview's size ([`RestTiles::overview`]). It neither replaces the view's
//!   reduction nor changes the counts, and no tile is drawn for it alone.
//! - **On the reference.** [`reduce_overview`] reduces a finished frame the desktop already
//!   retains, by the same linear-light area average ([`crate::area_coverage`]'s weights) the
//!   reference's view reduction uses, on the caller's worker thread. It borrows the frame's pixels
//!   and allocates only the overview, its coverage tables and one band of averaged rows, together
//!   within [`OVERVIEW_CPU_BYTES`]; a stage of the overview's size is copied, the overview itself.
//! - **Bounds.** Every GPU allocation an overview adds — the reduction's accumulator, coverage
//!   tables and parameters, the finished textures, current and replacement, and those retiring —
//!   is held within [`OVERVIEW_GPU_BYTES`] inside the GPU-preview budget, and a picture at rest
//!   planned at a percentage zoom of 100% or more leaves that much of its share for it.
use super::gpu::{RestReduction, RestTiles};
use crate::{
    Cancel, Error, ProxyBounds, ProxyPlan,
    colour::srgb::{decode_pixel_in, decode_table, quantizer},
    proxy::BoxDownscale,
};
use std::sync::Arc;

/// The bounds an overview raster is aspect-fitted within: 256 × 256 physical pixels.
pub const OVERVIEW_BOUNDS: ProxyBounds = ProxyBounds {
    width: 256,
    height: 256,
};

/// The most the reference's overview reduction holds at once: the overview, its coverage tables
/// and its band of averaged rows, 1 MiB. The frame it reduces is borrowed, never counted or copied.
pub const OVERVIEW_CPU_BYTES: u64 = 1 << 20;

/// The most every GPU allocation of the overview takes together, resident, in progress and
/// retiring: 4 MiB, charged inside the GPU-preview budget before anything is created (the GPU
/// crate's `OVERVIEW_GPU_BUDGET`, which a test holds equal).
pub const OVERVIEW_GPU_BYTES: u64 = 4 << 20;

/// The size of the overview of a `stage`-sized stage within `bounds`, themselves held within
/// [`OVERVIEW_BOUNDS`]: the stage scaled by `min(bounds / stage, 1)` on both axes with the
/// reference's rounding ([`ProxyPlan`]'s fit), each side at least one pixel; the stage itself when
/// it already fits. `None` for an empty stage. `O(1)`.
pub fn overview_size(stage: (u32, u32), bounds: ProxyBounds) -> Option<(u32, u32)> {
    if stage.0 == 0 || stage.1 == 0 {
        return None;
    }
    let bounds = ProxyBounds {
        width: bounds.width.clamp(1, OVERVIEW_BOUNDS.width),
        height: bounds.height.clamp(1, OVERVIEW_BOUNDS.height),
    };
    Some(ProxyPlan::fit(stage, stage, bounds).map_or(stage, |plan| (plan.width, plan.height)))
}

/// The reduction of an `output`-sized stage to the overview of `size`: the area-average weights
/// across and down ([`crate::area_coverage`]), the same ones the reference reduces with. `O(stage
/// sides)`, no pixel.
pub fn overview_reduction(output: (u32, u32), size: (u32, u32)) -> RestReduction {
    RestReduction {
        view: size,
        across: crate::area_coverage(output.0, size.0),
        down: crate::area_coverage(output.1, size.1),
    }
}

impl RestTiles {
    /// These tiles with the overview of their output stage within `bounds` reduced from them as
    /// they are drawn ([`Self::overview`]): an independent second reduction of the final tiles,
    /// which leaves the view's reduction, the counts and every tile as they are. `O(stage sides)`,
    /// no pixel: the coverage tables alone.
    pub fn with_overview(mut self: Box<Self>, bounds: ProxyBounds) -> Box<Self> {
        let output = (self.output.width, self.output.height);
        self.overview = overview_size(output, bounds).map(|size| overview_reduction(output, size));
        self
    }
}

/// An overview raster: opaque RGBA codes, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// What [`reduce_overview`] holds reducing a `stage` frame to `size`, its output included: the
/// overview's codes, the coverage tables of both axes and the band of averaged rows one output
/// row's source rows need, the least it can work with. The overview alone for a stage of its size,
/// which is copied. `O(1)`.
pub fn overview_cpu_bytes(stage: (u32, u32), size: (u32, u32)) -> u64 {
    let (width, height) = (u64::from(stage.0), u64::from(stage.1));
    let (out_width, out_height) = (u64::from(size.0), u64::from(size.1));
    let output = out_width * out_height * 4;
    if stage == size {
        return output;
    }
    // Per axis: a first index (u32) and an offset (usize) per output sample, and a weight (f64)
    // for at most every source sample plus one per output sample.
    let tables = (out_width + out_height) * 4
        + (out_width + out_height + 2) * 8
        + (width + height + out_width + out_height) * 8;
    output + tables + min_band(stage, size)
}

/// The averaged rows one output row's source rows take: `⌈rows ÷ output rows⌉ + 1` rows of the
/// overview's width in `f32`, three channels.
fn min_band(stage: (u32, u32), size: (u32, u32)) -> u64 {
    let rows = u64::from(stage.1).div_ceil(u64::from(size.1)) + 1;
    rows * u64::from(size.0) * 3 * 4
}

/// The overview of `pixels`, a finished `stage` frame of opaque RGBA codes the caller retains and
/// lends, at `size` ([`overview_size`]): each code decoded through the output's table,
/// area-averaged across and then down in `f64` with the reference's weights, and quantized by the
/// output's thresholds — the bytes the reference's own view reduction of the frame gives at that
/// size, whatever its bands. A stage of the overview's size is the frame's codes, copied. Serial
/// bands on the caller's thread, which must be a worker; within [`OVERVIEW_CPU_BYTES`] or refused
/// as a resource limit, never past it. `cancel` is checked once a row.
pub fn reduce_overview(
    pixels: &[u8],
    stage: (u32, u32),
    size: (u32, u32),
    cancel: &Cancel,
) -> Result<Overview, Error> {
    let (width, height) = stage;
    if size.0 == 0 || size.1 == 0 || size.0 > width || size.1 > height {
        return Err(Error::validation(format!(
            "an overview of {}x{} does not fit a {width}x{height} stage",
            size.0, size.1
        )));
    }
    // Refused before a pixel is read: past the bound, then a frame of another length.
    let needed = overview_cpu_bytes(stage, size);
    if needed > OVERVIEW_CPU_BYTES {
        return Err(Error::resource_limit(format!(
            "an overview of a {width}x{height} stage needs {needed} bytes, past its \
             {OVERVIEW_CPU_BYTES}-byte bound"
        )));
    }
    if pixels.len() != crate::Raster::expected_len(width, height)? {
        return Err(Error::validation(
            "an overview's frame does not match its stage",
        ));
    }
    if size == stage {
        cancel.check()?;
        return Ok(Overview {
            width,
            height,
            rgba: Arc::new(pixels.to_vec()),
        });
    }
    let plan = ProxyPlan {
        width: size.0,
        height: size.1,
        bounds: OVERVIEW_BOUNDS,
        window: None,
    };
    // What the bound leaves for the band beside the output and the tables, each band at least one
    // output row's source rows, which `needed` already counts.
    let band_bytes = (OVERVIEW_CPU_BYTES - needed + min_band(stage, size)) as usize;
    // Serial: one band's rows at a time, so the bound holds whatever the pool's width.
    let mut downscale = BoxDownscale::new(width, height, plan, band_bytes)?;
    downscale.parallel = false;
    let (table, quantizer) = (decode_table(), quantizer());
    let stride = width as usize * 4;
    let read_row = |y: u32| {
        let start = y as usize * stride;
        let bytes = &pixels[start..start + stride];
        move |x: u32| {
            let at = x as usize * 4;
            decode_pixel_in(table, [bytes[at], bytes[at + 1], bytes[at + 2]])
        }
    };
    let output_stride = size.0 as usize * 4;
    let mut rgba = vec![0u8; output_stride * size.1 as usize];
    let mut rows = Vec::new();
    for (band, out) in rgba
        .chunks_mut(downscale.band_rows * output_stride)
        .enumerate()
    {
        downscale.band(band, &mut rows, cancel, &read_row, |y, x, sum| {
            let pixel = &mut out[y * output_stride + x * 4..][..4];
            for (channel, value) in sum.iter().enumerate() {
                pixel[channel] = quantizer.channel(*value);
            }
            pixel[3] = 255;
        })?;
    }
    Ok(Overview {
        width: size.0,
        height: size.1,
        rgba: Arc::new(rgba),
    })
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
