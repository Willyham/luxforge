//! Object removal probe: segmentation, stroke resolution, mask refinement and fill, outside the
//! editor. The contract is the README's "Contract" section.

pub mod args;
pub mod commands;
pub mod corpus;
pub mod fill;
pub mod generative;
pub mod image;
pub mod lab;
pub mod mask;
pub mod models;
pub mod paths;
pub mod refine;
pub mod runtime;
pub mod score;
pub mod segment;
pub mod stroke;
pub mod tensor;
mod tract_backend;
