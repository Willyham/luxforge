//! Optional usage and performance statistics: protected consent, typed events and their bounded
//! delivery (`docs/design/usage-statistics.md`). A host service, not an editing module: nothing here
//! reads an original, a pixel or a recipe value, and nothing runs until a person opts in.
pub mod consent;
pub mod eligibility;
pub mod encode;
pub mod event;
pub mod gate;
pub mod image_id;
pub mod registry;

pub use gate::{Gate, Generation};

/// The disclosure the startup notice and Settings › Privacy describe. Consent saved for another
/// version is not consent to this one.
pub const DISCLOSURE_VERSION: u32 = 1;

/// The only production destination. Tests inject their own receiver.
pub const DESTINATION: &str = "https://api.luxforge.app/v1/usage/batches";
