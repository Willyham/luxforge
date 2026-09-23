//! Lightwell's widget library: the Develop workspace's visual language and generated controls.
//!
//! This crate depends on Iced only. It never depends on `lightwell-core`, and no test here links
//! it. That boundary is deliberate, for three reasons. First, it is enforced by the compiler
//! rather than by review: with no `lightwell-core` type in scope, nothing in this crate can reach
//! the catalog owner's authoritative state, validate a parameter against a module descriptor, or
//! construct an `edit.*` request — every widget here is therefore structurally unable to hold
//! editing logic, and takes plain data structs, enums and message values or closures instead.
//! Second, it is a compile-unit boundary: a styling or layout change here rebuilds this crate and
//! `lightwell-app`, never `lightwell-core`, so widget churn never lengthens the core's own build.
//! Third, the split is free at runtime: this crate links into the same desktop binary as
//! everything else and holds only pure functions and constants, so it starts no process, no
//! thread and no timer of its own — it adds no startup work beyond what any other Rust module
//! would.

pub mod geometry;
pub mod photo_surface;
pub mod theme;
mod widgets;

pub use photo_surface::{PhotoRaster, Placement, photo_surface};
pub use widgets::*;

mod gallery;
mod gallery_components;
mod gallery_panels;

/// Builds one instance of every widget in every state shown on the components board
/// (`docs/design/develop-workspace/components.png`), as `Element<'_, ()>` values, so a caller can
/// prove the whole set builds without panicking.
///
/// Hidden because it exists for unit checks and the real-app gallery evidence renderer,
/// not for reuse as part of the widget API.
#[doc(hidden)]
pub fn gallery_states() -> Vec<iced::Element<'static, ()>> {
    gallery::gallery()
}

/// Names the exact gallery states in draw order for the real-app evidence board.
#[doc(hidden)]
pub fn gallery_named_states() -> Vec<(&'static str, iced::Element<'static, ()>)> {
    const NAMES: [&str; 73] = [
        "Highlights · resting slider",
        "Exposure · dragging slider",
        "Contrast · editing slider value",
        "Temperature · invalid slider value",
        "Saturation · disabled slider",
        "Basic · expanded section",
        "Detail · collapsed section",
        "Lens profile · unavailable section",
        "Tone · subgroup with reset",
        "Rotate right · icon button",
        "Crop · selected icon button",
        "Reset · disabled icon button",
        "Crop ratio · segmented choice",
        "Warm · selected chip",
        "Print draft · ordinary chip",
        "Shadows +25 · current history row",
        "Vibrance +15 · previewed history row",
        "Crop 4:5 · ordinary history row",
        "Rotate right · branch history row",
        "Original not found · notice",
        "Changed elsewhere · warning notice",
        "Crop · floating bar",
        "Double-click · reset wrapper",
        "Canvas · mode strip",
        "JSON request · inline menu",
        "Histogram · ready",
        "Histogram · stale",
        "Histogram · empty",
        "Shadow clipping · untinted",
        "Shadow clipping · tinted",
        "Shadow clipping · active",
        "Highlight clipping · disabled",
        "Title typography",
        "Control label typography",
        "Caption typography",
        "Section label typography",
        "Error caption typography",
        "Value typography",
        "Hue rail · resting",
        "Hue rail · below soft range",
        "Hue rail · above soft range",
        "Angle field · resting",
        "Angle field · editing",
        "Angle field · invalid",
        "Angle field · disabled",
        "Angle stepper · enabled",
        "Angle stepper · disabled",
        "Straighten toggle · off",
        "Straighten toggle · on",
        "Straighten toggle · disabled",
        "Mode menu · first option",
        "Mode menu · last option",
        "Mode menu · disabled",
        "Colour swatch · resting",
        "Colour swatch · open",
        "Colour swatch · disabled",
        "Colour picker · resting",
        "Colour picker · dragging",
        "Colour picker · disabled",
        "Curve · resting",
        "Curve · selected point dragging",
        "Curve · histogram and two channels",
        "Curve · disabled",
        "Named vector icons · 12 and 16 points",
        "Basic · expanded module section",
        "Module bands · collapsed, unavailable, collapsed group",
        "Colour mixer · tab row per selected tab",
        "Neutral picker · labelled buttons",
        "Truncation · band hints and history labels on one line",
        "Transforms · icon-button row",
        "Crop and straighten · drafting",
        "Pixel · field rows",
        "Crop and straighten · idle",
    ];
    let states = gallery_states();
    assert_eq!(
        states.len(),
        NAMES.len(),
        "every gallery state needs one caption"
    );
    NAMES.into_iter().zip(states).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn gallery_builds_every_widget_state_without_panicking() {
        let states = super::gallery_named_states();
        assert_eq!(states.len(), 73);
    }
}
