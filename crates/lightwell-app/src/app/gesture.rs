//! The one gesture this client holds, and the driver that runs a core draft for it.
//!
//! A client holds at most one draft, so the desktop has exactly one place for it: [`Editor::gesture`].
//! A slider and a mask shape or stroke draft through the core ([`Gesture::Core`]), each as one
//! [`CoreDraft`] state machine with a small kind of its own; the crop frame editor keeps its own
//! desktop-local draft ([`Gesture::Crop`]) but takes the same place; and a discarded core draft whose
//! owner round trip has not answered keeps it too ([`Gesture::Closing`]), so the next gesture's
//! `draft.begin` can never overtake the `draft.cancel` of the last one. Whether anything may start
//! is answered in one place, [`Editor::gesture_refusal`].
//!
//! The driver runs what the state machine answers: `draft.set` synchronously on this thread, in the
//! update that produced the fields ([performance rule 12](../../../../docs/engineering/performance-rules.md#rules)),
//! and `draft.begin`, `draft.commit`, `draft.cancel` and `draft.reapply` as owner tasks whose answers
//! come back as one [`DraftMessage`] family, each naming the gesture and draft it belongs to.
use crate::{
    app::{
        Editor,
        crop::PendingDraft,
        draft::{CoreDraft, Event, GestureId, Round, Step},
        evidence::Settle,
        message::{DraftMessage, Message},
        tasks::{self, Refresh, RoundTrip, mutation},
    },
    crop_draft::CropDraft,
    mask_draft::{ContentMap, MaskDraft},
};
use iced::Task;
use lightwell_core::{AssetId, Draft, DraftId, ErrorKind, PreviewJob, mask::commands::MaskTarget};
use serde_json::{Value, json};

/// The one draft this client holds.
#[derive(Clone, Debug)]
pub(crate) enum Gesture {
    /// A slider or mask gesture on the core draft lifecycle.
    Core(CoreGesture),
    /// A discarded core draft still waiting for its owner round trips. It shows nothing and takes
    /// nothing, but a new core draft cannot start until it has ended at the owner.
    Closing {
        draft: CoreDraft,
        /// Ask for the committed frame again once the cancel has answered: the gesture drafted
        /// pixels that are still on screen.
        reseed: bool,
    },
    /// The crop frame editor's desktop-local draft, starting, open, or both while it rebases.
    Crop(CropGesture),
}

/// A slider or mask gesture and the core draft behind it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CoreGesture {
    pub(crate) asset: AssetId,
    pub(crate) draft: CoreDraft,
    pub(crate) kind: Kind,
}

/// What a core gesture drafts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Slider(SliderGesture),
    Mask(MaskGesture),
}

/// A generated control's gesture: one declared field of one action, dragged.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderGesture {
    /// The action being drafted and the one field this gesture moves.
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// The control's label, for the status line.
    pub(crate) label: String,
    /// The host-owned target the draft was begun with.
    pub(crate) target: MaskTarget,
    /// The last accepted value's preview job was refused, so no frame of its own is coming: the
    /// frame on screen is what that value shows until release.
    pub(crate) unpreviewed: bool,
}

/// A mask shape or stroke, dragged or painted on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaskGesture {
    pub(crate) shape: MaskDraft,
    /// The content-to-output map, read once from `render.transform` when the gesture opened and
    /// then applied locally per pointer move.
    pub(crate) map: Option<ContentMap>,
}

/// The crop draft: `pending` while its truncated preview is on its way, `draft` once it is open.
#[derive(Clone, Debug, Default)]
pub(crate) struct CropGesture {
    pub(crate) draft: Option<CropDraft>,
    pub(crate) pending: Option<PendingDraft>,
}

impl MaskGesture {
    /// The fields the gesture would commit now, or `None` while it has nothing to send: an armed
    /// brush has no path, and a path is a required parameter.
    pub(crate) fn fields(&self) -> Option<Value> {
        self.shape
            .brush()
            .is_none_or(|stroke| stroke.drawn())
            .then(|| Value::Object(self.shape.fields()))
    }
}

/// What wants to start while the gesture slot may be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Starting {
    /// A slider gesture of a generated control.
    Slider,
    /// A mask shape or stroke gesture.
    Mask,
    /// A `mask.*` command from the Masks panel.
    MaskCommand,
    /// The crop draft.
    Crop,
    /// Another canvas mode.
    Mode,
    /// A pick on the photograph.
    Pick,
    /// The components gallery.
    Gallery,
    /// Compare with the original.
    Compare,
    /// A preset applied to the photograph.
    Preset,
    /// A preview of the committed state for the view's own sake: a refit to new bounds, or a new
    /// mask overlay.
    Refit,
}

impl Starting {
    /// This opens a draft of its own, so it takes the one slot.
    fn claims_slot(self) -> bool {
        matches!(self, Self::Slider | Self::Mask | Self::Crop)
    }

    /// This waits for a discarded draft to close: it would open a draft of its own, or ask for a
    /// frame the closing draft's own read-back is already bringing.
    fn waits_for_closing(self) -> bool {
        self.claims_slot() || self == Self::Refit
    }

    fn clause(self) -> &'static str {
        match self {
            Self::Slider => "before editing a slider",
            Self::Mask | Self::MaskCommand => "before editing a mask",
            Self::Crop => "before cropping",
            Self::Mode => "before entering this mode",
            Self::Pick => "before picking from the photograph",
            Self::Gallery => "before opening Components",
            Self::Compare => "before comparing with the original",
            Self::Preset => "before applying a preset",
            Self::Refit => "before refitting the preview",
        }
    }
}

impl Kind {
    /// The action the core draft runs: a control's action, or the `mask.*` method a shape commits.
    fn action(&self) -> Option<String> {
        match self {
            Self::Slider(slider) => Some(slider.action.clone()),
            Self::Mask(mask) => mask.shape.method().map(str::to_owned),
        }
    }

    fn target(&self) -> MaskTarget {
        match self {
            Self::Slider(slider) => slider.target.clone(),
            Self::Mask(mask) => MaskTarget {
                mask: mask.shape.mask.clone(),
                component: mask.shape.component.clone(),
                ..MaskTarget::default()
            },
        }
    }

    /// The prefix of every evidence event this gesture records.
    pub(crate) fn prefix(&self) -> &'static str {
        match self {
            Self::Slider(_) => "slider_draft",
            Self::Mask(_) => "mask_draft",
        }
    }

    /// What the status line calls this gesture.
    fn noun(&self) -> &'static str {
        match self {
            Self::Slider(_) => "slider draft",
            Self::Mask(_) => "mask gesture",
        }
    }

    /// A brush in hand that has painted nothing: it has nothing to Apply and nothing to lose.
    pub(crate) fn armed(&self) -> bool {
        match self {
            Self::Slider(_) => false,
            Self::Mask(mask) => mask.shape.brush().is_some_and(|stroke| !stroke.drawn()),
        }
    }
}

impl CoreGesture {
    /// The gesture has nothing of its own to lose and can give its draft up: an armed brush whose
    /// core draft is open and idle.
    pub(crate) fn yields(&self) -> bool {
        self.kind.armed() && self.draft.idle()
    }

    pub(crate) fn slider(&self) -> Option<&SliderGesture> {
        match &self.kind {
            Kind::Slider(slider) => Some(slider),
            Kind::Mask(_) => None,
        }
    }

    pub(crate) fn mask(&self) -> Option<&MaskGesture> {
        match &self.kind {
            Kind::Mask(mask) => Some(mask),
            Kind::Slider(_) => None,
        }
    }
}

impl Gesture {
    /// The open slider or mask gesture.
    pub(crate) fn core(&self) -> Option<&CoreGesture> {
        match self {
            Self::Core(gesture) => Some(gesture),
            _ => None,
        }
    }

    pub(crate) fn crop(&self) -> Option<&CropGesture> {
        match self {
            Self::Crop(crop) => Some(crop),
            _ => None,
        }
    }
}

impl Editor {
    /// The open core gesture.
    pub(crate) fn core_gesture(&self) -> Option<&CoreGesture> {
        self.gesture.as_ref().and_then(Gesture::core)
    }

    /// A discarded core draft is still ending at the owner.
    pub(crate) fn gesture_closing(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Closing { .. }))
    }

    /// The open slider gesture.
    pub(crate) fn slider_gesture(&self) -> Option<&SliderGesture> {
        self.core_gesture().and_then(CoreGesture::slider)
    }

    /// The open mask gesture.
    pub(crate) fn mask_gesture(&self) -> Option<&MaskGesture> {
        self.core_gesture().and_then(CoreGesture::mask)
    }

    pub(crate) fn mask_gesture_mut(&mut self) -> Option<&mut MaskGesture> {
        match &mut self.gesture {
            Some(Gesture::Core(CoreGesture {
                kind: Kind::Mask(mask),
                ..
            })) => Some(mask),
            _ => None,
        }
    }

    /// The open mask gesture's shape.
    pub(crate) fn mask_shape(&self) -> Option<&MaskDraft> {
        self.mask_gesture().map(|mask| &mask.shape)
    }

    /// The (action, parameter) of the control whose slider gesture is open.
    pub(crate) fn drafting_control(&self) -> Option<(&str, &str)> {
        self.slider_gesture()
            .map(|slider| (slider.action.as_str(), slider.parameter.as_str()))
    }

    /// The open crop draft.
    pub(crate) fn crop(&self) -> Option<&CropDraft> {
        self.gesture
            .as_ref()
            .and_then(Gesture::crop)
            .and_then(|crop| crop.draft.as_ref())
    }

    pub(crate) fn crop_mut(&mut self) -> Option<&mut CropDraft> {
        match &mut self.gesture {
            Some(Gesture::Crop(crop)) => crop.draft.as_mut(),
            _ => None,
        }
    }

    /// The crop draft that is starting or rebasing, waiting for its truncated preview.
    pub(crate) fn crop_pending(&self) -> Option<&PendingDraft> {
        self.gesture
            .as_ref()
            .and_then(Gesture::crop)
            .and_then(|crop| crop.pending.as_ref())
    }

    pub(crate) fn crop_pending_mut(&mut self) -> Option<&mut PendingDraft> {
        match &mut self.gesture {
            Some(Gesture::Crop(crop)) => crop.pending.as_mut(),
            _ => None,
        }
    }

    /// Replace the crop draft, keeping whatever is pending. The slot empties when neither is left.
    pub(crate) fn set_crop(&mut self, draft: Option<CropDraft>) {
        let pending = self.take_crop_pending();
        self.put_crop(draft, pending);
    }

    /// Replace what the crop draft is waiting for, keeping the open draft.
    pub(crate) fn set_crop_pending(&mut self, pending: Option<PendingDraft>) {
        let draft = self.take_crop();
        self.put_crop(draft, pending);
    }

    pub(crate) fn take_crop_pending(&mut self) -> Option<PendingDraft> {
        match &mut self.gesture {
            Some(Gesture::Crop(crop)) => {
                let pending = crop.pending.take();
                if crop.draft.is_none() {
                    self.gesture = None;
                }
                pending
            }
            _ => None,
        }
    }

    fn take_crop(&mut self) -> Option<CropDraft> {
        match &mut self.gesture {
            Some(Gesture::Crop(crop)) => {
                let draft = crop.draft.take();
                if crop.pending.is_none() {
                    self.gesture = None;
                }
                draft
            }
            _ => None,
        }
    }

    fn put_crop(&mut self, draft: Option<CropDraft>, pending: Option<PendingDraft>) {
        if draft.is_none() && pending.is_none() {
            if matches!(self.gesture, Some(Gesture::Crop(_))) {
                self.gesture = None;
            }
            return;
        }
        debug_assert!(
            matches!(self.gesture, None | Some(Gesture::Crop(_))),
            "the crop draft never displaces another gesture"
        );
        self.gesture = Some(Gesture::Crop(CropGesture { draft, pending }));
    }

    /// Why `starting` cannot start now, in the words the status bar uses, or `None` when it can.
    ///
    /// This is the one-draft rule, answered once: a client holds one draft, so any gesture that
    /// needs one waits for the open gesture to be applied, cancelled or finished, and so does any
    /// mode change, pick, preset, gallery or comparison that would displace or pause it. An armed
    /// brush is the one gesture that never refuses: it has painted nothing, so the gesture that
    /// needs the slot takes it ([`Editor::claim_slot`]) and everything else goes ahead around it. A
    /// discarded core draft still closing refuses only what would open a draft of its own, or ask
    /// for the frame its own read-back is already bringing.
    pub(crate) fn gesture_refusal(&self, starting: Starting) -> Option<String> {
        let held = match self.gesture.as_ref()? {
            Gesture::Closing { .. } => {
                return starting.waits_for_closing().then(|| {
                    format!(
                        "Wait for the discarded draft to close {}",
                        starting.clause()
                    )
                });
            }
            Gesture::Core(gesture) if gesture.yields() => return None,
            // A brush still opening has no draft to hand over yet, so what would take the slot
            // waits for it; everything else goes ahead around it.
            Gesture::Core(gesture) if gesture.kind.armed() => {
                return starting.claims_slot().then(|| {
                    format!("Wait for the brush to finish opening {}", starting.clause())
                });
            }
            Gesture::Core(gesture) => match &gesture.kind {
                Kind::Slider(_) => {
                    return Some(format!(
                        "Finish or discard the slider draft {}",
                        starting.clause()
                    ));
                }
                Kind::Mask(mask) => {
                    if starting == Starting::Mode {
                        return Some(format!(
                            "Apply or Cancel the {} gesture before leaving Mask mode",
                            mask.shape.op.label().to_lowercase()
                        ));
                    }
                    "Apply or Cancel the mask gesture"
                }
            },
            Gesture::Crop(_) => {
                if starting == Starting::Mode {
                    return Some("Apply or Cancel the crop draft before leaving this mode".into());
                }
                "Apply or Cancel the crop draft"
            }
        };
        Some(format!("{held} {}", starting.clause()))
    }

    /// Take the slot for a new draft. An armed brush gives its core draft up and the returned
    /// draft is the one to end: a new core gesture's `draft.begin` task and the crop's first
    /// request cancel it before anything else, so no later request can overtake the cancel. Every
    /// other gesture was refused before this is called, and leaves the slot as it is.
    pub(crate) fn claim_slot(&mut self) -> Option<DraftId> {
        self.put_down_brush()
            .and_then(|draft| draft.draft_id.clone())
    }

    /// Take an armed brush out of the slot, when that is what the slot holds.
    fn put_down_brush(&mut self) -> Option<CoreDraft> {
        if !matches!(&self.gesture, Some(Gesture::Core(gesture)) if gesture.yields()) {
            return None;
        }
        let Some(Gesture::Core(gesture)) = self.gesture.take() else {
            return None;
        };
        self.session.draft = None;
        self.event(
            "mask_draft_disarmed",
            json!({"draft_id": gesture.draft.draft_id.as_ref().map(DraftId::as_str)}),
        );
        Some(gesture.draft)
    }

    /// Put an armed brush down so a command that is not a gesture can run: its core draft closes —
    /// at once, or once its `draft.begin` has answered — and nothing is asked of the screen,
    /// because the command's own answer redraws it.
    pub(crate) fn disarm(&mut self) -> Task<Message> {
        if !matches!(&self.gesture, Some(Gesture::Core(gesture)) if gesture.kind.armed()) {
            return Task::none();
        }
        let Some(Gesture::Core(gesture)) = self.gesture.take() else {
            return Task::none();
        };
        self.session.draft = None;
        self.event(
            "mask_draft_disarmed",
            json!({"draft_id": gesture.draft.draft_id.as_ref().map(DraftId::as_str)}),
        );
        let mut draft = gesture.draft;
        let step = draft.handle(Event::Cancel);
        self.gesture = Some(Gesture::Closing {
            draft,
            reseed: false,
        });
        self.run(step)
    }

    /// A fresh local identity for the gesture about to open.
    pub(crate) fn next_gesture(&mut self) -> GestureId {
        self.gesture_serial += 1;
        GestureId(self.gesture_serial)
    }

    /// Open a core gesture: its `draft.begin` goes out now, after the cancel of any draft it
    /// displaced. `fields` are what the gesture already holds; they are sent once the draft exists.
    pub(crate) fn open_core(
        &mut self,
        kind: Kind,
        fields: Option<Value>,
        displaced: Option<DraftId>,
    ) -> Task<Message> {
        let (Some(state), Some(action)) = (&self.state, kind.action()) else {
            return Task::none();
        };
        let (asset, revision) = (state.asset.id.clone(), state.revision);
        let gesture = self.next_gesture();
        let target = kind.target();
        let (draft, step) = CoreDraft::open(gesture, revision, fields);
        self.gesture = Some(Gesture::Core(CoreGesture {
            asset: asset.clone(),
            draft,
            kind,
        }));
        debug_assert_eq!(step, Step::Begin, "a gesture opens with its draft.begin");
        tasks::draft_begin_task(
            self.owner.clone(),
            self.client,
            gesture,
            asset,
            action,
            target,
            displaced,
        )
    }

    /// Feed the open core gesture one event and run whatever it calls for.
    pub(crate) fn drive(&mut self, event: Event) -> Task<Message> {
        let step = match &mut self.gesture {
            Some(Gesture::Core(gesture)) => gesture.draft.handle(event),
            Some(Gesture::Closing { draft, .. }) => draft.handle(event),
            _ => return Task::none(),
        };
        self.close_if_discarded();
        self.run(step)
    }

    /// Release, Enter or Apply: commit the open core gesture once.
    pub(crate) fn release(&mut self) -> Task<Message> {
        let Some(gesture) = self.core_gesture() else {
            return Task::none();
        };
        if gesture.kind.armed() {
            self.status = "Paint a stroke on the photograph first".into();
            return Task::none();
        }
        if gesture.slider().is_some() {
            self.dragging = None;
        }
        self.drive(Event::Release)
    }

    /// Escape, Discard or a script: end the open core gesture and commit nothing.
    pub(crate) fn discard(&mut self) -> Task<Message> {
        self.drive(Event::Cancel)
    }

    /// One answer or intent of the draft lifecycle.
    pub(crate) fn draft_message(&mut self, message: DraftMessage) -> Task<Message> {
        match message {
            DraftMessage::Commit => self.release(),
            DraftMessage::Cancel => self.discard(),
            DraftMessage::Reapply => self.drive(Event::Reapply),
            DraftMessage::Begun { gesture, result } => {
                self.draft_begun(gesture, result.map(|draft| *draft))
            }
            DraftMessage::Committed {
                gesture,
                draft,
                result,
            } => self.draft_committed(gesture, &draft, result.map(|refresh| refresh.map(|r| *r))),
            DraftMessage::Reapplied {
                gesture,
                draft,
                result,
            } => self.draft_reapplied(gesture, &draft, result.map(|draft| *draft)),
            DraftMessage::Cancelled {
                draft,
                cancelled,
                reseed,
            } => self.draft_cancelled(&draft, cancelled, reseed),
        }
    }

    /// A new authoritative revision arrived while a core gesture was open. The draft is kept and
    /// marked, exactly as the crop draft is, so nothing is discarded without a decision.
    pub(crate) fn gesture_revision(&mut self, revision: u64) {
        let task = self.drive(Event::Revision(revision));
        // A revision answers with a notice or nothing: it never sends a request.
        drop(task);
    }

    /// A discarded core gesture leaves the screen at once, whatever its owner round trips are still
    /// doing: the field returns to the authoritative value, drafted frames still in the queue are
    /// held back, and the slot keeps only the closing draft.
    fn close_if_discarded(&mut self) {
        if !matches!(&self.gesture, Some(Gesture::Core(gesture)) if gesture.draft.closing()) {
            return;
        }
        let Some(Gesture::Core(gesture)) = self.gesture.take() else {
            return;
        };
        self.session.draft = None;
        // Nothing the gesture asked for reaches the screen after this: its drafted frames are
        // stopped and held below the delivery floor, so the next frame presented is the committed
        // one the cancel asks for. The drafted pixels already on screen stay until it lands. An
        // armed brush drafted nothing, so it has nothing to hold back.
        if gesture.draft.drafted() {
            self.preview_generation = self.preview_queue.cancel();
        }
        match &gesture.kind {
            Kind::Slider(slider) => {
                self.dragging = None;
                self.status = format!("{} draft discarded", slider.label);
                self.event("slider_draft_cancelled", json!({ "label": slider.label }));
                self.seed_values();
            }
            Kind::Mask(mask) => {
                self.status = format!("{} discarded", mask.shape.op.label());
                self.event("mask_draft_cancelled", json!({"op": mask.shape.op.label()}));
            }
        }
        if self.state.is_none() {
            self.settle_step(Settle::Preview);
        }
        self.gesture = Some(Gesture::Closing {
            draft: gesture.draft,
            reseed: true,
        });
    }

    fn run(&mut self, step: Step) -> Task<Message> {
        match step {
            Step::None | Step::Begin => Task::none(),
            Step::Set { draft_id, fields } => self.send_set(draft_id, fields),
            Step::Commit {
                draft_id,
                expected_revision,
            } => self.send_commit(draft_id, expected_revision),
            Step::Cancel(draft_id) => {
                let reseed = match &self.gesture {
                    Some(Gesture::Closing { reseed, .. }) => *reseed,
                    _ => true,
                };
                self.closing_cancel(draft_id, reseed)
            }
            Step::Reapply(draft_id) => {
                let Some(gesture) = self.core_gesture() else {
                    return Task::none();
                };
                tasks::draft_reapply_task(
                    self.owner.clone(),
                    self.client,
                    gesture.draft.gesture,
                    draft_id,
                )
            }
            Step::Conflicted => {
                let Some(Gesture::Core(gesture)) = &mut self.gesture else {
                    return Task::none();
                };
                let revision = self.state.as_ref().map(|state| state.revision);
                let (prefix, noun) = (gesture.kind.prefix(), gesture.kind.noun());
                let detail = match &mut gesture.kind {
                    Kind::Slider(slider) => json!({"label":slider.label,"revision":revision}),
                    Kind::Mask(mask) => {
                        // A conflict ends the pointer gesture: a drag must not carry on into a draft
                        // that can no longer be committed.
                        mask.shape.interrupt();
                        json!({ "revision": revision })
                    }
                };
                if let Some(session) = &mut self.session.draft {
                    session.conflicted = true;
                }
                self.status = format!("Changed elsewhere: discard the {noun} or reapply it");
                self.event(&format!("{prefix}_conflicted"), detail);
                self.settle_step(Settle::SliderDraft);
                Task::none()
            }
            Step::Refused => {
                let Some(gesture) = self.core_gesture() else {
                    return Task::none();
                };
                self.status = format!(
                    "Changed elsewhere: discard the {} or reapply it",
                    gesture.kind.noun()
                );
                self.settle_step(Settle::SliderDraft);
                Task::none()
            }
            Step::Done => {
                self.end_gesture();
                Task::none()
            }
        }
    }

    /// Drop the gesture from the slot. The core draft was ended by its own request.
    fn end_gesture(&mut self) {
        if matches!(&self.gesture, Some(Gesture::Core(_))) {
            self.session.draft = None;
            self.dragging = None;
        }
        if matches!(
            &self.gesture,
            Some(Gesture::Core(_) | Gesture::Closing { .. })
        ) {
            self.gesture = None;
        }
    }

    /// The one `draft.set` the state machine asked for, with the one preview job for the fields it
    /// accepted. Synchronous on purpose: see [`tasks::draft_set_now`]. The answer is taken up here,
    /// in the update that produced the fields.
    fn send_set(&mut self, draft_id: DraftId, fields: Value) -> Task<Message> {
        let Some(gesture) = self.core_gesture() else {
            return Task::none();
        };
        let (prefix, asset) = (gesture.kind.prefix(), gesture.asset.clone());
        self.event(
            &format!("{prefix}_set"),
            json!({"draft_id":draft_id.as_str(),"fields":fields}),
        );
        #[cfg(test)]
        if self.fake_sets.is_some() {
            let refused = self
                .fake_sets
                .as_mut()
                .and_then(std::collections::VecDeque::pop_front);
            let result = match refused {
                Some(error) => Err(error),
                None => crate::app::testing::accepted_set(self, &draft_id, &fields),
            };
            return self.draft_set(result);
        }
        let proxy = self.proxy_bounds();
        let result = tasks::draft_set_now(&self.owner, self.client, draft_id, asset, fields, proxy);
        self.draft_set(result)
    }

    /// One `draft.set` answered with the preview of the fields it accepted.
    pub(crate) fn draft_set(
        &mut self,
        result: Result<(Draft, PreviewJob, RoundTrip), String>,
    ) -> Task<Message> {
        // Only the gesture that sent it takes an answer up. The set runs synchronously, so no answer
        // outlives its gesture today; this keeps that true whatever path an answer takes, because
        // storing a discarded gesture's draft or queuing its frame would present what nobody holds.
        if self
            .core_gesture()
            .is_none_or(|gesture| gesture.draft.in_flight() != Some(Round::Set))
        {
            self.event("draft_set_dropped", json!({ "accepted": result.is_ok() }));
            return Task::none();
        }
        match result {
            Ok((set, job, round_trip)) => {
                let draft_revision = set.draft_revision;
                self.session.draft = Some(set.clone());
                self.preview_generation = self.request_preview(job);
                self.set_previewed(draft_revision, round_trip);
                self.drive(Event::Set(Ok(set)))
            }
            Err(error) => {
                self.set_unpreviewed(&error);
                let task = self.drive(Event::Set(Err(error)));
                // A drained gesture whose newest value has no frame of its own is still drained:
                // the frame on screen is the evidence of that, so a scripted step settles on it.
                if self
                    .core_gesture()
                    .is_some_and(|gesture| gesture.draft.drained())
                    && self
                        .slider_gesture()
                        .is_some_and(|slider| slider.unpreviewed)
                {
                    self.settle_step(Settle::SliderDraft);
                }
                task
            }
        }
    }

    /// The record that ties an input to the frame it will produce: the `draft.set` this answers
    /// carried the fields, and the preview job just queued for it is `generation`, which the
    /// `preview_displayed` event of its frame repeats. Without it a measurement can only guess
    /// which frame belongs to which input.
    fn set_previewed(&mut self, draft_revision: u64, round_trip: RoundTrip) {
        let generation = self.preview_generation;
        let Some(Gesture::Core(gesture)) = &mut self.gesture else {
            return;
        };
        let sent = gesture.draft.sent().cloned();
        match &mut gesture.kind {
            Kind::Slider(slider) => {
                slider.unpreviewed = false;
                let label = slider.label.clone();
                let value = sent.and_then(|fields| fields.get(&slider.parameter).cloned());
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
                self.status = format!("Drafting {label}…");
                self.event(
                    "slider_draft_preview",
                    json!({"generation":generation,"draft_revision":draft_revision,"value":value,"round_trip_ms":{"executor_wait":legs[0],"draft_set":legs[1],"preview_job":legs[2],"return":legs[3]},"loop":loop_timing}),
                );
            }
            Kind::Mask(mask) => {
                // `positions` is the path's length and not the path: a measurement needs to know
                // how much geometry the frame carries, and a log is not where a stroke is stored.
                let positions = mask.shape.brush().map(|stroke| stroke.captured().len());
                self.event(
                    "mask_draft_preview",
                    json!({"generation":generation,"draft_revision":draft_revision,
                           "positions":positions}),
                );
            }
        }
    }

    /// The draft accepted the fields but its preview job was refused, or the set itself was.
    fn set_unpreviewed(&mut self, error: &str) {
        let Some(Gesture::Core(gesture)) = &mut self.gesture else {
            return;
        };
        let draft_revision = gesture.draft.draft_revision;
        let sent = gesture.draft.sent().cloned();
        match &mut gesture.kind {
            Kind::Slider(slider) => {
                // No frame of its own is coming. A drafted RAW temperature or tint is not this: the
                // core previews it approximately on the developed planes. What is left is a RAW
                // whose development is not in memory at all — evicted while a redevelopment or a
                // source preparation of an earlier request is in flight — which the core answers
                // with preparation-required rather than a stale frame. The gesture goes on, its
                // release commits and that commit's frame waits for the development; the status
                // bar says so instead of showing the error code.
                slider.unpreviewed = true;
                let label = slider.label.clone();
                let value = sent.and_then(|fields| fields.get(&slider.parameter).cloned());
                self.status = if error.starts_with(ErrorKind::PreparationRequired.code()) {
                    format!(
                        "{label} cannot be previewed until the RAW development is ready; it shows on release"
                    )
                } else {
                    error.to_owned()
                };
                self.event(
                    "slider_draft_unpreviewed",
                    json!({"draft_revision":draft_revision,"value":value,"error":error}),
                );
            }
            Kind::Mask(_) => self.status = error.to_owned(),
        }
    }

    /// Commit the core draft once.
    fn send_commit(&mut self, draft_id: DraftId, expected_revision: u64) -> Task<Message> {
        let Some(gesture) = self.core_gesture() else {
            return Task::none();
        };
        let (prefix, id, asset) = (
            gesture.kind.prefix(),
            gesture.draft.gesture,
            gesture.asset.clone(),
        );
        let mutation = mutation(expected_revision);
        self.event(
            &format!("{prefix}_commit"),
            json!({"draft_id":draft_id.as_str(),"request_id":mutation.request_id,"expected_revision":expected_revision}),
        );
        let proxy = self.proxy_bounds();
        tasks::draft_commit_task(
            self.owner.clone(),
            self.client,
            id,
            draft_id,
            asset,
            mutation,
            proxy,
        )
    }

    /// End a closing core draft at the owner. The committed frame is read back after the cancel,
    /// in the same task, so the session it carries can no longer hold the draft.
    fn closing_cancel(&mut self, draft_id: DraftId, reseed: bool) -> Task<Message> {
        let reseed = reseed
            .then(|| {
                self.state
                    .as_ref()
                    .map(|state| (state.asset.id.clone(), self.displayed_entry()))
            })
            .flatten()
            .map(|(asset, entry)| (asset, entry, self.proxy_bounds()));
        tasks::draft_cancel_task(self.owner.clone(), self.client, draft_id, reseed)
    }

    /// `draft.begin` answered. Only the gesture that asked takes it up; a discarded gesture cancels
    /// the draft it answers with, and an answer nobody asked for ends its draft rather than leaving
    /// one behind.
    fn draft_begun(&mut self, gesture: GestureId, result: Result<Draft, String>) -> Task<Message> {
        let seen = self.state.as_ref().map_or(0, |state| state.revision);
        let owned = match &self.gesture {
            Some(Gesture::Core(open)) => open.draft.answers(gesture, None),
            Some(Gesture::Closing { draft, .. }) => draft.answers(gesture, None),
            _ => false,
        };
        if !owned {
            let dropped = result.as_ref().ok().map(|draft| draft.draft_id.clone());
            self.event(
                "draft_begin_dropped",
                json!({"draft_id":dropped.as_ref().map(DraftId::as_str)}),
            );
            return match dropped {
                Some(draft_id) => {
                    tasks::draft_cancel_task(self.owner.clone(), self.client, draft_id, None)
                }
                None => Task::none(),
            };
        }
        let open = matches!(&self.gesture, Some(Gesture::Core(_)));
        if open && let Ok(opened) = &result {
            self.session.draft = Some(opened.clone());
        }
        let error = result.as_ref().err().cloned();
        let task = self.drive(Event::Begun {
            answer: result,
            seen,
        });
        if open && let Some(error) = error {
            self.status = error;
        }
        task
    }

    /// `draft.commit` answered. A real outcome merges into history like any other command; a no-op
    /// ends the gesture with no entry and no history refresh; a refusal keeps the draft, so it can
    /// be discarded or reapplied deliberately.
    fn draft_committed(
        &mut self,
        gesture: GestureId,
        draft_id: &DraftId,
        result: Result<Option<Refresh>, String>,
    ) -> Task<Message> {
        self.busy = false;
        let owned = self
            .core_gesture()
            .is_some_and(|open| open.draft.answers(gesture, Some(draft_id)));
        if !owned {
            // Nothing else ends a gesture while its commit is in flight, so this does not happen;
            // if it ever did, the refresh is still the owner's own state.
            if let Ok(Some(refresh)) = result {
                self.accept(refresh);
            }
            return Task::none();
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let prefix = self
                    .core_gesture()
                    .map_or("draft", |open| open.kind.prefix());
                if error.starts_with(ErrorKind::Conflict.code())
                    && let Some(mask) = self.mask_gesture_mut()
                {
                    mask.shape.interrupt();
                }
                // A refusal belongs in the evidence log beside the commit it answers: a run that
                // shows the request and not its outcome cannot be read afterwards.
                self.event(&format!("{prefix}_refused"), json!({ "reason": error }));
                self.status = error.clone();
                self.settle_step(Settle::SliderDraft);
                return self.drive(Event::Committed(Err(error)));
            }
        };
        let Some(Gesture::Core(open)) = self.gesture.take() else {
            return Task::none();
        };
        self.session.draft = None;
        self.dragging = None;
        match (open.kind, outcome) {
            (Kind::Slider(_), Some(refresh)) => {
                self.accept(refresh);
                Task::none()
            }
            (Kind::Slider(slider), None) => {
                self.status = format!("{} unchanged; nothing was committed", slider.label);
                self.event("slider_draft_noop", json!({ "label": slider.label }));
                // The drafted pixels are still on screen and they are not the committed ones, so
                // the step settles on the render that replaces them rather than on this message.
                match self.reseed_committed() {
                    Some(task) => task,
                    None => {
                        self.settle_step(Settle::Preview);
                        Task::none()
                    }
                }
            }
            (Kind::Mask(mask), Some(refresh)) => self.mask_committed(mask.shape, refresh),
            (Kind::Mask(_), None) => {
                self.status = "The mask gesture changed nothing; nothing was committed".into();
                self.refresh_mask_overlay()
            }
        }
    }

    /// After a gesture ended without committing, the drafted pixels are still on screen and the
    /// field still holds the drafted text: put both back to the committed state. The field is
    /// re-seeded from the values already fetched with the recipe, and one preview job replaces the
    /// pixels — no `asset.state` request and no history refresh. `None` when there is nothing to
    /// show.
    pub(crate) fn reseed_committed(&mut self) -> Option<Task<Message>> {
        let asset = self.state.as_ref()?.asset.id.clone();
        let entry = self.displayed_entry();
        self.seed_values();
        let proxy = self.proxy_bounds();
        Some(tasks::current_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            entry,
            proxy,
        ))
    }

    /// `draft.reapply` answered: the draft is based on the current revision again and the fields
    /// this client set are re-sent, so the drafted preview returns. An answer for a gesture that is
    /// no longer open, or for another draft, is dropped: storing it would report a draft that
    /// nothing holds, and clearing a round trip would unblock a newer gesture's own.
    fn draft_reapplied(
        &mut self,
        gesture: GestureId,
        draft_id: &DraftId,
        result: Result<Draft, String>,
    ) -> Task<Message> {
        let (owned, open) = match &self.gesture {
            Some(Gesture::Core(core)) => (core.draft.answers(gesture, Some(draft_id)), true),
            Some(Gesture::Closing { draft, .. }) => (draft.answers(gesture, Some(draft_id)), false),
            _ => (false, false),
        };
        if !owned {
            self.event(
                "draft_reapply_dropped",
                json!({"draft_id":draft_id.as_str(),"accepted":result.is_ok()}),
            );
            return Task::none();
        }
        if open {
            match &result {
                Ok(rebased) => {
                    self.session.draft = Some(rebased.clone());
                    if let Some(mask) = self.mask_gesture_mut() {
                        mask.shape.interrupt();
                    }
                    if let Some(slider) = self.slider_gesture() {
                        self.status = format!("Drafting {}…", slider.label);
                    }
                }
                Err(error) => self.status = error.clone(),
            }
        }
        self.drive(Event::Reapplied(result))
    }

    /// `draft.cancel` answered, with the committed frame read after it when one was asked for.
    fn draft_cancelled(
        &mut self,
        draft_id: &DraftId,
        cancelled: Result<(), String>,
        reseed: Option<Result<Box<tasks::PreviewPayload>, String>>,
    ) -> Task<Message> {
        if matches!(&self.gesture, Some(Gesture::Closing { draft, .. })
            if draft.draft_id.as_ref() == Some(draft_id))
        {
            let _ = self.drive(Event::Cancelled);
        }
        if let Err(error) = cancelled {
            self.event(
                "draft_cancel_failed",
                json!({"draft_id":draft_id.as_str(),"error":error}),
            );
        }
        match reseed {
            Some(result) => self.dispatch(Message::PreviewLoaded(result)),
            None => Task::none(),
        }
    }
}
