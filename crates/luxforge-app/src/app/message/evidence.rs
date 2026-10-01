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
    // ── catalog lane D: views and desktop ──
    /// The `pick.set` a Select `agent_pick` step sent through the run's second client answered.
    SelectAgentAnswered(Result<Value, String>),
    /// One press of a loupe `arrows` step: the first once the look-ahead is warm, the rest one per
    /// tick of the step's own timer, which exists only while presses remain.
    LoupeArrow,
    /// A display frame of a running `grid_scroll` step: scroll the grid on by the step's speed.
    GridScrollFrame(std::time::Instant),
    // ── end lane D ──
    /// The graphics backend, recorded with every captured frame.
    Info(iced::system::Information),
}
