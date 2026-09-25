//! The tool modules end to end, one module per tool: the colour mixer, Presence and the vignette
//! against their independent references through the real render paths, the developer controls
//! proof, and presets; and the one field-patch conformance suite (`conformance/`) every field-patch
//! module passes, which `cargo xtask editor-acceptance` also runs in release.

mod conformance;
mod controls;
mod field_patch;
mod mixer;
mod presence;
mod presets;
mod vignette;
