//! The title bar model: what is open, the view controls and the things that act on the whole photo.
use crate::state::Inputs;

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TitleBarModel {
    pub(crate) file_name: Option<String>,
    pub(crate) dimensions: Option<(u32, u32)>,
    /// The zoom field's text as typed.
    pub(crate) zoom_text: String,
    pub(crate) fit_selected: bool,
    pub(crate) hundred_selected: bool,
    /// A photograph is open, so the view controls act on something.
    pub(crate) can_view: bool,
    pub(crate) can_open: bool,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    pub(crate) state_panel_open: bool,
    pub(crate) tools_panel_open: bool,
    /// Compare is holding the Original entry's preview.
    pub(crate) compare_held: bool,
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
        fit_selected: matches!(zoom, lightwell_core::Zoom::Fit),
        hundred_selected: matches!(zoom, lightwell_core::Zoom::Percent { value } if *value == 100.0),
        can_view: inputs.state.is_some(),
        can_open: inputs.can_open,
        can_undo: editable,
        can_redo: editable,
        state_panel_open: inputs.session.workspace.state_panel,
        tools_panel_open: inputs.session.workspace.tools_panel,
        compare_held: inputs.compare_held,
    }
}
