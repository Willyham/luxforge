//! What a seam reports happened, in its own terms: a frame reached the surface, an answer came
//! back, a request was refused. A seam reports each one through [`Editor::outcome`] and knows
//! nothing about who is listening.
//!
//! The one listener is the evidence driver (`evidence.rs`), present only in an evidence run. It
//! matches an outcome to whatever the running script step waits for, records what a captured frame
//! reports and logs the outcome that ended each step. Without it an outcome is dropped where it is
//! reported: every variant is a few words or a borrow of something the seam already holds, so
//! reporting one costs an ordinary session nothing but the check.
use crate::app::{Editor, tasks::PerformanceRead};
use luxforge_core::{EntryId, HistoryEntry};
use serde_json::Value;

/// One thing that happened in a seam.
pub(crate) enum Outcome<'a> {
    /// A frame of the photograph reached the surface, or the exact phase of the proxy on screen
    /// landed: see [`Presented`].
    Presented(Presented),
    /// A preview failed; `newest` when it was the newest one asked for, so its failure is on the
    /// canvas in place of the frame that was asked for.
    PreviewFailed { newest: bool },
    /// The change just answered puts no new frame on screen: a commit that changed nothing and
    /// needs no render, or a draft discarded with no photograph open.
    NoNewFrame,
    /// The open request — an open, a commit, a refresh or a history selection — reached its
    /// outcome, `failed` or with its frame on screen.
    RequestEnded { failed: bool },
    /// A render of this entry was asked for: a refresh or a history selection.
    EntryRequested(&'a HistoryEntry),
    /// A frame rendered for this entry was made the photograph.
    EntryShown(&'a EntryId),
    /// The photograph was taken off the surface after a failure.
    Withdrawn,
    /// A view change asked for a new frame: of the photograph, or of the crop draft's input stage.
    FrameRequested(Requested),
    /// The open draft was refused or conflicted: its release or commit was refused, its newest
    /// value was refused with nothing else left to drain, or another client changed the stack
    /// under it. The frame on screen, with the reason in the status bar, is what happened.
    DraftRefused,
    /// The crop draft's wait for its input stage is over: the stage is on screen, or none will come
    /// because its render failed or was superseded, it could not be shown or the draft did not
    /// open.
    CropStage,
    /// A session round trip for a view or workspace change answered.
    SessionAnswered,
    /// The percent-zoom surface's scroll offset reached the owner and the owner answered.
    PanAnswered,
    /// The pointer readout's `render.sample` answered.
    ReadoutAnswered,
    /// A canvas pick ended without committing anything: its fields were filled, or it was refused
    /// with its reason in the status bar.
    PickEnded,
    /// A canvas pick sent its commit: what it did is the render that follows.
    PickCommitting,
    /// A preset library call and the listing after it answered, or the call was refused, with its
    /// reason when it failed.
    PresetsAnswered { failure: Option<&'a str> },
    /// A clipping overlay's texture reached the surface, or it failed, with the reason.
    ClippingOverlay { failure: Option<&'a str> },
    /// The mask tool's content map answered, before an unplaced tool can accept a gesture.
    MaskMap { available: bool },
    /// The mask overlay's coverage grid arrived for the frame on screen; `shown` when the surface
    /// took it.
    MaskGrid { shown: bool },
    /// The frame arrived without the coverage grid it asked for, for `reason`. `forced` when the
    /// open gesture showed the tint of its own accord, over a setting of `off`.
    MaskGridAbsent { forced: bool, reason: &'a str },
    /// A `mask.*` command the desktop sent was refused by the host.
    MaskCommandFailed(&'a str),
    /// The Performance section's read answered: the read, when it could be taken up.
    PerformanceRead(Option<Box<PerformanceRead>>),
    /// The Performance section started sampling again, so nothing read before counts.
    PerformanceRestarted,
    /// `export.plan` answered for the export in progress.
    ExportPlanned(&'a Value),
    /// `export.jpeg` queued the export in progress.
    ExportQueued(&'a Value),
    /// The export ended — written, failed or cancelled — or its request was refused: the job's
    /// last record when one was read, and the reason when it failed.
    ExportEnded {
        record: Option<&'a Value>,
        failure: Option<&'a str>,
    },
    // ── catalog lane D: views and desktop ──
    /// Nothing the Select workspace asked the owner for is in flight: the events, the view, its
    /// facets, the rows near the screen and a staleness check have all answered.
    SelectSettled,
    /// Long-running work's model was derived again: what the status bar, the sheet and the
    /// Performance rows show may have changed.
    LongWorkShown,
    // ── end lane D ──
}

/// What reached the photo surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presented {
    /// A whole frame with no draft open: an open, a committed render or a history selection.
    Photo,
    /// The exact phase of the proxy on screen, taken up without drawing it: the same picture, now
    /// with the numbers of the exact render.
    Exact,
    /// A visible region with no draft open. It proves the pixels in view, not the whole-frame
    /// histogram and stack a history selection or a commit is recorded with.
    Region,
    /// A frame of the open draft. `slider` for a slider gesture; `newest` when nothing newer of the
    /// draft is still coming: no `draft.set` or commit waiting and no newer preview asked for.
    Draft { slider: bool, newest: bool },
}

/// Which frame a view change asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Requested {
    /// The photograph's.
    Photo,
    /// The crop draft's input stage.
    CropStage,
}

impl Outcome<'_> {
    /// The outcome's name, as the evidence log records the one that ended a step.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Presented(Presented::Photo) => "presented_photo",
            Self::Presented(Presented::Exact) => "presented_exact",
            Self::Presented(Presented::Region) => "presented_region",
            Self::Presented(Presented::Draft { .. }) => "presented_draft",
            Self::PreviewFailed { .. } => "preview_failed",
            Self::NoNewFrame => "no_new_frame",
            Self::RequestEnded { .. } => "request_ended",
            Self::EntryRequested(_) => "entry_requested",
            Self::EntryShown(_) => "entry_shown",
            Self::Withdrawn => "withdrawn",
            Self::FrameRequested(_) => "frame_requested",
            Self::DraftRefused => "draft_refused",
            Self::CropStage => "crop_stage",
            Self::SessionAnswered => "session_answered",
            Self::PanAnswered => "pan_answered",
            Self::ReadoutAnswered => "readout_answered",
            Self::PickEnded => "pick_ended",
            Self::PickCommitting => "pick_committing",
            Self::PresetsAnswered { .. } => "presets_answered",
            Self::ClippingOverlay { .. } => "clipping_overlay",
            Self::MaskMap { .. } => "mask_map",
            Self::MaskGrid { .. } => "mask_grid",
            Self::MaskGridAbsent { .. } => "mask_grid_absent",
            Self::MaskCommandFailed(_) => "mask_command_failed",
            Self::PerformanceRead(_) => "performance_read",
            Self::PerformanceRestarted => "performance_restarted",
            Self::ExportPlanned(_) => "export_planned",
            Self::ExportQueued(_) => "export_queued",
            Self::ExportEnded { .. } => "export_ended",
            Self::SelectSettled => "select_settled",
            Self::LongWorkShown => "long_work_shown",
        }
    }
}

impl Editor {
    /// Report what just happened in a seam. Only an evidence run listens; otherwise this does
    /// nothing.
    pub(crate) fn outcome(&mut self, outcome: Outcome<'_>) {
        if self.evidence.is_some() {
            self.observe(outcome);
        }
    }
}
