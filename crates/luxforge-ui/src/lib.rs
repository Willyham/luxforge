//! Luxforge's widget library: the Develop workspace's visual language and generated controls.
//!
//! This crate depends on Iced only. It never depends on `luxforge-core`, and no test here links
//! it. That boundary is deliberate, for three reasons. First, it is enforced by the compiler
//! rather than by review: with no `luxforge-core` type in scope, nothing in this crate can reach
//! the catalog owner's authoritative state, validate a parameter against a module descriptor, or
//! construct an `edit.*` request — every widget here is therefore structurally unable to hold
//! editing logic, and takes plain data structs, enums and message values or closures instead.
//! Second, it is a compile-unit boundary: a styling or layout change here rebuilds this crate and
//! `luxforge-app`, never `luxforge-core`, so widget churn never lengthens the core's own build.
//! Third, the split keeps UI-independent ownership in the core while linking into the same
//! desktop binary. The photo surface starts one idle-blocked GPU retirement worker per pipeline;
//! it holds no authoritative editing state and wakes the desktop when resources retire or a photo
//! draw enters or leaves a temporary stale state.

pub mod geometry;
pub mod photo_surface;
pub mod theme;
mod widgets;

pub use photo_surface::{
    Frame, Placement, RegionFrame, RegionOverlay, RegionQuality, SurfaceDiagnostics, Turn,
    photo_surface, region_texture_admissible, set_surface_waker, stage_surface,
    surface_diagnostics, surface_retirement_pending, viewport_surface,
};
pub use widgets::*;

mod gallery;
mod gallery_components;
mod gallery_masks;
mod gallery_performance;

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

/// The components board's pages in draw order, each a named group of named states. Small pages
/// keep every example visible in a native 1440×1000 background capture: the two large canvases get
/// their own pages, while related compact states stay together.
#[doc(hidden)]
pub const GALLERY_PAGES: [(&str, &[&str]); 13] = [
    (
        "Sliders and sections",
        &[
            "Highlights · resting slider",
            "Exposure · dragging slider",
            "Contrast · editing slider value",
            "Temperature · invalid slider value",
            "Saturation · disabled slider",
            "Basic · expanded section",
            "Detail · collapsed section",
            "Lens profile · unavailable section",
            "Tone · subgroup with reset",
        ],
    ),
    (
        "Actions, history and performance",
        &[
            "Rotate right · icon button",
            "Crop · selected icon button",
            "Reset · disabled icon button",
            "View · zoom control at Fit",
            "Undo and redo · enabled and disabled",
            "Versions · compact chips",
            "Copy · small icon button",
            "Crop ratio · segmented choice",
            "Warm · selected chip",
            "Print draft · ordinary chip",
            "Shadows +25 · current history row",
            "Vibrance +15 · previewed history row",
            "Crop 4:5 · ordinary history row",
            "Rotate right · branch history row",
            "Performance heading · expanded with a caption, collapsed",
            "Metric rows · one sample, part of a window, unavailable",
            "Job rows · running, with a long detail, with progress, finished",
        ],
    ),
    (
        "Notices, canvas chrome and menus",
        &[
            "Original not found · error notice",
            "Changed elsewhere · warning notice",
            "Preview is stale · neutral notice",
            "Crop · floating bar",
            "Crop · draft bar",
            "Double-click · reset wrapper",
            "Canvas · mode strip",
            "Canvas · mode strip, Crop and Thirds on",
            "JSON request · inline menu",
        ],
    ),
    (
        "Histogram and typography",
        &[
            "Histogram · ready",
            "Histogram · stale",
            "Histogram · empty",
            "Histogram · pending",
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
        ],
    ),
    (
        "Rails and number fields",
        &[
            "Hue rail · resting",
            "Hue rail · below soft range",
            "Hue rail · above soft range",
            "Angle field · resting",
            "Angle field · editing",
            "Angle field · invalid",
            "Angle field · disabled",
            "Angle stepper · enabled",
            "Angle stepper · disabled",
            "Angle stepper · rail, dragging",
        ],
    ),
    (
        "Toggles, choices and swatches",
        &[
            "Straighten toggle · off",
            "Straighten toggle · on",
            "Straighten toggle · disabled",
            "Mode menu · first option",
            "Mode menu · last option",
            "Mode menu · disabled",
            "Colour swatch · resting",
            "Colour swatch · open",
            "Colour swatch · disabled",
        ],
    ),
    (
        "Colour picker",
        &[
            "Colour picker · resting",
            "Colour picker · dragging",
            "Colour picker · disabled",
        ],
    ),
    (
        "Curve points",
        &["Curve · resting", "Curve · selected point dragging"],
    ),
    (
        "Curve channels and named vector icons",
        &[
            "Curve · histogram and two channels",
            "Curve · disabled",
            "Named vector icons · 12 and 16 points",
        ],
    ),
    (
        "Tabs, labelled buttons and truncation",
        &[
            "Tab row · each tab selected",
            "Labelled buttons · resting, selected, disabled, primary",
            "Section headers · truncated hints and reason, scoped",
            "History rows · truncated labels, actor and tag kept",
        ],
    ),
    (
        "Mask rows and controls",
        &[
            "Mask rows · resting, open, hidden and neutral",
            "Mask rows · menu open, renamed in place, disabled",
            "Coverage thumbnails · grid, sampled down, pending",
            "Component rows · fixed Add, inverted, Subtract, Intersect, each kind",
            "Component rows · selected, hovered, menu open, disabled, a refusal",
            "Mode control · each mode, fixed, disabled",
            "Overlay · tint, selection on black, off",
        ],
    ),
    (
        "Mask menus, fields and swatches",
        &[
            "New mask · button, count and kind menu",
            "Add row · Add component as Subtract, New mask at the limit",
            "Group rules · open mask with menu, armed brush",
            "Component fields · a radial's six in two columns",
            "Component fields · typing and refused",
            "Toggle rows · with hints, plain, disabled",
            "Colour range swatches · three of five, picking, full",
            "Brush strokes · deletable and refused",
        ],
    ),
    (
        "Range and mask draft bar",
        &[
            "Luminance range · resting, shoulders open",
            "Luminance range · high edge dragging",
            "Range · disabled, no shoulders",
            "Face · mask draft bar, brush with Done",
        ],
    ),
];

/// Builds gallery page `page`'s states in draw order, each with its 1-based number on the whole
/// board and its name, or `None` past the last page.
#[doc(hidden)]
pub fn gallery_page(page: usize) -> Option<Vec<(usize, &'static str, iced::Element<'static, ()>)>> {
    let (_, names) = GALLERY_PAGES.get(page)?;
    let states = gallery_states();
    assert_eq!(
        states.len(),
        GALLERY_PAGES
            .iter()
            .map(|(_, names)| names.len())
            .sum::<usize>(),
        "every gallery state needs one caption"
    );
    let first: usize = GALLERY_PAGES[..page]
        .iter()
        .map(|(_, names)| names.len())
        .sum();
    Some(
        names
            .iter()
            .zip(states.into_iter().skip(first))
            .enumerate()
            .map(|(offset, (name, widget))| (first + offset + 1, *name, widget))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{GALLERY_PAGES, gallery_page};

    #[test]
    fn gallery_builds_every_widget_state_without_panicking() {
        let mut next = 1;
        for (page, (_, names)) in GALLERY_PAGES.iter().enumerate() {
            let states = gallery_page(page).unwrap();
            assert!(!states.is_empty(), "no page is empty");
            assert_eq!(states.len(), names.len());
            for (number, name, _) in &states {
                assert_eq!(*number, next, "{name} follows its predecessor");
                next += 1;
            }
        }
        assert_eq!(next - 1, 99);
        assert!(gallery_page(GALLERY_PAGES.len()).is_none());
    }
}
