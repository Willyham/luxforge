//! The desktop window's fixed layout, as plain numbers: the bars' heights, the side panels'
//! widths, the rules between the regions, the photograph's inset at Fit, and the arithmetic over
//! them that says how much of the window the photo surface gets.
//!
//! Everything here is framework-free, so the view model (`state/`), the view (`view/`) and the
//! update layer (`app/`) read the same numbers: the view lays the shell out with them, and the view
//! model and the update layer size the clipping overlay, the Fit percentage and the proxy bounds
//! with them before a frame is laid out.

/// The title bar's fixed height above the 1 px rule under it: 44 pt with the rule, as the default
/// board draws it.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 43.0;
/// The state panel's fixed width, at the layout's left edge.
pub(crate) const STATE_PANEL_WIDTH: f32 = 240.0;
/// The tools panel's fixed width, at the layout's right edge.
pub(crate) const TOOLS_PANEL_WIDTH: f32 = 300.0;
/// The status bar's fixed height under the 1 px rule over it: 26 pt with the rule.
pub(crate) const STATUS_BAR_HEIGHT: f32 = 25.0;
/// The width of each rule between the shell's regions: under the title bar, over the status bar
/// and beside each open side panel.
pub(crate) const DIVIDER_WIDTH: f32 = 1.0;

/// The photograph's inset from the canvas at Fit, at the top and on both sides.
pub(crate) const FIT_INSET_EDGE: f32 = 20.0;
/// The photograph's inset from the canvas bottom at Fit: the mode strip, its inset and a gap, so at
/// Fit no pixel of the photograph lies under the strip in either orientation.
pub(crate) const FIT_INSET_BOTTOM: f32 = 56.0;
/// The Fit padding's total horizontal and vertical inset, which is what the Fit arithmetic takes
/// off the photo surface.
pub(crate) const FIT_INSET: (f32, f32) = (2.0 * FIT_INSET_EDGE, FIT_INSET_EDGE + FIT_INSET_BOTTOM);

/// The photo surface's logical size for one window and panel configuration: the window minus the
/// title bar, the status bar, whichever panels are open and the rules beside them. It is arithmetic
/// over the layout constants, not a measurement, so the overlay's cell grid and the proxy bounds can
/// be decided before a frame is laid out.
pub(crate) fn photo_surface(
    window: (f32, f32),
    state_panel: bool,
    tools_panel: bool,
) -> (f32, f32) {
    let mut width = window.0;
    if state_panel {
        width -= STATE_PANEL_WIDTH + DIVIDER_WIDTH;
    }
    if tools_panel {
        width -= TOOLS_PANEL_WIDTH + DIVIDER_WIDTH;
    }
    // Two horizontal rules, one under the title bar and one over the status bar.
    let height = window.1 - TITLE_BAR_HEIGHT - STATUS_BAR_HEIGHT - 2.0 * DIVIDER_WIDTH;
    (width.max(0.0), height.max(0.0))
}

/// The physical x range of the canvas region inside a captured frame, so evidence can prove the
/// image was drawn where the layout puts it: the state panel's width from the left edge when it is
/// open, and the tools panel's width taken off the right edge when it is open.
pub(crate) fn surface_columns(
    logical_width: f32,
    scale: f32,
    state_panel: bool,
    tools_panel: bool,
) -> [u32; 2] {
    let left = if state_panel { STATE_PANEL_WIDTH } else { 0.0 };
    let right_edge = if tools_panel {
        logical_width - TOOLS_PANEL_WIDTH
    } else {
        logical_width
    };
    [
        (left * scale).round() as u32,
        (right_edge * scale).round() as u32,
    ]
}

/// The canvas region inside a captured frame as `[left, top, right, bottom]` physical pixels: the
/// area between the panels' rules and between the rules under the title bar and over the status
/// bar. At a percentage zoom this is exactly the scrollable the photograph pans in, so evidence can
/// map a captured pixel back to the source pixel the zoom and the pan put there.
pub(crate) fn canvas_rect(
    logical: (f32, f32),
    scale: f32,
    state_panel: bool,
    tools_panel: bool,
) -> [u32; 4] {
    let left = if state_panel {
        STATE_PANEL_WIDTH + DIVIDER_WIDTH
    } else {
        0.0
    };
    let right = if tools_panel {
        logical.0 - TOOLS_PANEL_WIDTH - DIVIDER_WIDTH
    } else {
        logical.0
    };
    let top = TITLE_BAR_HEIGHT + DIVIDER_WIDTH;
    let bottom = logical.1 - STATUS_BAR_HEIGHT - DIVIDER_WIDTH;
    [left, top, right, bottom].map(|edge| (edge * scale).round().max(0.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_shrinks_by_exactly_the_open_panels() {
        let window = (1440.0, 900.0);
        let divider = DIVIDER_WIDTH;
        let both = photo_surface(window, true, true);
        assert_eq!(
            both,
            (
                1440.0 - STATE_PANEL_WIDTH - TOOLS_PANEL_WIDTH - 2.0 * divider,
                900.0 - TITLE_BAR_HEIGHT - STATUS_BAR_HEIGHT - 2.0 * divider
            )
        );
        let none = photo_surface(window, false, false);
        assert_eq!(none.0, 1440.0);
        assert_eq!(none.1, both.1, "the panels never change the height");
        assert!(photo_surface((10.0, 10.0), true, true).0 >= 0.0);
    }
}
