//! The slider-gesture driver for a patch action. It holds no parameter knowledge of its own: which
//! action is drafted, which field it carries and what the value means all come from the descriptor
//! the control was generated from, so every module with a patch action gets this gesture and no
//! module is named here.
//!
//! One gesture is one core draft. Pointer-down (the first move) opens it with `draft.begin`; every
//! later move sends `draft.set` and the one preview job for the settings it accepted **the moment**
//! nothing is in flight, and only records the newest value while one is. There is no tick: the
//! bound is unchanged in substance — at most one draft round trip in flight, at most one preview
//! job per accepted value, intermediate values coalesced — but nothing waits on a timer for it.
//! Release commits once through `draft.commit`, Escape cancels through `draft.cancel`, and an
//! external revision marks the draft conflicted, which keeps it and refuses the commit until the
//! Changed elsewhere notice is answered with Discard or Reapply.
use crate::{
    app::{
        Editor,
        evidence::Settle,
        fields::number_text,
        message::Message,
        tasks::{
            draft_begin_task, draft_cancel_task, draft_commit_task, draft_reapply_task,
            draft_set_now, mutation,
        },
    },
    state::tools,
};
use iced::Task;
use lightwell_core::{AssetId, Draft, DraftId, ErrorKind};
use serde_json::{Value, json};

/// How the gesture ends once the round trip in flight has answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    Commit,
    Cancel,
}

/// One slider gesture's draft, as the desktop tracks it. The authoritative draft lives in the
/// client's own core session; this is the correlation the desktop needs to bound its requests.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderDraft {
    /// The patch action being drafted and the one field this gesture moves.
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// The control's label, for the status line.
    pub(crate) label: String,
    pub(crate) asset: AssetId,
    /// Known once `draft.begin` answered; until then every request waits.
    pub(crate) draft_id: Option<DraftId>,
    pub(crate) base_revision: u64,
    pub(crate) draft_revision: u64,
    pub(crate) conflicted: bool,
    /// A `draft.*` round trip is in flight; nothing else is sent until it answers.
    pub(crate) in_flight: bool,
    /// The newest value the pointer produced that no `draft.set` has carried yet.
    pub(crate) pending: Option<f64>,
    /// The value the last accepted `draft.set` carried.
    pub(crate) sent: Option<f64>,
    /// The gesture ended while a round trip was in flight.
    pub(crate) finish: Option<Finish>,
}

impl SliderDraft {
    /// The field this gesture would commit, as one patch.
    fn fields(&self, value: f64) -> Value {
        json!({ self.parameter.clone(): value })
    }

    /// A value is waiting to be sent when it differs from the one already accepted.
    fn outstanding(&self) -> Option<f64> {
        self.pending.filter(|value| Some(*value) != self.sent)
    }

    /// Nothing is in flight and nothing is waiting: the displayed preview is the drafted one.
    pub(crate) fn drained(&self) -> bool {
        !self.in_flight && self.outstanding().is_none() && self.finish.is_none()
    }

    /// The draft as this desktop knows it, in the shape `session.state` reports. It is only used
    /// when a response has not carried the session's own copy yet, so a captured frame never shows
    /// "no draft" while a gesture is plainly open on screen.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "draft_id": self.draft_id.as_ref().map(DraftId::as_str),
            "action": self.action,
            "fields": self.sent.map(|value| self.fields(value)).unwrap_or_else(|| json!({})),
            "base_revision": self.base_revision,
            "draft_revision": self.draft_revision,
            "conflicted": self.conflicted,
        })
    }
}

impl Editor {
    /// Why a slider draft cannot start right now, in the words the status bar uses. At most one
    /// draft exists per client, so the crop draft and a slider gesture exclude each other.
    fn slider_draft_refusal(&self) -> Option<String> {
        if self.crop.is_some() || self.crop_pending.is_some() {
            return Some("Apply or Cancel the crop draft before editing a slider".into());
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before editing".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        None
    }

    /// What the field shows for one value a drag produced. The widget has already quantized the
    /// value to the parameter's declared step and precision, so this only has to write it with the
    /// decimals that parameter declares; a value with no declared parameter behind it falls back to
    /// its plain text, which is the same rule every other field follows.
    fn dragged_text(&self, action: &str, parameter: &str, value: f64) -> String {
        match crate::app::fields::declared(&self.modules, action, parameter) {
            Some(declared) => crate::app::fields::format_number(declared, value),
            None => number_text(value),
        }
    }

    /// A slider of a patch action moved. The first move of a gesture opens the draft; later moves
    /// only record the newest value, which the tick sends.
    pub(crate) fn slider_moved(
        &mut self,
        action: String,
        parameter: String,
        value: f64,
    ) -> Task<Message> {
        if let Some(draft) = &self.slider_draft {
            if draft.action != action || draft.parameter != parameter {
                self.status = "Finish the open slider gesture before starting another".into();
                return Task::none();
            }
            let text = self.dragged_text(&action, &parameter, value);
            self.fields.set(&action, &parameter, text);
            self.editing = None;
            self.dragging = Some((action, parameter));
            if let Some(draft) = &mut self.slider_draft {
                draft.pending = Some(value);
            }
            // Sent now when the previous round trip has answered; recorded otherwise, and the
            // answer to that round trip sends the newest value. No timer stands between the input
            // and the request it produces.
            return self.slider_tick();
        }
        if let Some(reason) = self.slider_draft_refusal() {
            self.status = reason;
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let base_revision = state.revision;
        let label = tools::control_label(&self.modules, &action, &parameter)
            .unwrap_or_else(|| parameter.clone());
        let text = self.dragged_text(&action, &parameter, value);
        self.fields.set(&action, &parameter, text);
        self.editing = None;
        self.dragging = Some((action.clone(), parameter.clone()));
        self.slider_draft = Some(SliderDraft {
            action: action.clone(),
            parameter,
            label: label.clone(),
            asset: asset.clone(),
            draft_id: None,
            base_revision,
            draft_revision: 0,
            conflicted: false,
            in_flight: true,
            pending: Some(value),
            sent: None,
            finish: None,
        });
        self.status = format!("Drafting {label}…");
        self.event(
            "slider_draft_begin",
            json!({"action":action,"revision":base_revision}),
        );
        draft_begin_task(self.owner.clone(), self.client, asset, action)
    }

    /// The gate every outstanding value passes through: at most one `draft.set` and one preview
    /// job at a time, and nothing at all while a previous round trip is in flight.
    ///
    /// A move calls this directly. `Message::SliderDraftTick` is the same call under its old name,
    /// which the evidence driver and the paced step still send after each move; with the send
    /// already done it finds nothing outstanding and does nothing.
    pub(crate) fn slider_tick(&mut self) -> Task<Message> {
        let Some(draft) = &self.slider_draft else {
            return Task::none();
        };
        if draft.in_flight || draft.finish.is_some() || draft.conflicted {
            return Task::none();
        }
        self.send_slider_set()
    }

    /// Send the one outstanding value, if there is one. The caller has already decided that
    /// nothing is in flight.
    fn send_slider_set(&mut self) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        let (Some(draft_id), Some(value)) = (draft.draft_id.clone(), draft.outstanding()) else {
            return Task::none();
        };
        let fields = draft.fields(value);
        let asset = draft.asset.clone();
        draft.in_flight = true;
        draft.sent = Some(value);
        draft.pending = None;
        self.event(
            "slider_draft_set",
            json!({"draft_id":draft_id.as_str(),"fields":fields}),
        );
        let proxy = self.proxy_bounds();
        // Synchronous on purpose: see `draft_set_now`. The answer is handled exactly as a message
        // would be, so nothing else about the gesture changes.
        let result = draft_set_now(&self.owner, self.client, draft_id, asset, fields, proxy);
        self.slider_set(result)
    }

    /// `draft.begin` answered: the draft exists, so the first value can go out.
    pub(crate) fn slider_begun(&mut self, result: Result<Draft, String>) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        draft.in_flight = false;
        match result {
            Ok(opened) => {
                draft.draft_id = Some(opened.draft_id.clone());
                draft.base_revision = opened.base_revision;
                draft.conflicted = opened.conflicted;
                self.session.draft = Some(opened);
                self.after_slider_round_trip()
            }
            Err(error) => {
                self.status = error;
                self.end_slider_draft();
                Task::none()
            }
        }
    }

    /// One `draft.set` answered with the preview of the settings it accepted.
    pub(crate) fn slider_set(
        &mut self,
        result: Result<
            (
                Draft,
                lightwell_core::PreviewJob,
                crate::app::tasks::RoundTrip,
            ),
            String,
        >,
    ) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        draft.in_flight = false;
        match result {
            Ok((set, job, round_trip)) => {
                let now = std::time::Instant::now();
                let legs = round_trip.legs_ms(now);
                let timing = self.loop_timing.get();
                let since = |at: Option<std::time::Instant>| {
                    at.map(|at| now.duration_since(at).as_secs_f64() * 1000.0)
                };
                let loop_timing = json!({
                    "last_update_ms": timing.last_update_ms,
                    "last_rederive_ms": timing.last_rederive_ms,
                    "last_view_ms": timing.last_view_ms,
                    "since_view_end_ms": since(timing.last_view_end),
                    "since_update_end_ms": since(timing.last_update_end),
                });
                draft.draft_revision = set.draft_revision;
                draft.conflicted = set.conflicted;
                let label = draft.label.clone();
                let (draft_revision, sent) = (set.draft_revision, draft.sent);
                self.session.draft = Some(set);
                self.preview_generation = self.preview_queue.request(job);
                self.status = format!("Drafting {label}…");
                // The one record that ties an input to the frame it will produce: the `draft.set`
                // this answers carried `value`, and the preview job just queued for it is
                // `generation`, which the `preview_displayed` event of its upload repeats. Without
                // it a measurement can only guess which frame belongs to which slider value.
                self.event(
                    "slider_draft_preview",
                    json!({"generation":self.preview_generation,"draft_revision":draft_revision,"value":sent,"round_trip_ms":{"executor_wait":legs[0],"draft_set":legs[1],"preview_job":legs[2],"return":legs[3]},"loop":loop_timing}),
                );
                self.after_slider_round_trip()
            }
            Err(error) => {
                self.status = error;
                self.after_slider_round_trip()
            }
        }
    }

    /// Whatever the gesture asked for while a round trip was in flight happens now: the end it
    /// requested, else the newest value it produced.
    fn after_slider_round_trip(&mut self) -> Task<Message> {
        match self.slider_draft.as_ref().and_then(|draft| draft.finish) {
            Some(Finish::Commit) => self.slider_commit(),
            Some(Finish::Cancel) => self.slider_cancel(),
            None => self.send_slider_set(),
        }
    }

    /// Release, key-up or Enter: end the gesture and commit it exactly once. The newest value is
    /// sent first when the tick has not carried it yet, so the committed patch is what is on screen.
    pub(crate) fn slider_commit(&mut self) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        self.dragging = None;
        if draft.in_flight {
            draft.finish = Some(Finish::Commit);
            return Task::none();
        }
        if draft.conflicted {
            draft.finish = None;
            self.status = "Changed elsewhere: discard the slider draft or reapply it".into();
            self.settle_step(Settle::SliderDraft);
            return Task::none();
        }
        if draft.outstanding().is_some() {
            draft.finish = Some(Finish::Commit);
            return self.send_slider_set();
        }
        let Some(draft_id) = draft.draft_id.clone() else {
            draft.finish = Some(Finish::Commit);
            return Task::none();
        };
        let (asset, base_revision) = (draft.asset.clone(), draft.base_revision);
        draft.finish = None;
        draft.in_flight = true;
        let mutation = mutation(base_revision);
        self.event(
            "slider_draft_commit",
            json!({"draft_id":draft_id.as_str(),"request_id":mutation.request_id,"expected_revision":base_revision}),
        );
        let proxy = self.proxy_bounds();
        draft_commit_task(
            self.owner.clone(),
            self.client,
            draft_id,
            asset,
            mutation,
            proxy,
        )
    }

    /// `draft.commit` answered. A real outcome merges into history like any other command; a no-op
    /// ends the gesture with no entry and no history refresh.
    pub(crate) fn slider_committed(
        &mut self,
        result: Result<Option<crate::app::tasks::Refresh>, String>,
    ) -> Task<Message> {
        self.busy = false;
        match result {
            Ok(Some(refresh)) => {
                self.end_slider_draft();
                self.accept(refresh);
            }
            Ok(None) => {
                let label = self
                    .slider_draft
                    .as_ref()
                    .map(|draft| draft.label.clone())
                    .unwrap_or_default();
                self.end_slider_draft();
                self.status = format!("{label} unchanged; nothing was committed");
                self.event("slider_draft_noop", json!({ "label": label }));
                // The drafted pixels are still on screen and they are not the committed ones, so
                // the step settles on the render that replaces them rather than on this message:
                // capturing here would show the draft and leave that render to land under the
                // next step.
                return match self.reseed_after_slider_draft() {
                    Some(task) => task,
                    None => {
                        self.settle_step(Settle::Preview);
                        Task::none()
                    }
                };
            }
            Err(error) => {
                // A refused commit keeps the draft, so the value can be discarded or reapplied
                // deliberately; a stale revision is exactly the conflict the notice explains.
                if let Some(draft) = &mut self.slider_draft {
                    draft.in_flight = false;
                    draft.finish = None;
                    if error.starts_with(ErrorKind::Conflict.code()) {
                        draft.conflicted = true;
                    }
                }
                self.status = error;
                self.settle_step(Settle::SliderDraft);
            }
        }
        Task::none()
    }

    /// Escape, focus loss, the Changed elsewhere notice's Discard or a script: end the gesture and
    /// commit nothing. The field returns to the authoritative value and the canvas returns to the
    /// committed pixels.
    pub(crate) fn slider_cancel(&mut self) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        self.dragging = None;
        if draft.in_flight {
            draft.finish = Some(Finish::Cancel);
            return Task::none();
        }
        let draft_id = draft.draft_id.clone();
        let label = draft.label.clone();
        self.end_slider_draft();
        self.status = format!("{label} draft discarded");
        self.event("slider_draft_cancelled", json!({ "label": label }));
        let mut tasks = Vec::new();
        if let Some(draft_id) = draft_id {
            tasks.push(draft_cancel_task(self.owner.clone(), self.client, draft_id));
        }
        match self.reseed_after_slider_draft() {
            Some(task) => tasks.push(task),
            None => self.settle_step(Settle::Preview),
        }
        Task::batch(tasks)
    }

    /// The Changed elsewhere notice's Reapply: rebase the draft on the current revision, keeping
    /// the field this client set, then re-send it so the preview shows it again.
    pub(crate) fn slider_reapply(&mut self) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        let Some(draft_id) = draft.draft_id.clone() else {
            return Task::none();
        };
        if draft.in_flight {
            return Task::none();
        }
        draft.in_flight = true;
        draft_reapply_task(self.owner.clone(), self.client, draft_id)
    }

    /// `draft.reapply` answered: the draft is based on the current revision again and its value is
    /// re-sent, so the drafted preview returns.
    pub(crate) fn slider_reapplied(&mut self, result: Result<Draft, String>) -> Task<Message> {
        let Some(draft) = &mut self.slider_draft else {
            return Task::none();
        };
        draft.in_flight = false;
        match result {
            Ok(rebased) => {
                draft.base_revision = rebased.base_revision;
                draft.draft_revision = rebased.draft_revision;
                draft.conflicted = rebased.conflicted;
                // Re-send what this client set: the rebased draft still holds it, but the preview
                // on screen belongs to the revision that displaced it.
                draft.pending = draft.sent;
                draft.sent = None;
                let label = draft.label.clone();
                self.session.draft = Some(rebased);
                self.status = format!("Drafting {label}…");
                self.send_slider_set()
            }
            Err(error) => {
                self.status = error;
                Task::none()
            }
        }
    }

    /// Drop the gesture's own state. The core draft is ended by its own request; this is only what
    /// the desktop keeps to correlate and bound it.
    pub(crate) fn end_slider_draft(&mut self) {
        self.slider_draft = None;
        self.session.draft = None;
        self.dragging = None;
    }

    /// After a cancelled or no-op gesture the drafted pixels are still on screen and the field
    /// still holds the drafted text: put both back to the committed state. The field is re-seeded
    /// from the values already fetched with the recipe, and one preview job replaces the pixels —
    /// no `asset.state` request and no history refresh. `None` when there is nothing to show.
    fn reseed_after_slider_draft(&mut self) -> Option<Task<Message>> {
        let asset = self.state.as_ref()?.asset.id.clone();
        let entry = self.displayed_entry();
        self.seed_values();
        let proxy = self.proxy_bounds();
        Some(crate::app::tasks::current_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            entry,
            proxy,
        ))
    }

    /// A new authoritative revision arrived while a slider gesture was open. The draft is kept and
    /// marked, exactly as the crop draft is, so nothing is discarded without a decision.
    pub(crate) fn settle_slider_draft(&mut self, revision: u64) {
        let Some(draft) = &mut self.slider_draft else {
            return;
        };
        if draft.base_revision == revision || draft.conflicted {
            return;
        }
        draft.conflicted = true;
        if let Some(session) = &mut self.session.draft {
            session.conflicted = true;
        }
        let label = draft.label.clone();
        self.status = "Changed elsewhere: discard the slider draft or reapply it".into();
        self.event(
            "slider_draft_conflicted",
            json!({"label":label,"revision":revision}),
        );
        self.settle_step(Settle::SliderDraft);
    }
}
