//! The open photograph's residency. Handled in `app/residency.rs`.
use crate::app::{residency::Why, tasks::PreviewPayload};
use luxforge_core::{AssetId, SourceRelease};

#[derive(Clone, Debug)]
pub(crate) enum ResidencyMessage {
    /// Select has been showing for the grace period, since the showing numbered here.
    SelectIdle(u64),
    /// The system reported memory pressure. Only macOS reports it, so elsewhere only a test sends
    /// it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Pressure,
    /// The owner answered a release of the photograph's prepared source, asked under the open
    /// generation numbered here.
    SourceReleased {
        asset: AssetId,
        why: Why,
        generation: u64,
        release: SourceRelease,
    },
    /// The displayed entry's preview Develop asked for on taking the photograph's pixels back, its
    /// original prepared again.
    Prepared(Result<Box<PreviewPayload>, String>),
    /// The allocator returned its freed pages, or could not, after this long.
    Relieved {
        why: &'static str,
        result: Result<luxforge_process::memory::Relief, luxforge_process::Unavailable>,
        elapsed_ms: f64,
    },
}
