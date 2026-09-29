//! JPEG export: one saved entry's exact render written to a new file, never touching the original
//! or any existing file. The design is `docs/design/export.md`.
//!
//! - [`metadata`] reads the original's supported EXIF fields once, when the source is prepared, and
//!   writes the export's own EXIF segment when Keep metadata is on.
//! - [`encode`] turns a rendered frame into a baseline quality-90 JPEG with an embedded sRGB
//!   profile, streaming into any writer.
//! - [`publish`] checks a destination, stages a temporary file beside it and publishes it without
//!   replacing anything.
//!
//! The job that freezes a target, renders it off the catalog owner and drives these three steps
//! lives with the owner's other jobs.

pub(crate) mod encode;
pub(crate) mod metadata;
pub(crate) mod publish;

pub use metadata::CaptureMetadata;

/// The accepted export quality (`docs/decisions.md`, export).
pub(crate) const QUALITY: u8 = 90;
