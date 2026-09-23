//! The status bar model: the last message, who else is connected and what the renderer is doing.
use crate::state::Inputs;
use lightwell_core::Zoom;

/// How long the frame on the photo surface took to render, as the preview worker measured it for
/// that frame's own phase ([`lightwell_core::PreviewResult::render_ms`]). It travels with the
/// frame: a zoom that hands a retained frame back to the surface brings that frame's own time with
/// it, so the figure is always the picture on screen and never the time since some earlier request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderTime {
    pub(crate) ms: f64,
    /// The frame is the display-size proxy rather than the exact full-resolution render.
    pub(crate) proxy: bool,
}

impl RenderTime {
    /// "Rendered in 12 ms (proxy)", or "Rendered in 85 ms" for the exact render. A frame faster
    /// than half a millisecond says so rather than claiming zero.
    pub(crate) fn text(self) -> String {
        let figure = if self.ms < 0.5 {
            "<1".to_owned()
        } else {
            format!("{}", self.ms.round() as i64)
        };
        let phase = if self.proxy { " (proxy)" } else { "" };
        format!("Rendered in {figure} ms{phase}")
    }
}

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
            match inputs.render {
                Some(time) => time.text(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_render_time_names_the_frame_it_describes() {
        assert_eq!(
            RenderTime {
                ms: 12.4,
                proxy: true
            }
            .text(),
            "Rendered in 12 ms (proxy)"
        );
        assert_eq!(
            RenderTime {
                ms: 85.5,
                proxy: false
            }
            .text(),
            "Rendered in 86 ms"
        );
        // A tiny frame is not "0 ms".
        assert_eq!(
            RenderTime {
                ms: 0.2,
                proxy: true
            }
            .text(),
            "Rendered in <1 ms (proxy)"
        );
        assert_eq!(
            RenderTime {
                ms: 0.5,
                proxy: false
            }
            .text(),
            "Rendered in 1 ms"
        );
    }
}
