//! Rendering: a recipe compiled into segments and evaluated over one source, by concept.
//!
//! - [`entry`]: the one way in, [`render`], and the [`Render`] it returns.
//! - [`compiled`]: the compiled IR, segments separated by stage boundaries, and [`Entry`], the
//!   one dispatch over the boundary kinds.
//! - [`geometry`]: exact geometry, a resample's mapping and read rectangle, and the byte
//!   domain's bilinear pass.
//! - [`colour_runs`]: colour runs and their masked blend.
//! - [`pipeline`]: the one pipeline, generic over its pixel domain.
//! - [`byte`] and [`linear`]: the two pixel domains, each with its rows and its driver.
//! - [`spatial`] and [`window`]: the spatial primitive's execution and the windowed proxy.
//! - [`raster`]: the rendered frame and its helpers.
//! - [`mod@locate`]: the public locate and transform types.
//! - [`context`] and [`parallel`]: the render context's budgets and the one parallel gate.

mod byte;
mod colour_runs;
mod compiled;
mod context;
mod entry;
mod geometry;
mod input_grid;
pub(crate) mod linear;
mod locate;
pub(crate) mod map;
pub(crate) mod parallel;
mod pipeline;
mod raster;
mod restoration;
pub(crate) mod spatial;
#[cfg(test)]
pub(crate) mod testing;
mod window;

#[cfg(test)]
mod cancellation_tests;
#[cfg(test)]
mod colour_tests;
#[cfg(test)]
mod geometry_tests;
#[cfg(test)]
mod locate_tests;
#[cfg(test)]
mod mask_tests;
#[cfg(test)]
mod sample_tests;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod warp_tests;

use byte::{Byte, check_source, rasterize};
use colour_runs::{ColorRun, apply_units, color_chunk_rows, color_runs};
use compiled::ResampleEntry;
use compiled::mapped_replacements;
pub(crate) use compiled::{Compiled, Entry, Segment};
pub use context::{RenderContext, ScratchBudget};
#[cfg(test)]
pub(crate) use entry::ProxyRegionPlan;
pub(crate) use entry::{
    MaskInputMode, ProxyStage, RegionRenderOutcome, StagePixels, layer_input, prefix_pixels,
};
pub use entry::{RegionFrame, Render, RenderOptions, RenderSource, render};
use geometry::{bilinear, nearest_index, resample_frame};
pub(crate) use input_grid::{GridRequest, grid_input};
pub use input_grid::{INPUT_GRID_MAX_CELLS, InputGridCache};
pub use linear::{LinearSettings, WhiteBalanceApproximation};
pub use locate::{ContentPoint, Sample, stage_transform};
pub(crate) use locate::{locate, transform_of};
pub use map::{GeometryMap, MapError, MappingDescriptor, MappingShape, StageSize};
pub(crate) use pipeline::{Evaluation, PixelDomain, RowScratch, SpatialMode};
use pipeline::{SegmentRows, SpatialEntry, Taps, segment_pass, spatial_entry};
pub use raster::Raster;
#[cfg(test)]
pub(crate) use raster::frame_writes;
pub(crate) use raster::{frame_mut, zeroed_frame};

pub use restoration::PrefixUse;
pub(crate) use restoration::RestorationPrefixCache;
