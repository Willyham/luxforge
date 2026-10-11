//! Object removal probe: segmentation, stroke resolution, mask refinement and fill, outside the
//! editor. The contract is `docs/design/object-removal.md`.

pub mod args;
pub mod commands;
pub mod fill;
pub mod image;
pub mod lab;
pub mod mask;
pub mod models;
pub mod refine;
pub mod runtime;
pub mod segment;
pub mod stroke;
pub mod tensor;
mod tract_backend;
