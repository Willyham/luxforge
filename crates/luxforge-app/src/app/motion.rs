//! A drag tick the GPU does not draw (`docs/design/gpu-preview.md`, "A tick the GPU does not
//! draw"): its frame is held for a reason that passes within a tick or an upload, and otherwise
//! the reference renderer draws one whole frame of the drafted stack per tick, the latest winning.
//! A session the GPU does not draw at all drags on the display-size proxy instead.
//!
//! - **Proxied.** In a session whose GPU stage cannot draw at all — no adapter, which a launch with
//!   `--no-gpu-render` or a host with only a software adapter answers too, or a lost device — every
//!   tick asks the reference for the drafted stack's display-size proxy
//!   (`PreviewIntent::Interactive`): at the view's own bounds below 100%, and at Fit's at 100% and
//!   above, which the view magnifies. The queue's latest-wins slot bounds it: a newer tick replaces
//!   the pending job and never stops the proxy in progress. The release's reference frame is sharp
//!   at rest (owner, 2026-10-06).
//! - **Held.** `compiling` until the status bar's half-second threshold, `unchanged`, `unplannable`
//!   while no view is known (the tick's job was planned with no GPU preview), and the boundary,
//!   source and surface waits (`boundary-pending`, `boundary-uploading`, `source-uploading`,
//!   `source-missing`, `surface-pending`). The tick queues nothing and the frame on screen stays:
//!   the surface draws the tick's plan once the wait is over, or the next tick does. Every drag's
//!   first tick waits on its boundary this way, so no drag starts by queuing a whole frame. A hold
//!   that has lasted the same half second, from the first tick of its run, renders the newest
//!   held tick's reference frame after all, so a drag paused on a wait that does not pass still
//!   shows its value.
//! - **Rendered.** Any other reason — a layer the GPU cannot draw, a slot past the budget, the GPU
//!   stage refused — queues the tick's reference job: the whole drafted stack, its report, and its
//!   reduction to the view where the view draws the stage smaller than it is. One such job is in
//!   flight at a time and no tick supersedes it: a newer tick's job waits here, replacing any older
//!   one, and is queued once the frame in flight has been taken up, so a drag the reference draws
//!   shows a new frame as fast as the reference renders one. The status bar's notice names the
//!   reason ([`crate::state::status`]).
//!
//! Each waiting tick holds its job — the evaluation it was planned with, no pixels — and is let go
//! with its draft: a release, a cancel, another draft, or a tick the GPU draws.
use super::{Editor, gpu_preview::Tick};
use luxforge_core::{Draft, DraftId, PreviewIntent, PreviewJob};
use serde_json::json;
use std::time::{Duration, Instant};

/// The reasons a tick holds its frame for, which pass within a tick or an upload. `compiling` and
/// `unplannable` hold too, on their own conditions ([`holds`]).
const PASSING: [&str; 6] = [
    "unchanged",
    "boundary-pending",
    "boundary-uploading",
    "source-uploading",
    "source-missing",
    "surface-pending",
];

/// How long a run of held ticks holds before the newest one's reference frame is rendered: the
/// status bar's `compiling` threshold, which a compile is held to as well.
const HOLD_AT_MOST: Duration = crate::state::status::COMPILING_AFTER;

/// How often the desktop looks at a held tick again while it waits: whether the surface now draws
/// it, or the hold has lasted [`HOLD_AT_MOST`].
pub(super) const HOLD_LOOK: Duration = Duration::from_millis(50);

/// One tick of the open draft whose frame waits: held, or queued behind the frame in flight.
struct Waiting {
    draft: DraftId,
    revision: u64,
    /// When the run of held ticks it ends began.
    since: Instant,
    job: Box<PreviewJob>,
    /// The job is queued with phase timing, as an evidence run's mask ticks are.
    timed: bool,
}

/// The open draft's ticks the GPU does not draw.
#[derive(Default)]
pub(crate) struct Motion {
    /// The newest tick, holding the frame on screen for a reason that passes.
    held: Option<Waiting>,
    /// The newest tick's reference job, waiting for the one in flight to be taken up.
    next: Option<Waiting>,
    /// The generation of the reference job a tick queued, while it may still be in flight.
    in_flight: Option<u64>,
}

/// Whether a tick whose reason is `code` holds the frame, and for how long its reason has already
/// lasted: `compiling` for as long as its ticks have named it, short of the threshold; `unplannable`
/// only for a job planned with no GPU preview, whose view was not known; and the reasons that pass.
fn holds(code: &str, compiling_for: Option<Duration>, planned: bool) -> Option<Duration> {
    match code {
        "compiling" => Some(compiling_for.unwrap_or_default()),
        "unplannable" if !planned => Some(Duration::ZERO),
        code if PASSING.contains(&code) => Some(Duration::ZERO),
        _ => None,
    }
}

impl Editor {
    /// One tick of the open draft `set` that the GPU does not draw, with its preview `job`, planned
    /// with a GPU preview or not (`planned`): held, or its reference frame queued now or after the
    /// one in flight ([module documentation](self)). Answers the generation and queue time of a job
    /// queued now; `None` when the tick holds or waits.
    pub(crate) fn cpu_tick(
        &mut self,
        set: &Draft,
        job: PreviewJob,
        planned: bool,
    ) -> Option<(u64, Option<Instant>)> {
        // The tick was planned at the view as it is now: held or rendered, the view needs no plan
        // of its own.
        self.view_plan.dirty = false;
        let timed = self.log.diagnostics.is_some() && self.mask_gesture().is_some();
        // A session the GPU does not draw at all drags on the proxy, latest winning.
        if self.gpu_stage_refusal().is_some() {
            self.motion.held = None;
            self.motion.next = None;
            self.motion.in_flight = None;
            let mut job = job;
            job.intent = PreviewIntent::Interactive;
            return Some(self.request_preview_inner(job, timed));
        }
        let now = Instant::now();
        let reason = self.gpu_cpu_reason().map(|reason| {
            (
                reason.code.to_owned(),
                holds(reason.code, reason.compiling_for, planned),
            )
        });
        let (code, held_for) = match reason {
            Some((code, held_for)) => (Some(code), held_for),
            None => (None, None),
        };
        let waiting = |since| Waiting {
            draft: set.draft_id.clone(),
            revision: set.draft_revision,
            since,
            job: Box::new(job),
            timed,
        };
        if let Some(held_for) = held_for {
            let since = self
                .motion
                .held
                .as_ref()
                .filter(|held| held.draft == set.draft_id)
                .map_or(now.checked_sub(held_for).unwrap_or(now), |held| held.since);
            if now.saturating_duration_since(since) < HOLD_AT_MOST {
                // The older tick's reference frame is not wanted: the surface draws this one soon.
                self.motion.next = None;
                self.motion.held = Some(waiting(since));
                self.event("gpu_preview_tick", || {
                    json!({"draft_id": set.draft_id.as_str(),
                        "draft_revision": set.draft_revision, "path": "held", "reason": code})
                });
                return None;
            }
        }
        self.motion.held = None;
        self.request_motion_reference(waiting(now))
    }

    /// Queue `waiting`'s reference job now, or keep it as the next while another tick's is in
    /// flight, in place of any older one.
    fn request_motion_reference(&mut self, waiting: Waiting) -> Option<(u64, Option<Instant>)> {
        if self.motion_in_flight() {
            let (draft, revision) = (waiting.draft.clone(), waiting.revision);
            let reason = self.gpu_plan_fallback();
            self.event("gpu_preview_tick", || {
                json!({"draft_id": draft.as_str(), "draft_revision": revision, "path": "cpu",
                    "reason": reason, "generation": null, "waits": true})
            });
            self.motion.next = Some(waiting);
            return None;
        }
        self.motion.next = None;
        let requested = self.request_preview_inner(*waiting.job, waiting.timed);
        self.motion.in_flight = Some(requested.0);
        Some(requested)
    }

    /// Whether the reference job a tick queued is still the newest job asked for and not yet taken
    /// up.
    pub(crate) fn motion_in_flight(&self) -> bool {
        self.motion.in_flight.is_some_and(|generation| {
            self.presentation.requested == generation && self.presentation.queue.is_busy()
        })
    }

    /// A tick drawn on the GPU: nothing older waits for a frame of its own.
    pub(crate) fn motion_drawn_on_gpu(&mut self) {
        self.motion.held = None;
        self.motion.next = None;
    }

    /// Whether a tick of the open draft still waits for its frame: held, or queued behind the
    /// frame in flight. The frame on screen is then not the draft's newest.
    pub(crate) fn drag_frame_waiting(&self) -> bool {
        self.motion.held.is_some() || self.motion.next.is_some()
    }

    /// Make the run of held ticks have begun `by` earlier, so a test need not wait out
    /// [`HOLD_AT_MOST`].
    #[cfg(test)]
    pub(crate) fn backdate_hold(&mut self, by: Duration) {
        if let Some(held) = self.motion.held.as_mut() {
            held.since -= by;
        }
    }

    /// Whether a held tick is to be looked at again ([`HOLD_LOOK`]).
    pub(crate) fn motion_hold_pending(&self) -> bool {
        self.motion.held.is_some()
    }

    /// One more tick of a paused draft: its view planned again at 100% and above, where the
    /// region its last tick drew no longer holds the view after a pan. Drawn on the GPU, or taken
    /// as any tick the GPU does not draw.
    pub(crate) fn drag_view_planned(&mut self, mut job: PreviewJob) {
        let Some(set) = self.session.draft.as_ref().map(super::gesture::identity) else {
            return;
        };
        self.gpu_hold_source(job.evaluation.source());
        let planned = job.gpu.is_some();
        match self.gpu_tick(&set, job.gpu.take()) {
            Tick::Gpu => {
                self.motion_drawn_on_gpu();
                let content = self.presentation.presented_content;
                self.request_mask_coverage(&job, content);
                self.gpu_ticked(&set);
            }
            Tick::Cpu => {
                if let Some((generation, _)) = self.cpu_tick(&set, job, planned) {
                    self.gpu_cpu_tick(&set, generation);
                    self.presentation.preview_generation = generation;
                }
            }
        }
    }
}

/// After every message: the waiting ticks follow their draft. A held tick the surface now draws
/// is the gesture's frame; one held for [`HOLD_AT_MOST`] renders its reference frame; and the
/// next tick's reference job is queued once the frame in flight has been taken up.
pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> iced::Task<super::Message> {
    let open = editor
        .core_gesture()
        .map(|gesture| gesture.draft.draft_id.clone())
        .filter(|draft| editor.view_plan.released_draft.as_ref() != Some(draft));
    for slot in [&mut editor.motion.held, &mut editor.motion.next] {
        if slot
            .as_ref()
            .is_some_and(|waiting| Some(&waiting.draft) != open.as_ref())
        {
            *slot = None;
        }
    }
    if !editor.motion_in_flight() {
        editor.motion.in_flight = None;
    }
    if let Some(held) = &editor.motion.held {
        let (revision, lasted) = (held.revision, held.since.elapsed());
        if editor.gpu_shows_revision(revision) {
            editor.motion.held = None;
            editor.event(
                "gpu_preview_hold_ended",
                || json!({"draft_revision": revision, "why": "drawn"}),
            );
            editor.gpu_tick_presented();
        } else if lasted >= HOLD_AT_MOST
            && let Some(held) = editor.motion.held.take()
        {
            editor.event("gpu_preview_hold_ended", || {
                json!({"draft_revision": revision, "why": "lasted",
                    "held_ms": lasted.as_secs_f64() * 1000.0})
            });
            queued(editor, held);
        }
    }
    if editor.motion.next.is_some()
        && !editor.motion_in_flight()
        && let Some(next) = editor.motion.next.take()
    {
        queued(editor, next);
    }
    iced::Task::none()
}

/// Queue a waiting tick's reference job outside its own `draft.set` answer, as the frame of the
/// draft's newest revision asked for.
fn queued(editor: &mut Editor, waiting: Waiting) {
    let (draft, revision) = (waiting.draft.clone(), waiting.revision);
    if let Some((generation, _)) = editor.request_motion_reference(waiting) {
        editor.presentation.preview_generation = generation;
        editor.event("gpu_preview_reference", || {
            json!({"draft_id": draft.as_str(), "draft_revision": revision,
                "generation": generation})
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reasons_that_pass_hold_and_the_rest_render() {
        for code in PASSING {
            assert_eq!(holds(code, None, true), Some(Duration::ZERO), "{code}");
        }
        assert_eq!(
            holds("compiling", Some(Duration::from_millis(120)), true),
            Some(Duration::from_millis(120)),
            "a compile holds for what is left of its threshold"
        );
        assert_eq!(holds("unplannable", None, false), Some(Duration::ZERO));
        assert_eq!(
            holds("unplannable", None, true),
            None,
            "a plan that failed with its view known renders"
        );
        for code in [
            "boundary-stage",
            "pixel-stage",
            "budget-exceeded",
            "boundary-size",
            "no-adapter",
            "device-lost",
            "pipeline-failed",
            "warp-grid",
        ] {
            assert_eq!(holds(code, None, true), None, "{code}");
        }
    }
}
