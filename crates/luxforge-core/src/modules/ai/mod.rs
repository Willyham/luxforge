//! AI editing (`docs/design/ai-editing.md`): the `luxforge.ai` module, whose Generative tier fills
//! a brush selection with a generative model through the host's candidate seam, and the fill
//! pipeline around a model that every fill tier shares ([`fill`]). Accepting a candidate is
//! Corrections' `edit.accept-correction-patch`; this module adds no accept path, candidate store
//! or repair stage of its own.
mod analysis;
pub(crate) mod fill;
mod formats;
mod generative;
mod models;
mod module;
mod object_region;
mod objects;
mod pick;
mod picked;
mod segment;
mod selection;
mod setups;
#[cfg(test)]
mod tests;

pub use models::{DeveloperDownloads, DeveloperModels};
pub(crate) use module::AiModule;
