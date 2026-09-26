//! The status bar model: the last message, the pointer readout, who else is connected and what the
//! renderer is doing.
use crate::state::{Inputs, histogram};
use luxforge_core::Zoom;

/// How long the frame on the photo surface took to render, as the preview worker measured it for
/// that frame's own phase ([`luxforge_core::PreviewResult::render_ms`]). It travels with the
/// frame: a zoom that hands a retained frame back to the surface brings that frame's own time with
/// it, so the figure is always the picture on screen and never the time since some earlier request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderTime {
    pub(crate) ms: f64,
    /// The frame is the display-size proxy rather than the exact full-resolution render.
    pub(crate) proxy: bool,
    /// The frame approximates a drafted RAW white balance on planes developed at another one
    /// ([`luxforge_core::PreviewResult::approximate_white_balance`]).
    pub(crate) approximate: bool,
}

impl RenderTime {
    /// "Rendered in 12 ms (proxy)", "Rendered in 85 ms" for the exact render, and "(proxy,
    /// approximate)" or "(approximate)" for a drafted RAW white balance approximated on the
    /// developed planes. A frame faster than half a millisecond says so rather than claiming zero.
    pub(crate) fn text(self) -> String {
        let figure = if self.ms < 0.5 {
            "<1".to_owned()
        } else {
            format!("{}", self.ms.round() as i64)
        };
        let phase = match (self.proxy, self.approximate) {
            (true, true) => " (proxy, approximate)",
            (true, false) => " (proxy)",
            (false, true) => " (approximate)",
            (false, false) => "",
        };
        format!("Rendered in {figure} ms{phase}")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBarModel {
    pub(crate) message: String,
    /// The three output codes under the pointer and their pixel, while the pointer is over the
    /// photograph; `None` otherwise. The view keeps a fixed slot for it either way, so nothing else
    /// in the bar moves as it comes and goes.
    pub(crate) readout: Option<String>,
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
        readout: inputs.readout.map(histogram::readout_text),
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
        let time = |ms: f64, proxy: bool, approximate: bool| RenderTime {
            ms,
            proxy,
            approximate,
        };
        assert_eq!(time(12.4, true, false).text(), "Rendered in 12 ms (proxy)");
        assert_eq!(time(85.5, false, false).text(), "Rendered in 86 ms");
        // A tiny frame is not "0 ms".
        assert_eq!(time(0.2, true, false).text(), "Rendered in <1 ms (proxy)");
        assert_eq!(time(0.5, false, false).text(), "Rendered in 1 ms");
        // A drafted RAW white balance approximated on the developed planes says so, at Fit and
        // at 100% alike.
        assert_eq!(
            time(9.2, true, true).text(),
            "Rendered in 9 ms (proxy, approximate)"
        );
        assert_eq!(
            time(140.0, false, true).text(),
            "Rendered in 140 ms (approximate)"
        );
    }
}
