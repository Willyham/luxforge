//! The renderer report.
use luxforge_core::ClientSession;

/// The owner's answer to the desktop's report of which renderer draws its picture, and Iced's name
/// for the adapter its window draws with. Handled in `app/renderer.rs`.
#[derive(Clone, Debug)]
pub(crate) enum RendererMessage {
    /// The reporting client's session, which carries the reported renderer, or why the owner
    /// could not take the report. Boxed, so the adapter's name does not carry a session's size.
    Reported(Result<Box<ClientSession>, String>),
    /// The adapter the window's renderer draws with, `name` on `backend` as Iced's system
    /// information names them, for the GPU tile worker.
    Adapter { backend: String, name: String },
}
