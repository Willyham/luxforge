//! Rendering: a recipe compiled into segments and evaluated over one source, by concept.
//!
//! - [`entry`]: the one way in, [`render`], and the [`Render`] it returns.
//! - [`boundary`]: a GPU preview's held input boundary, rendered once per draft.
//! - [`compiled`]: the compiled IR, segments separated by stage boundaries, and [`Entry`], the
//!   one dispatch over the boundary kinds.
//! - [`gpu`]: the GPU programs modules own and the plan a gesture's preview is drawn from.
//! - [`geometry`]: exact geometry, a resample's mapping and read rectangle, and the byte
//!   domain's bilinear pass.
//! - [`colour_runs`]: colour runs and their masked blend.
//! - [`pipeline`]: the one pipeline, generic over its pixel domain.
//! - [`byte`] and [`linear`]: the two pixel domains, each with its rows and its driver.
//! - [`spatial`] and [`window`]: the spatial primitive's execution and the GPU window walk.
//! - [`raster`]: the rendered frame and its helpers.
//! - [`mod@locate`]: the public locate and transform types.
//! - [`context`] and [`parallel`]: the render context's budgets and the one parallel gate.

mod boundary;
mod byte;
mod colour_runs;
mod compiled;
mod context;
mod entry;
mod geometry;
pub(crate) mod gpu;
mod input_grid;
pub(crate) mod linear;
mod locate;
pub(crate) mod map;
pub(crate) mod parallel;
mod pipeline;
mod raster;
pub(crate) mod reduced;
pub(crate) mod spatial;
#[cfg(test)]
pub(crate) mod testing;
mod view;
mod window;

#[cfg(test)]
mod boundary_tests;
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

pub use boundary::{BOUNDARY_MAX_BYTES, BoundaryFormat, BoundaryFrame};
use byte::{Byte, check_source, rasterize};
use colour_runs::{ColorRun, MaskedInput, apply_units, color_chunk_rows, color_runs};
use compiled::ResampleEntry;
use compiled::mapped_replacements;
pub(crate) use compiled::{Compiled, Entry, Segment};
pub use context::{RenderContext, ScratchBudget};
pub(crate) use entry::{MaskInputMode, ProxyStage, StagePixels, layer_input, prefix_pixels};
pub use entry::{Render, RenderOptions, RenderSource, render};
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
pub(crate) use view::reduce_to_view;
