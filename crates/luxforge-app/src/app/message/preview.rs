//! Preview presentation: the workers' wake, the quiet settle and planned preview jobs.
use crate::app::tasks::PreviewPayload;

/// Taking up what the preview worker finished and presenting it. Handled in `app/preview.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PreviewMessage {
    /// Take up what the preview and overlay workers have finished. Their wake produces it.
    Poll,
    /// A view-only request planned off the update loop. Its desired rectangle is re-read when
    /// admitted, so a pan that overtook planning never queues stale pixels.
    ViewLoaded {
        epoch: u64,
        intent: luxforge_core::PreviewIntent,
        result: Result<Box<luxforge_core::PreviewJob>, String>,
    },
    /// One gated quiet-interval wake for an accepted live gesture.
    QuietTick,
    /// A preview job and the session that selects it, read by something that did not set `busy`:
    /// a comparison, or the displayed entry again once a gesture ended without committing.
    Loaded(Result<Box<PreviewPayload>, String>),
    /// The stack on screen planned again for the Masks panel's thumbnails, on entering Mask mode.
    /// Handled in `app/thumbnails.rs`: its evaluation goes to the thumbnail worker or is dropped.
    ThumbnailSource(Result<Box<luxforge_core::PreviewJob>, String>),
}
