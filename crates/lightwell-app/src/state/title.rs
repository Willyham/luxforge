//! The title bar model: what is open, the view controls and the things that act on the whole photo.
use crate::state::Inputs;

/// Which segment of the [Fit, 100%] view control is selected. A typed percentage selects neither,
/// so the control never claims a zoom the session does not hold.
pub(crate) const SEGMENT_FIT: usize = 0;
pub(crate) const SEGMENT_HUNDRED: usize = 1;
/// No segment: an index the control can never match.
pub(crate) const SEGMENT_NONE: usize = usize::MAX;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TitleBarModel {
    pub(crate) file_name: Option<String>,
    pub(crate) dimensions: Option<(u32, u32)>,
    /// The zoom field's text as typed.
    pub(crate) zoom_text: String,
    /// Which of [Fit, 100%] the session's zoom selects, or [`SEGMENT_NONE`].
    pub(crate) zoom_segment: usize,
    /// A photograph is open, so the view controls act on something.
    pub(crate) can_view: bool,
    pub(crate) can_open: bool,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    pub(crate) state_panel_open: bool,
    pub(crate) tools_panel_open: bool,
    /// Compare is holding the Original entry's preview.
    pub(crate) compare_held: bool,
    /// Both clipping overlays are on, so the bar's Clipping toggle reads as selected. `J` and this
    /// button drive the pair together; the two triangles drive them one at a time.
    pub(crate) clipping_on: bool,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> TitleBarModel {
    let editable = inputs.state.is_some() && inputs.session.preview.can_edit() && !inputs.busy;
    let zoom = &inputs.session.preview.view.zoom;
    TitleBarModel {
        file_name: inputs.state.and_then(|state| {
            state
                .asset
                .locator
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }),
        dimensions: inputs.dimensions,
        zoom_text: inputs.zoom.to_owned(),
        zoom_segment: match zoom {
            lightwell_core::Zoom::Fit => SEGMENT_FIT,
            lightwell_core::Zoom::Percent { value } if *value == 100.0 => SEGMENT_HUNDRED,
            lightwell_core::Zoom::Percent { .. } => SEGMENT_NONE,
        },
        can_view: inputs.state.is_some(),
        can_open: inputs.can_open,
        can_undo: editable,
        can_redo: editable,
        state_panel_open: inputs.session.workspace.state_panel,
        tools_panel_open: inputs.session.workspace.tools_panel,
        compare_held: inputs.compare_held,
        clipping_on: inputs.session.workspace.clip_shadows
            && inputs.session.workspace.clip_highlights,
    }
}
