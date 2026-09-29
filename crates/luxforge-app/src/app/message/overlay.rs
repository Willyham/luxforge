//! The clipping overlays.

/// Which clipping overlay one toggle acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipEndpoint {
    Shadows,
    Highlights,
}

impl ClipEndpoint {
    /// The `workspace.set` field this endpoint's overlay is stored in.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Shadows => "clip_shadows",
            Self::Highlights => "clip_highlights",
        }
    }
}

/// The clipping overlay drawn over the photograph. Handled in `app/overlay.rs`; a derived overlay
/// and a mask's coverage grid reach the presenter in the update that takes them up, with no message
/// of their own.
#[derive(Clone, Debug)]
pub(crate) enum OverlayMessage {
    /// Turn one clipping overlay on or off. `None` toggles both together, which is what the title
    /// bar's Clipping button and `J` do; `Some` toggles the one triangle that was clicked.
    ToggleClipping(Option<ClipEndpoint>),
}
