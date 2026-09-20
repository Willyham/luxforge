//! The status bar model: the last message, who else is connected and what the renderer is doing.
use crate::state::Inputs;
use lightwell_core::Zoom;

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBarModel {
    pub(crate) message: String,
    /// Live API clients, so an agent's presence is visible.
    pub(crate) clients: usize,
    /// The render phase the last request reached.
    pub(crate) render: String,
    pub(crate) zoom_text: String,
    pub(crate) scale_text: String,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> StatusBarModel {
    StatusBarModel {
        message: inputs.status.to_owned(),
        clients: inputs.clients,
        render: inputs.phase.to_owned(),
        zoom_text: match inputs.session.preview.view.zoom {
            Zoom::Fit => "Fit".into(),
            Zoom::Percent { value } => format!("{value}%"),
        },
        scale_text: format!(
            "Display scale {:.2}×; 100% maps one source pixel to one physical framebuffer pixel",
            inputs.scale_factor
        ),
    }
}
