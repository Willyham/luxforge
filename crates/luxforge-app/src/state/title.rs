//! The title bar model: what is open, the view controls and the things that act on the whole photo.
use crate::{
    layout,
    state::{Inputs, MenuTarget, canvas::ZoomView, histogram},
};
use luxforge_core::{SourceKind, Zoom};

/// Which segment of the [Fit, 100%, percentage] view control is selected: a typed percentage
/// other than 100 selects the third, which shows it.
pub(crate) const SEGMENT_FIT: usize = 0;
pub(crate) const SEGMENT_HUNDRED: usize = 1;
pub(crate) const SEGMENT_PERCENT: usize = 2;

/// Open's tooltip, with the shortcut the keymap gives it on this platform.
pub(crate) const OPEN_TOOLTIP: &str = if cfg!(target_os = "macos") {
    "Open (\u{2318}O)"
} else {
    "Open (Ctrl+O)"
};

/// Export's tooltip, with the shortcut the keymap gives its first item on this platform.
pub(crate) const EXPORT_TOOLTIP: &str = if cfg!(target_os = "macos") {
    "Export JPEG (\u{2318}E)"
} else {
    "Export JPEG (Ctrl+E)"
};

/// The Export menu's two items, in order: what each is labelled and whether it keeps metadata.
/// The palette offers the same two, under the same labels.
pub(crate) const EXPORT_ITEMS: [(&str, bool); 2] = [
    ("Export JPEG\u{2026}", false),
    ("Export JPEG, keep metadata\u{2026}", true),
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TitleBarModel {
    pub(crate) developer: bool,
    pub(crate) can_open_gallery: bool,
    pub(crate) file_name: Option<String>,
    /// The displayed photograph's dimensions and the source format the core reports:
    /// `3389 × 4236 · JPEG`.
    pub(crate) identity: Option<String>,
    /// The zoom field's text as typed.
    pub(crate) zoom_text: String,
    /// The zoom field is open for typing in place of the percentage segment.
    pub(crate) zoom_editing: bool,
    /// What the percentage segment reads: the effective percentage of the zoom on screen, `18%`
    /// at Fit, or `%` before a photograph gives Fit a size.
    pub(crate) zoom_percent: String,
    /// Which of [Fit, 100%, percentage] the session's zoom selects.
    pub(crate) zoom_segment: usize,
    /// A photograph is open, so the view controls act on something.
    pub(crate) can_view: bool,
    pub(crate) can_toggle_panels: bool,
    pub(crate) can_open: bool,
    /// The Export button is enabled.
    pub(crate) can_export: bool,
    /// The Export button's menu is open under it.
    pub(crate) export_menu_open: bool,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    pub(crate) state_panel_open: bool,
    pub(crate) tools_panel_open: bool,
    /// Compare is holding the Original entry's preview.
    pub(crate) compare_held: bool,
    /// Both clipping overlays are on, so the bar's Clipping toggle reads as selected. `J` and this
    /// button drive the pair together; the two triangles drive them one at a time.
    pub(crate) clipping_on: bool,
    /// The window fills the screen, so the bar starts at its ordinary inset: macOS hides the
    /// traffic lights there.
    pub(crate) fullscreen: bool,
}

/// A percentage as the view control and the status bar print it: whole above 10%, where a tenth
/// is noise, and to one decimal below, where it is not.
pub(crate) fn percent_text(value: f32) -> String {
    if value >= 10.0 {
        format!("{}%", value.round() as i64)
    } else {
        let tenths = (value * 10.0).round() / 10.0;
        format!("{tenths}%")
    }
}

/// How many physical pixels one source pixel of the displayed photograph covers, as a percentage:
/// what Fit comes to for this window, these panels and this display, or the percentage itself.
/// `None` before a photograph gives Fit a size.
pub(crate) fn effective_percent(inputs: &Inputs<'_>) -> Option<f32> {
    match inputs.session.preview.view.zoom {
        Zoom::Percent { value } => Some(value),
        Zoom::Fit => {
            let source = inputs.dimensions?;
            let workspace = &inputs.session.workspace;
            let surface = layout::photo_surface(
                inputs.view_state.window,
                workspace.state_panel,
                workspace.tools_panel,
                inputs.develop.strip_shown()
                    && inputs.select.shown == crate::state::select::Shown::Develop,
            );
            let (width, _) = histogram::displayed_size(
                ZoomView::Fit,
                source,
                surface,
                inputs.view_state.scale_factor,
                layout::FIT_INSET,
            )?;
            Some(width / source.0 as f32 * 100.0)
        }
    }
}

/// The dimensions and the source's format, as far as the core reports them. The core names the
/// format of the source it decoded and no colour space, so none is shown.
fn identity(inputs: &Inputs<'_>) -> Option<String> {
    // A cached preview drawn while a photograph of the development set prepares is the preview's
    // size, not the photograph's: nothing is said until its render is on screen.
    if inputs.develop.preview.is_some() {
        return None;
    }
    let (width, height) = inputs.dimensions?;
    let mut identity = format!("{width} \u{d7} {height}");
    if let Some(state) = inputs.document.state.as_ref() {
        identity.push_str(match state.asset.source {
            SourceKind::Jpeg => " \u{b7} JPEG",
            SourceKind::Raw { .. } => " \u{b7} RAW",
        });
    }
    Some(identity)
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> TitleBarModel {
    let editable = inputs.edit_refusal.is_none();
    let interacting = !inputs.mask_tool_owns_controls();
    // An open draft refuses Undo and Redo, so neither is offered while it is.
    let navigable = editable && inputs.history_refusal.is_none();
    let zoom = &inputs.session.preview.view.zoom;
    TitleBarModel {
        developer: inputs.developer,
        // The gallery's one refusal already waits for a request in flight.
        can_open_gallery: inputs.developer
            && inputs.gallery_refusal.is_none()
            && !inputs.compare_held,
        // While a photograph of the development set is being switched to, its name.
        file_name: inputs
            .document
            .state
            .as_ref()
            .and_then(|state| {
                state
                    .asset
                    .locator
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .or_else(|| inputs.develop.switching.clone()),
        identity: identity(inputs),
        zoom_text: inputs.view_state.zoom.clone(),
        zoom_editing: inputs.view_state.zoom_editing && interacting,
        zoom_percent: effective_percent(inputs)
            .map(percent_text)
            .unwrap_or_else(|| "%".to_owned()),
        zoom_segment: match zoom {
            Zoom::Fit => SEGMENT_FIT,
            Zoom::Percent { value } if *value == 100.0 => SEGMENT_HUNDRED,
            Zoom::Percent { .. } => SEGMENT_PERCENT,
        },
        can_view: inputs.document.state.is_some() && interacting,
        can_toggle_panels: interacting,
        can_open: inputs.can_open,
        can_export: inputs.can_export,
        export_menu_open: inputs.can_export
            && matches!(inputs.view_state.menu.as_ref(), Some(MenuTarget::Export)),
        // The core answers an undo with no parent entry, or a redo with nothing undone, as a no-op,
        // so neither is offered then.
        can_undo: navigable
            && inputs
                .document
                .state
                .as_ref()
                .is_some_and(|state| state.current_entry.undo_parent.is_some()),
        can_redo: navigable
            && inputs
                .document
                .state
                .as_ref()
                .is_some_and(|state| !state.redo.is_empty()),
        state_panel_open: inputs.session.workspace.state_panel,
        tools_panel_open: inputs.session.workspace.tools_panel,
        compare_held: inputs.compare_held,
        clipping_on: inputs.session.workspace.clip_shadows
            && inputs.session.workspace.clip_highlights,
        fullscreen: inputs.view_state.fullscreen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percentage_is_whole_above_ten_and_to_a_tenth_below() {
        assert_eq!(percent_text(18.2), "18%");
        assert_eq!(percent_text(100.0), "100%");
        assert_eq!(percent_text(1600.0), "1600%");
        assert_eq!(percent_text(9.96), "10%");
        assert_eq!(percent_text(6.25), "6.3%");
        assert_eq!(percent_text(5.0), "5%");
    }
}
