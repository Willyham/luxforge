//! Evidence mode.
use crate::app::tasks::HostAnswer;
use serde_json::Value;

/// Evidence mode: its timers, frame captures and the host methods its scripts call. Handled in
/// `app/evidence.rs`.
#[derive(Clone, Debug)]
pub(crate) enum EvidenceMessage {
    /// The evidence deadline check.
    Tick,
    /// The one gated deadline of a `view_idle` step, before any evidence capture can redraw.
    ViewIdleDeadline,
    /// The end of an `idle` step's settle or of its window.
    IdleDeadline,
    /// One tick of a paced evidence slider step: send its next value. Exists only while a paced
    /// step has values left to send, which is also when the subscription that produces it exists.
    PacedSliderTick,
    /// One tick of a **paced stroke** step: the next pointer position of a scripted brush stroke,
    /// sent in real time rather than with the whole path at once. It exists for the same reason
    /// `PacedSliderTick` does — a gesture delivered all at once measures the driver's coalescing and
    /// not the editor's own latency — and a stroke is the one gesture whose positions arrive that way
    /// from a hand.
    PacedStrokeTick,
    /// A scripted double-click's second press, once its gap has passed. Exists only while a
    /// double-click step waits for it, which is also when the timer that produces it exists.
    DoubleClickSecond,
    /// Capture the frame the next redraw presents.
    Capture,
    Captured(iced::window::Screenshot),
    /// One captured frame was written to the evidence directory.
    Saved(Result<Value, String>),
    /// A host method an evidence script called directly answered, with the preset library read
    /// after it when the method was one of the library's own.
    HostAnswered(Result<Box<HostAnswer>, String>),
    /// The edit an `agent` step sent through the run's second client answered.
    AgentAnswered(Result<Value, String>),
    /// Iced's name for the adapter that draws the window and its backend, which an enumeration of
    /// that backend, off the update loop, identifies further.
    Info(iced::system::Information),
    /// The adapter that draws the window as the enumeration found it, or `None` where it found no
    /// adapter of that backend and name: recorded with every captured frame.
    Adapter(
        Box<(
            iced::system::Information,
            Option<luxforge_ui::adapters::Adapter>,
        )>,
    ),
    /// The GPU identity hook's boundary, held from the frame on screen off the UI thread, or
    /// `None` when that frame could not be held; see `app/gpu_identity.rs`.
    GpuBoundary(Option<luxforge_ui::photo_surface::GpuBoundary>),
}
