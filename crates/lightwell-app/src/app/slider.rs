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
        fields,
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
use serde_json::{Map, Value, json};

/// A double-click reset that arrived while this client still had a gesture's commit, or another
/// request, in flight. It runs, as the one action it is, as soon as nothing is in flight.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingReset {
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// The photograph it was asked for: a reset never runs against another one.
    pub(crate) asset: AssetId,
}

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
    pub(crate) pending: Option<Value>,
    /// The value the last accepted `draft.set` carried.
    pub(crate) sent: Option<Value>,
    /// The gesture ended while a round trip was in flight.
    pub(crate) finish: Option<Finish>,
    /// The last accepted value's preview job was refused, so no frame of its own is coming: the
    /// frame on screen is what that value shows until release.
    pub(crate) unpreviewed: bool,
}

impl SliderDraft {
    /// The field this gesture would commit, as one patch.
    fn fields(&self, value: &Value) -> Value {
        json!({ self.parameter.clone(): value })
    }

    /// A value is waiting to be sent when it differs from the one already accepted.
    fn outstanding(&self) -> Option<&Value> {
        self.pending
            .as_ref()
            .filter(|value| Some(*value) != self.sent.as_ref())
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
            "fields": self.sent.as_ref().map(|value| self.fields(value)).unwrap_or_else(|| json!({})),
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
        // A mask gesture holds this client's one core draft, exactly as the crop draft does, and the
        // adjustments a mask is bound to sit directly under the gesture that drew it — so reaching
        // one with a gradient half drawn is an ordinary mistake to make. An **armed** brush is the
        // exception the delivered rule already names: it has painted nothing and has nothing to
        // Apply, so it gives its draft up below rather than refusing the slider that wants it.
        if self.mask_draft.is_some() && !self.armed_brush() {
            return Some("Apply or Cancel the mask gesture before editing a slider".into());
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before editing".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        None
    }

    /// A slider of a patch action moved. The first move of a gesture opens the draft; later moves
    /// only record the newest value, which the tick sends.
    pub(crate) fn slider_moved(
        &mut self,
        action: String,
        parameter: String,
        value: f64,
    ) -> Task<Message> {
        self.control_moved(action, parameter, Value::from(value))
    }

    /// Draft one declared field of any JSON kind. A control never carries a second parameter.
    pub(crate) fn control_moved(
        &mut self,
        action: String,
        parameter: String,
        value: Value,
    ) -> Task<Message> {
        if let Some(draft) = &self.slider_draft {
            if draft.action != action || draft.parameter != parameter {
                self.status = "Finish the open slider gesture before starting another".into();
                return Task::none();
            }
            self.set_control_field_value(&action, &parameter, &value);
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
        // An armed brush holds the one core draft this gesture needs and has nothing painted to lose
        // by giving it up, which is what every other gesture and every mask command already do with
        // one. Without this the host refuses the `draft.begin` a round trip later and the gesture is
        // left with a draft that does not exist.
        let disarm = self.disarm_brush();
        let Some(state) = &self.state else {
            return disarm.unwrap_or_else(Task::none);
        };
        let asset = state.asset.id.clone();
        let base_revision = state.revision;
        let label = tools::control_label(&self.modules, &action, &parameter)
            .unwrap_or_else(|| parameter.clone());
        self.set_control_field_value(&action, &parameter, &value);
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
            unpreviewed: false,
        });
        self.status = format!("Drafting {label}…");
        // The host-owned target this gesture drafts through. For a module action it is the mask the
        // panel's sections are bound to, which is what makes a masked slider follow the drag the way
        // a global one does; for a `mask.*` control it is the mask and component the panel has open,
        // because no declared parameter kind can carry an identity.
        let target = self.draft_target(&action);
        self.event(
            "slider_draft_begin",
            json!({"action":action,"revision":base_revision,"target":target}),
        );
        let begin = draft_begin_task(self.owner.clone(), self.client, asset, action, target);
        match disarm {
            Some(cancel) => Task::batch([cancel, begin]),
            None => begin,
        }
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
        let (Some(draft_id), Some(value)) = (draft.draft_id.clone(), draft.outstanding().cloned())
        else {
            return Task::none();
        };
        let fields = draft.fields(&value);
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
                draft.unpreviewed = false;
                draft.conflicted = set.conflicted;
                let label = draft.label.clone();
                let (draft_revision, sent) = (set.draft_revision, draft.sent.clone());
                self.session.draft = Some(set);
                self.preview_generation = self.request_preview(job);
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
                // The draft accepted the value but its preview job was refused, so no frame of its
                // own is coming. A drafted RAW temperature or tint is not this: the core previews
                // it approximately on the developed planes. What is left is a RAW whose
                // development is not in memory at all — evicted while a redevelopment or a source
                // preparation of an earlier request is in flight — which the core answers with
                // preparation-required rather than a stale frame. The gesture goes on, its release
                // commits and that commit's frame waits for the development; the status bar says
                // so instead of showing the error code.
                let label = draft.label.clone();
                let (draft_revision, sent) = (draft.draft_revision, draft.sent.clone());
                draft.unpreviewed = true;
                self.status = if error.starts_with(ErrorKind::PreparationRequired.code()) {
                    format!(
                        "{label} cannot be previewed until the RAW development is ready; it shows on release"
                    )
                } else {
                    error.clone()
                };
                self.event(
                    "slider_draft_unpreviewed",
                    json!({"draft_revision":draft_revision,"value":sent,"error":error}),
                );
                let task = self.after_slider_round_trip();
                // A drained gesture whose newest value has no frame of its own is still drained:
                // the frame on screen is the evidence of that, so a scripted step settles on it.
                if self.slider_draft.as_ref().is_some_and(SliderDraft::drained) {
                    self.settle_step(Settle::SliderDraft);
                }
                task
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
                draft.pending = draft.sent.clone();
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

    /// A release of a drafting control with no draft open. Such a control opens its draft on its
    /// first change, so nothing changed: the press landed exactly on the value, or the draft was
    /// refused and the status bar says why. There is nothing to commit. Submitting the unchanged
    /// field instead would send a request that changes nothing — or, for a RAW custom white
    /// balance still showing its 6504 K starting value under As shot, one that switches to Custom —
    /// and hold the section busy for its round trip, which is exactly when a double-click's second
    /// press arrives.
    pub(crate) fn release_without_draft(&mut self, action: &str, parameter: &str) -> Task<Message> {
        if self
            .dragging
            .as_ref()
            .is_some_and(|(dragged, field)| dragged == action && field == parameter)
        {
            self.dragging = None;
        }
        Task::none()
    }

    /// Double-clicking a control's label or rail: reset that one field. A number control that
    /// declares its own reset runs that action, such as RAW's temperature and tint returning to As
    /// shot; otherwise the field goes to its declared default, as one action where that one field
    /// is a whole request, and otherwise only its text is refilled ([`fields::field_reset`]).
    ///
    /// The first click of a double-click on a rail usually moves the value a step or two, so it
    /// opens a gesture whose release commits; the second click arrives while that commit is still
    /// answering. Sent then, the reset would name the revision the commit is replacing and be
    /// refused as stale — for a RAW white balance, whose commit waits for the mosaic to be
    /// redeveloped, for a second or more. So a reset that is one action waits while this client has
    /// a gesture or a request in flight, and [`Editor::run_pending_reset`] sends it, against the
    /// revision that answer brings, as soon as nothing is.
    pub(crate) fn reset_field(&mut self, action: String, parameter: String) -> Task<Message> {
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|d| d.parameter(&parameter))
        else {
            self.status = fields::undeclared_label(&action, &parameter);
            return Task::none();
        };
        let default = fields::seed_text(declared);
        let declared_reset =
            tools::declared_field_reset(&self.modules, &action, &parameter).is_some();
        let reset = fields::field_reset(&self.modules, &action, &parameter);
        if reset.is_some()
            && (self.slider_draft.is_some() || self.busy)
            && self.session.preview.can_edit()
            && let Some(state) = &self.state
        {
            let (asset, revision) = (state.asset.id.clone(), state.revision);
            self.event(
                "field_reset_queued",
                json!({"action":action,"parameter":parameter,"revision":revision,
                    "gesture_open":self.slider_draft.is_some(),"busy":self.busy}),
            );
            self.pending_reset = Some(PendingReset {
                action,
                parameter,
                asset,
            });
            return Task::none();
        }
        self.editing = None;
        if declared_reset {
            // What the declared action leaves is known only from its answer, so until then the
            // field shows the authoritative value again rather than a default nothing will set.
            self.seed_values();
        } else {
            self.fields.set(&action, &parameter, default);
        }
        match reset.filter(|_| self.editable()) {
            Some((reset, preset)) => self.send_reset((action, parameter), reset, preset),
            None => Task::none(),
        }
    }

    /// Run a waiting reset once nothing is in flight: the gesture has ended and its commit, if it
    /// made one, has been adopted, so the reset names the revision that commit produced. A reset
    /// whose photograph is no longer open, or that would now land on a historical preview, is
    /// dropped with its reason rather than run somewhere it was not asked for.
    pub(crate) fn run_pending_reset(&mut self) -> Task<Message> {
        if self.pending_reset.is_none() || self.slider_draft.is_some() || self.busy {
            return Task::none();
        }
        let Some(reset) = self.pending_reset.take() else {
            return Task::none();
        };
        let label = tools::control_label(&self.modules, &reset.action, &reset.parameter)
            .unwrap_or_else(|| reset.parameter.clone());
        let reason = if self
            .state
            .as_ref()
            .is_none_or(|state| state.asset.id != reset.asset)
        {
            Some("another photograph is open")
        } else if !self.session.preview.can_edit() {
            Some("a historical entry is shown")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.status = format!("{label} was not reset: {reason}");
            self.event(
                "field_reset_dropped",
                json!({"action":reset.action,"parameter":reset.parameter,"reason":reason}),
            );
            return Task::none();
        }
        self.reset_field(reset.action, reset.parameter)
    }

    /// Send one field's reset as its own action: `action` and `preset` are the request, `field` the
    /// control it was asked of.
    fn send_reset(
        &mut self,
        field: (String, String),
        action: String,
        preset: Map<String, Value>,
    ) -> Task<Message> {
        let revision = self.state.as_ref().map(|state| state.revision);
        self.event(
            "field_reset_sent",
            json!({"action":action,"preset":preset,"revision":revision,
                "field":{"action":field.0,"parameter":field.1}}),
        );
        self.dispatch(Message::RunAction { action, preset })
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
