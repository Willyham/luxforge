//! The renderer report.
use luxforge_core::ClientSession;

/// The owner's answer to the desktop's report of which renderer draws its picture. Handled in
/// `app/renderer.rs`.
#[derive(Clone, Debug)]
pub(crate) enum RendererMessage {
    /// The reporting client's session, which carries the reported renderer, or why the owner
    /// could not take the report.
    Reported(Result<ClientSession, String>),
}
