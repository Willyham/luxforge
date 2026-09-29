//! Rendering: a recipe compiled into segments and evaluated over one source, by concept.
//!
//! - [`entry`]: the one way in, [`render`], and the [`Render`] it returns.
//! - [`compiled`]: the compiled IR, segments separated by stage boundaries.
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
pub(crate) mod linear;
mod locate;
pub(crate) mod parallel;
mod pipeline;
mod raster;
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
pub(crate) mod tests;

use byte::{Byte, check_source, rasterize};
use colour_runs::{ColorRun, apply_units, color_chunk_rows, color_pixel, color_runs};
use compiled::mapped_replacements;
pub(crate) use compiled::{Compiled, Entry, Segment};
pub use context::RenderContext;
pub(crate) use context::ScratchBudget;
#[cfg(test)]
pub(crate) use entry::ProxyRegionPlan;
pub(crate) use entry::{ProxyStage, RegionRenderOutcome, Render, layer_input};
pub use entry::{RegionFrame, RenderOptions, RenderSource, render};
use geometry::{bilinear, nearest_index, resample_frame};
pub use linear::LinearSettings;
pub(crate) use linear::WhiteBalanceApproximation;
pub use locate::{ContentPoint, Sample, StageSize, StageTransform, stage_transform};
pub(crate) use locate::{locate, transform_of};
pub(crate) use pipeline::{Evaluation, PixelDomain, RowScratch, SpatialMode};
use pipeline::{SegmentRows, SpatialEntry, Taps, segment_pass, spatial_entry};
pub use raster::Raster;
#[cfg(test)]
pub(crate) use raster::frame_writes;
pub(crate) use raster::{frame_mut, zeroed_frame};
