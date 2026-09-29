//! The crop tool: fine straightening and a crop rectangle over one input stage.
//!
//! The geometry is separated from the module so the host, the module and the desktop share exactly
//! one implementation of the rotated box, coverage and fitting math.
pub(super) mod geometry;
mod module;

pub(crate) use module::CropModule;
pub use module::{CROP_EFFECT, CropAspect};
