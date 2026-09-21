//! The status bar model: the last message, who else is connected and what the renderer is doing.
use crate::state::Inputs;
use lightwell_core::Zoom;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBarModel {
    pub(crate) message: String,
    /// Live API clients, so an agent's presence is visible, or why the count is unknown.
    pub(crate) clients: String,
    /// What the renderer is doing, or how long the displayed frame took.
    pub(crate) render: String,
    pub(crate) zoom_text: String,
    /// What 100% means on this display.
    pub(crate) scale_text: String,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> StatusBarModel {
    StatusBarModel {
        message: inputs.status.to_owned(),
        clients: match inputs.clients {
            Some(1) => "1 client".into(),
            Some(count) => format!("{count} clients"),
            None => "live API unavailable".into(),
        },
        render: if inputs.rendering {
            "Rendering…".into()
        } else {
            match inputs.render_ms {
                Some(ms) => format!("Rendered in {} ms", ms.round() as i64),
                None => "Idle".into(),
            }
        },
        zoom_text: match inputs.session.preview.view.zoom {
            Zoom::Fit => "Fit".into(),
            Zoom::Percent { value } => format!("{value}%"),
        },
        scale_text: format!("@{:.2}×", inputs.scale_factor),
    }
}
