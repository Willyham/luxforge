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

pub use theme::{Derived, Ink, Mode, Palette, Theme, Token};

/// An element drawn in Luxforge's [`Theme`]: every widget here returns one, and the desktop's view
/// is built of them.
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, iced::Renderer>;

pub use photo_surface::{
    FirstDrawn, Frame, Placement, RegionOverlay, SurfaceDiagnostics, SurfaceId, Turn,
    mips_admissible, photo_surface, set_surface_waker, stage_surface, surface_diagnostics,
    surface_retirement_pending, viewport_surface,
};
pub use widgets::*;

mod gallery;
mod gallery_components;
mod gallery_masks;
mod gallery_performance;
mod gallery_select;
mod gallery_select_grid;
mod gallery_thumbnails;

/// Builds one instance of every widget in every state shown on the components board
/// (`docs/design/develop-workspace/components.png`), as `Element<'_, ()>` values, so a caller can
/// prove the whole set builds without panicking.
///
/// Hidden because it exists for unit checks and the real-app gallery evidence renderer,
/// not for reuse as part of the widget API.
#[doc(hidden)]
pub fn gallery_states() -> Vec<Element<'static, ()>> {
    gallery::gallery()
}

/// The components board's pages in draw order, each a named group of named states. Small pages
/// keep every example visible in a native 1440×1000 background capture: the two large canvases get
/// their own pages, while related compact states stay together.
#[doc(hidden)]
pub const GALLERY_PAGES: &[(&str, &[&str])] = &[
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
            "Not in the lens database · warning inline notice",
            "JPEG assumption · neutral inline notice",
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
    (
        "Zoom stops",
        &[
            "Zoom stops · resting on 100%",
            "Zoom stops · Fit between 50% and 100%",
            "Zoom stops · disabled",
        ],
    ),
    // -- Select: the thumbnail grid's pages.
    (
        "Select grid · cells and moments",
        &[
            "Cells · resting, selected, active, picked, in the catalog, a collapsed burst, offline, unreadable, loading",
            "Burst · wider than the view, wrapping in one frame, then a single",
            "Day and camera · a burst with its pick, a bracket from metadata with Pick all 3",
            "Bracket from previews · a collapsed burst beside it, singles under it",
        ],
    ),
    (
        "Select grid · 10,000 files and the catalog's cells",
        &[
            "10,000 files · scrolled to a day in the middle",
            "Catalog cells · edited, selected, active, offline",
        ],
    ),
    // -- end Select: the thumbnail grid's pages.
    //
    // -- Select: chrome's pages.
    (
        "Select sources and filters",
        &[
            "Sources · files by card, event and disk; the catalog by folder and collection",
            "Filter bar · over files, at rest and with conditions set",
            "Search fields · the panel's and the filter bar's, empty and typed",
            "Filter bar · over the catalog, search, set conditions, save",
            "Filter chip · the Group menu open",
        ],
    ),
    (
        "Select title bar and long-running work",
        &[
            "Workspace switch · Select and Develop, with Add a folder…",
            "Develop N · ready, busy, nothing picked, a large count",
            "Status bar job · with a total, without one, alone",
            "Performance rows · a count and estimate, no estimate yet, working",
            "Progress sheet · reading a card, with its count and estimate",
            "Progress sheet · no total yet, working",
        ],
    ),
    (
        "Select loupe and filmstrip",
        &[
            "Loupe info bar · moment, frame, exposure and source",
            "Moment frames · a window of six, frame 3 active and picked",
            "100% inset · a camera preview and a Luxforge development",
            "Moment frames · the middle of a 1,000-frame burst",
            "Region box · the 100% region under the pointer",
            "Filmstrip · the development set's first photograph",
            "Key hints · the loupe's keys",
            "Filmstrip · deep in a long set, two previews still loading",
        ],
    ),
    // -- end Select: chrome's pages.
];

/// Builds gallery page `page`'s states in draw order, each with its 1-based number on the whole
/// board and its name, or `None` past the last page.
#[doc(hidden)]
pub fn gallery_page(page: usize) -> Option<Vec<(usize, &'static str, Element<'static, ()>)>> {
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
        assert_eq!(next - 1, 129);
        assert!(gallery_page(GALLERY_PAGES.len()).is_none());
    }
}
