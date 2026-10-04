//! The one gesture this client holds, and the driver that runs its core draft.
//!
//! A client holds at most one draft, so the desktop has exactly one place for it: [`Editor::gesture`].
//! A slider, a mask shape or stroke and the crop frame each draft through the core as one
//! [`CoreGesture`]: a [`CoreDraft`] state machine with a small kind of its own. Whether anything
//! may start is answered in one place, [`Editor::gesture_refusal`].
//!
//! The driver runs what the state machine answers. `draft.begin`, `draft.set`, `draft.reapply` and
//! `draft.cancel` are session-only requests, run synchronously on this thread in the update that
//! asks for them ([performance rule 12](../../../../docs/engineering/performance-rules.md#rules)):
//! a gesture's first `draft.set` goes in the update of its press, and a discarded draft has ended
//! at the owner before the update that discarded it is over, so nothing can overtake its cancel.
//! Only `draft.commit` is an owner task, whose answer comes back as [`DraftMessage::Committed`]
//! naming the gesture and draft it belongs to.
use crate::{
    app::{
        Editor,
        crop::CropGesture,
        draft::{CoreDraft, Event, GestureId, Round, Step},
        message::{Message, draft::DraftMessage, preview::PreviewMessage},
        outcome::Outcome,
        tasks::{self, PreviewPayload, Refresh, RoundTrip, mutation},
    },
    mask_draft::{ContentMap, MaskDraft},
    state::{self, IN_FLIGHT},
};
use iced::Task;
use luxforge_core::{AssetId, Draft, DraftId, DraftTarget, ErrorKind, IdentityKind, PreviewJob};
use serde_json::{Value, json};

/// This client's one gesture and the core draft behind it.
#[derive(Clone, Debug)]
pub(crate) struct CoreGesture {
    pub(crate) asset: AssetId,
    pub(crate) draft: CoreDraft,
    pub(crate) kind: Kind,
}

/// What a core gesture drafts.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Slider(SliderGesture),
    Mask(MaskGesture),
    Crop(CropGesture),
}

/// A generated control's gesture: one declared field of one action, dragged.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderGesture {
    /// The action being drafted and the one field this gesture moves.
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// The control's label, for the status line.
    pub(crate) label: String,
    /// The host-owned target the draft was begun with: the identities it names, by parameter name.
    pub(crate) target: DraftTarget,
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
    /// A Reapply asks for the rebased draft's geometry. The answer must still belong to this
    /// draft and base when it arrives; an intervening commit cannot give the kept stroke a map
    /// from another stack.
    pub(crate) map_draft: Option<MaskMapDraft>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaskMapDraft {
    pub(crate) draft_id: DraftId,
    pub(crate) base_revision: u64,
    pub(crate) draft_revision: u64,
}

impl MaskGesture {
    /// The fields the gesture would commit now.
    pub(crate) fn fields(&self) -> Value {
        Value::Object(self.shape.fields())
    }
}

/// What wants to start. Each variant declares which halves of [`Editor::gesture_refusal`] it
/// answers to ([`Starting::halves`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Starting {
    /// A slider gesture of a generated control: one draft and editable, but it goes ahead while a
    /// request is in flight, since its own round trips wait their turn.
    Slider,
    /// A mask shape or stroke gesture: all three halves.
    Mask,
    /// A `mask.*` command from the Masks panel or a generated `mask.*` control: all three halves.
    MaskCommand,
    /// The crop draft: all three halves.
    Crop,
    /// Another canvas mode: one draft only. A mode is the session's view state, which neither a
    /// historical preview nor a request in flight holds back.
    Mode,
    /// A pick on the photograph: all three halves. A sample-apply pick commits; a point pick
    /// commits nothing but answers to the same rule, so one sentence describes every canvas pick.
    /// A pick that commits asks again once its locate or query has answered.
    Pick,
    /// The components gallery: one draft and no request in flight. The gallery is this desktop's
    /// own view, so a historical preview does not hold it back.
    Gallery,
    /// Compare with the original: one draft and no request in flight. It selects the Original
    /// entry from the current state or a previewed one.
    Compare,
    /// A preset applied to the photograph, as the section's rows read it: one draft only. The
    /// section's own enabled state carries the rest, and the apply itself is a
    /// [`Starting::Action`].
    Preset,
    /// A discrete control's one commit: a button, a toggle, a choice, a field's Enter, a reset or
    /// a palette action. All three halves.
    Action,
    /// Undo, Redo or Restore, which move the current entry at once: one draft and no request in
    /// flight. Never the editable half, since Restore runs from a previewed entry.
    History,
    /// A preview of the committed state for the view's own sake — a refit to new bounds, or a new
    /// mask overlay: one draft only. It runs while a request is in flight or an entry is previewed.
    Refit,
    /// An export of the displayed entry: no request in flight only. The one-draft rule does not
    /// apply, since an open draft does not change the displayed entry an export writes.
    Export,
}

/// Which halves of the one refusal answer a start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Halves {
    /// The one-draft rule: an open draft is finished deliberately before this may start.
    pub(crate) one_draft: bool,
    /// A photograph is open and the current state, not a previewed entry, is on screen.
    pub(crate) editable: bool,
    /// No request of this desktop's is in flight.
    pub(crate) busy: bool,
}

impl Starting {
    /// The halves this start answers to, declared once per variant.
    pub(crate) const fn halves(self) -> Halves {
        let (one_draft, editable, busy) = match self {
            Self::Mask | Self::MaskCommand | Self::Crop | Self::Pick | Self::Action => {
                (true, true, true)
            }
            Self::Slider => (true, true, false),
            Self::Gallery | Self::History | Self::Compare => (true, false, true),
            Self::Mode | Self::Preset | Self::Refit => (true, false, false),
            Self::Export => (false, false, true),
        };
        Halves {
            one_draft,
            editable,
            busy,
        }
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
            Self::Action => "before running another edit",
            Self::History => "before undoing, redoing or restoring",
            Self::Refit => "before refitting the preview",
            Self::Export => "before exporting",
        }
    }
}

impl Kind {
    /// The action the core draft runs: a control's action, the `mask.*` method a shape commits or
    /// the crop frame's declared action.
    pub(crate) fn action(&self) -> Option<String> {
        match self {
            Self::Slider(slider) => Some(slider.action.clone()),
            Self::Mask(mask) => mask.shape.method().map(str::to_owned),
            Self::Crop(crop) => Some(crop.action.clone()),
        }
    }

    /// The objects the core draft edits, by the identity parameters its action declares.
    ///
    /// A shape or stroke gesture names what its command declares: the command it commits through
    /// is looked up by the method it drafts, and each identity that command declares is filled from
    /// the mask and component the shape is on ([`luxforge_core::declared_target`]), so the brush's
    /// `mask.add-stroke` and a gradient's `mask.set-linear` are one path and neither spells a
    /// field. A slider carries the target it was begun with, and the crop frame edits no object.
    pub(crate) fn target(&self) -> DraftTarget {
        match self {
            Self::Slider(slider) => slider.target.clone(),
            Self::Mask(mask) => mask
                .shape
                .method()
                .and_then(luxforge_core::mask::commands::find)
                .map(|command| {
                    luxforge_core::declared_target(&command.action, |kind| {
                        match kind {
                            IdentityKind::Mask => mask.shape.mask.as_ref().map(|id| id.as_str()),
                            IdentityKind::Component => {
                                mask.shape.component.as_ref().map(|id| id.as_str())
                            }
                            _ => None,
                        }
                        .map(str::to_owned)
                    })
                })
                .unwrap_or_default(),
            Self::Crop(_) => DraftTarget::new(),
        }
    }

    /// The prefix of every evidence event this gesture records.
    pub(crate) fn prefix(&self) -> &'static str {
        match self {
            Self::Slider(_) => "slider_draft",
            Self::Mask(_) => "mask_draft",
            Self::Crop(_) => "crop_draft",
        }
    }

    /// What the status line and the Changed elsewhere notice call this gesture.
    pub(crate) fn noun(&self) -> &'static str {
        match self {
            Self::Slider(_) => "slider draft",
            Self::Mask(_) => "mask gesture",
            Self::Crop(_) => "crop draft",
        }
    }

    /// A `draft.set` of this gesture is answered with a preview of the drafted stack. The crop frame
    /// is drawn over its input stage, which no drafted field changes, so it asks for none.
    pub(crate) fn previews(&self) -> bool {
        !matches!(self, Self::Crop(_))
    }

    /// The fields a changed gesture sends ([`Event::Changed`]), built once its draft can send them:
    /// a mask shape's, after its capture is checked, since a capture that failed — refused as it
    /// was painted, or too long once decimated — has no posted path and must never become an empty
    /// stroke. The refusal is left in `refused`. A slider and the crop frame offer theirs.
    pub(crate) fn build(&self, refused: &mut Option<luxforge_core::Error>) -> Option<Value> {
        let Self::Mask(mask) = self else {
            return None;
        };
        if let Some(error) = mask.shape.capture_error() {
            *refused = Some(error);
            return None;
        }
        Some(mask.fields())
    }

    /// End the pointer gesture in progress: a drag must not carry on into a draft that can no
    /// longer be committed as it is.
    fn interrupt(&mut self) {
        match self {
            Self::Slider(_) => {}
            Self::Mask(mask) => mask.shape.interrupt(),
            Self::Crop(crop) => crop.frame.interrupt(),
        }
    }
}

impl CoreGesture {
    pub(crate) fn slider(&self) -> Option<&SliderGesture> {
        match &self.kind {
            Kind::Slider(slider) => Some(slider),
            _ => None,
        }
    }

    pub(crate) fn mask(&self) -> Option<&MaskGesture> {
        match &self.kind {
            Kind::Mask(mask) => Some(mask),
            _ => None,
        }
    }

    pub(crate) fn crop(&self) -> Option<&CropGesture> {
        match &self.kind {
            Kind::Crop(crop) => Some(crop),
            _ => None,
        }
    }
}

impl Editor {
    /// The open core gesture.
    pub(crate) fn core_gesture(&self) -> Option<&CoreGesture> {
        self.gesture.as_deref()
    }

    pub(crate) fn core_gesture_mut(&mut self) -> Option<&mut CoreGesture> {
        self.gesture.as_deref_mut()
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
        match &mut self.core_gesture_mut()?.kind {
            Kind::Mask(mask) => Some(mask),
            _ => None,
        }
    }

    /// The shape the canvas draws and the panel reads: the open mask gesture's, or the brush in hand
    /// ([`Editor::held_mask`]).
    pub(crate) fn mask_shape(&self) -> Option<&MaskDraft> {
        self.held_mask().map(|mask| &mask.shape)
    }

    /// The (action, parameter) of the control whose slider gesture is open.
    pub(crate) fn drafting_control(&self) -> Option<(&str, &str)> {
        self.slider_gesture()
            .map(|slider| (slider.action.as_str(), slider.parameter.as_str()))
    }

    /// Why `starting` cannot start now, in the words the status bar uses, or `None` when it can:
    /// the one answer to "may this start" behind every start site, each of which writes the reason
    /// to the status bar. It asks only the halves `starting` declares ([`Starting::halves`]), in
    /// order: the one-draft rule, then the view model's one editability rule
    /// ([`state::editable_refusal`]), then no request in flight ([`IN_FLIGHT`]). With no draft
    /// open, a start taking the editable and busy halves is refused exactly as
    /// [`state::edit_refusal`] answers the models.
    ///
    /// A slider, mask or crop gesture's release is answered by [`Editor::release_refusal`] instead.
    pub(crate) fn gesture_refusal(&self, starting: Starting) -> Option<String> {
        // A held mask tool owns every other start. Export takes no one-draft half, so it asks
        // even while the tool's own core draft is open.
        if (self.core_gesture().is_none() || starting == Starting::Export)
            && !matches!(starting, Starting::Mask | Starting::Refit)
            && let Some(reason) = self.mask_tool_refusal()
        {
            return Some(reason);
        }
        let halves = starting.halves();
        if halves.one_draft
            && let Some(reason) = self.draft_refusal(starting)
        {
            return Some(reason);
        }
        if halves.editable
            && let Some(reason) =
                state::editable_refusal(self.document.state.as_ref(), &self.session)
        {
            return Some(reason.into());
        }
        (halves.busy && self.busy).then(|| IN_FLIGHT.into())
    }

    /// The one-draft rule, answered once: a client holds one draft, so any gesture that needs one
    /// waits for the open gesture to be applied, cancelled or finished, and so does any mode
    /// change, pick, preset, gallery or comparison that would displace or pause it. A brush in hand
    /// holds no draft until its press, so it is no part of this rule.
    fn draft_refusal(&self, starting: Starting) -> Option<String> {
        let gesture = self.core_gesture()?;
        let held = match (&gesture.kind, starting) {
            (Kind::Slider(_), _) => {
                return Some(format!(
                    "Finish or discard the slider draft {}",
                    starting.clause()
                ));
            }
            (Kind::Mask(mask), Starting::Mode) => {
                return Some(format!(
                    "Finish or cancel the {} gesture before leaving Mask mode",
                    mask.shape.op.label().to_lowercase()
                ));
            }
            (Kind::Crop(_), Starting::Mode) => {
                return Some("Apply or Cancel the crop draft before leaving this mode".into());
            }
            // A mask gesture is finished by its release, or by Done for a brush; a crop by Apply.
            (Kind::Mask(_), _) => format!("Finish or cancel the {}", gesture.kind.noun()),
            (kind, _) => format!("Apply or Cancel the {}", kind.noun()),
        };
        Some(format!("{held} {}", starting.clause()))
    }

    /// A fresh local identity for the gesture about to open.
    pub(crate) fn next_gesture(&mut self) -> GestureId {
        self.gesture_serial += 1;
        GestureId(self.gesture_serial)
    }

    /// Open a core gesture under the identity `gesture`: its `draft.begin` runs now, on this thread,
    /// and `fields` — what the gesture already holds — go out as its first `draft.set` in the same
    /// update. A refused begin opens nothing: what the start put up ends, and the status bar says
    /// why.
    pub(crate) fn open_core(
        &mut self,
        gesture: GestureId,
        kind: Kind,
        fields: Option<Value>,
    ) -> Task<Message> {
        let (Some(state), Some(action)) = (&self.document.state, kind.action()) else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let opened = match self.begin_draft(asset.clone(), &action, kind.target()) {
            Ok(opened) => opened,
            Err(error) => {
                self.controls.dragging = None;
                if matches!(kind, Kind::Crop(_)) {
                    self.crop_ended();
                }
                self.status.text = error;
                return Task::none();
            }
        };
        self.session.draft = Some(opened.clone());
        let (draft, step) = CoreDraft::open(gesture, opened, fields);
        self.gesture = Some(Box::new(CoreGesture { asset, draft, kind }));
        self.run(step)
    }

    /// `draft.begin`, synchronously ([`tasks::draft_begin_now`]).
    fn begin_draft(
        &mut self,
        asset: AssetId,
        action: &str,
        target: DraftTarget,
    ) -> Result<Draft, String> {
        #[cfg(test)]
        if let Some(stand_in) = &mut self.stand_in {
            return stand_in.begin(self.document.state.as_ref(), asset, action);
        }
        tasks::draft_begin_now(&self.owner, self.client, asset, action, &target)
    }

    /// `draft.reapply`, synchronously ([`tasks::draft_reapply_now`]).
    fn reapply_draft(&mut self, draft_id: &DraftId) -> Result<Draft, String> {
        #[cfg(test)]
        if let Some(stand_in) = &mut self.stand_in {
            return stand_in.reapply(
                self.document.state.as_ref(),
                self.gesture.as_deref(),
                draft_id,
            );
        }
        tasks::draft_reapply_now(&self.owner, self.client, draft_id)
    }

    /// End a core draft at the owner now ([`tasks::draft_cancel_now`]). While a photograph is open
    /// the displayed entry's frame is read back after the cancel, so the session it carries no
    /// longer holds the draft; that frame is returned for the caller to take up.
    fn cancel_draft(&mut self, draft_id: &DraftId) -> Option<Result<Box<PreviewPayload>, String>> {
        let reseed = self
            .document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), self.displayed_entry()))
            .map(|(asset, entry)| (asset, entry, self.proxy_bounds()));
        let (cancelled, reseed) = self.cancel_request(draft_id, reseed);
        if let Err(error) = cancelled {
            self.event(
                "draft_cancel_failed",
                || json!({"draft_id":draft_id.as_str(),"error":error}),
            );
        }
        reseed
    }

    fn cancel_request(
        &self,
        draft_id: &DraftId,
        reseed: Option<tasks::Reseed>,
    ) -> tasks::Cancelled {
        #[cfg(test)]
        if self.stand_in.is_some() {
            return (Ok(()), None);
        }
        tasks::draft_cancel_now(&self.owner, self.client, draft_id, reseed)
    }

    /// Feed the open core gesture one event and run whatever it calls for. Fields a mask gesture
    /// changed ([`Event::Changed`]) are built here, when its draft can send them; a capture that
    /// cannot be posted builds none and says why on the status line.
    pub(crate) fn drive(&mut self, event: Event) -> Task<Message> {
        let Some(CoreGesture { draft, kind, .. }) = self.gesture.as_deref_mut() else {
            return Task::none();
        };
        let mut refused = None;
        let step = draft.handle_with(event, &mut || kind.build(&mut refused));
        if let Some(error) = refused {
            self.status.text = error.to_string();
        }
        self.run(step)
    }

    /// Why the open gesture cannot be committed now, in the words the status bar uses: the one
    /// refusal behind a pointer release, Enter, a script's commit and every Apply button, which
    /// reads it through the view model ([`crate::state::Inputs::apply_refusal`]), so a button never
    /// reads enabled while the request would be refused. Every kind is refused for a conflicted
    /// draft, which the core's `draft.commit` refuses anyway.
    /// Only the crop's Apply, a deliberate press rather than a pointer coming up, also waits out a
    /// historical preview on screen and another request in flight, and refuses a frame that
    /// commits no valid output. A slider or mask gesture released while another request is in
    /// flight commits, so its pointer never comes up on a draft left open.
    pub(crate) fn release_refusal(&self) -> Option<String> {
        if let Some(error) = self.mask_shape().and_then(MaskDraft::capture_error) {
            return Some(error.to_string());
        }
        if let Some(reason) = self.mask_shape().and_then(MaskDraft::placement_refusal) {
            return Some(reason);
        }
        let gesture = self.core_gesture()?;
        if self.gesture_conflicted() {
            return Some(format!(
                "Changed elsewhere: discard the {} or reapply it",
                gesture.kind.noun()
            ));
        }
        match &gesture.kind {
            Kind::Crop(_) if !self.at_current() => {
                Some("Return to the current state to apply".into())
            }
            Kind::Crop(_) if self.busy => Some(IN_FLIGHT.into()),
            Kind::Crop(crop) => crop.frame.output().err().map(|error| error.to_string()),
            Kind::Slider(_) | Kind::Mask(_) => None,
        }
    }

    /// Release, Enter or Apply: commit the open core gesture once.
    pub(crate) fn release(&mut self) -> Task<Message> {
        // The pointer is up whether or not the commit may go.
        if self.slider_gesture().is_some() {
            self.controls.dragging = None;
        }
        if let Some(reason) = self.release_refusal() {
            self.status.text = reason;
            // The refused gesture's frame is the evidence of the refusal.
            self.outcome(Outcome::DraftRefused);
            return Task::none();
        }
        self.view_plan.quiet_since = None;
        self.view_plan.quiet_settle_requested = true;
        self.view_plan.epoch = self.view_plan.epoch.saturating_add(1);
        self.view_plan.released_draft = self
            .core_gesture()
            .map(|gesture| gesture.draft.draft_id.clone());
        if self.view_plan.released_draft.is_some() {
            self.presentation.displayed_draft_id = None;
            self.presentation.displayed_draft_revision = None;
        }
        self.drive(Event::Release)
    }

    /// Escape, Discard or a script: end the open core gesture and commit nothing.
    pub(crate) fn discard(&mut self) -> Task<Message> {
        if self.core_gesture().is_some() {
            self.presentation.displayed_draft_id = None;
            self.presentation.displayed_draft_revision = None;
        }
        self.drive(Event::Cancel)
    }

    /// The Changed elsewhere notice's Reapply. The crop frame is also rebased onto the stage the
    /// current rows report, which may have turned or changed size; every other gesture only
    /// rebases its draft.
    fn reapply(&mut self) -> Task<Message> {
        if self.crop().is_some() {
            return self.crop_reapply();
        }
        self.drive(Event::Reapply)
    }

    /// One intent or owner answer of the draft lifecycle. With a brush in hand and no core gesture
    /// open, Done, Enter, Cancel and Escape put the brush down ([`Editor::put_brush_down`]).
    pub(crate) fn draft_message(&mut self, message: DraftMessage) -> Task<Message> {
        match message {
            DraftMessage::Commit | DraftMessage::Cancel
                if self.gesture.is_none() && self.armed.is_some() =>
            {
                self.put_brush_down();
                Task::none()
            }
            DraftMessage::Commit => self.release(),
            DraftMessage::Cancel => self.discard(),
            DraftMessage::Reapply => self.reapply(),
            DraftMessage::Committed {
                gesture,
                draft,
                result,
            } => self.draft_committed(gesture, &draft, result.map(|refresh| refresh.map(|r| *r))),
        }
    }

    /// A new authoritative revision arrived while a core gesture was open. The draft is kept and
    /// marked, so nothing is discarded without a decision.
    pub(crate) fn gesture_revision(&mut self, revision: u64) {
        // A revision answers with a notice or nothing: it never sends a request.
        drop(self.drive(Event::Revision(revision)));
    }

    /// The Changed elsewhere notice is up: the open core gesture's draft is conflicted.
    pub(crate) fn gesture_conflicted(&self) -> bool {
        self.core_gesture()
            .is_some_and(|gesture| gesture.draft.conflicted)
    }

    /// A discarded core gesture ends in the update that discarded it: it leaves the screen — the
    /// field returns to the authoritative value and drafted frames still in the queue are held back
    /// — its core draft is cancelled at the owner, and the committed frame read back after that
    /// cancel is taken up.
    fn discarded(&mut self, draft_id: &DraftId) -> Task<Message> {
        let Some(gesture) = self.gesture.take() else {
            return Task::none();
        };
        self.session.draft = None;
        self.presentation.displayed_draft_id = None;
        self.presentation.displayed_draft_revision = None;
        // Nothing the gesture asked for reaches the screen after this: its drafted frames are
        // stopped and held below the delivery floor, so the next frame presented is the committed
        // one read back below. The drafted pixels already on screen stay until it lands. A gesture
        // that drafted nothing has nothing to hold back.
        if gesture.draft.drafted() {
            self.presentation.preview_generation = self.cancel_preview_queue();
        }
        match &gesture.kind {
            Kind::Slider(slider) => {
                self.controls.dragging = None;
                self.status.text = format!("{} draft discarded", slider.label);
                self.event(
                    "slider_draft_cancelled",
                    || json!({ "label": slider.label }),
                );
                self.seed_values();
            }
            Kind::Mask(mask) => {
                self.status.text = format!("{} discarded", mask.shape.op.label());
                self.event(
                    "mask_draft_cancelled",
                    || json!({"op": mask.shape.op.label()}),
                );
            }
            Kind::Crop(crop) => self.crop_discarded(crop, &gesture.draft),
        }
        if self.document.state.is_none() {
            self.outcome(Outcome::NoNewFrame);
        }
        match self.cancel_draft(draft_id) {
            Some(reseed) => self.dispatch(Message::Preview(PreviewMessage::Loaded(reseed))),
            None => Task::none(),
        }
    }

    fn run(&mut self, step: Step) -> Task<Message> {
        match step {
            Step::None => Task::none(),
            Step::Set { draft_id, fields } => self.send_set(draft_id, fields),
            Step::Commit {
                draft_id,
                expected_revision,
            } => self.send_commit(draft_id, expected_revision),
            Step::Cancel(draft_id) => self.discarded(&draft_id),
            Step::Reapply(draft_id) => {
                let result = self.reapply_draft(&draft_id);
                self.reapplied(result)
            }
            Step::Conflicted => {
                let revision = self.document.state.as_ref().map(|state| state.revision);
                let Some(gesture) = self.core_gesture_mut() else {
                    return Task::none();
                };
                gesture.kind.interrupt();
                let (prefix, noun) = (gesture.kind.prefix(), gesture.kind.noun());
                if let Some(session) = &mut self.session.draft {
                    session.conflicted = true;
                }
                self.event(format_args!("{prefix}_conflicted"), || {
                    match self
                        .core_gesture()
                        .map(|gesture| (&gesture.kind, &gesture.draft))
                    {
                        Some((Kind::Slider(slider), _)) => {
                            json!({"label":slider.label,"revision":revision})
                        }
                        Some((Kind::Crop(crop), draft)) => crop.summary(draft),
                        Some((Kind::Mask(_), _)) | None => json!({ "revision": revision }),
                    }
                });
                self.changed_elsewhere(noun)
            }
            Step::Refused => match self.core_gesture() {
                Some(gesture) => self.changed_elsewhere(gesture.kind.noun()),
                None => Task::none(),
            },
        }
    }

    /// The open gesture's draft is conflicted: say so, and report it; the frame on screen, with this
    /// line, is what happened.
    fn changed_elsewhere(&mut self, noun: &str) -> Task<Message> {
        self.status.text = format!("Changed elsewhere: discard the {noun} or reapply it");
        self.outcome(Outcome::DraftRefused);
        Task::none()
    }

    /// The one `draft.set` the state machine asked for, with the one preview job for the fields it
    /// accepted when the gesture previews them. Synchronous on purpose: see [`tasks::draft_set_now`].
    /// The answer is taken up here, in the update that produced the fields.
    fn send_set(&mut self, draft_id: DraftId, fields: Value) -> Task<Message> {
        let Some(gesture) = self.core_gesture() else {
            return Task::none();
        };
        let (prefix, asset) = (gesture.kind.prefix(), gesture.asset.clone());
        let previews = gesture.kind.previews();
        self.event(
            format_args!("{prefix}_set"),
            || json!({"draft_id":draft_id.as_str(),"fields":fields}),
        );
        #[cfg(test)]
        if let Some(stand_in) = &mut self.stand_in {
            let result = match stand_in.sets.pop_front() {
                Some(error) => Err(error),
                None => crate::app::testing::accepted_set(self, &draft_id, &fields),
            };
            return self.draft_set(result);
        }
        let preview = previews.then(|| (asset, self.proxy_bounds()));
        // The GPU preview is planned with the tick's job at Fit and at 100% or more, where the
        // surface draws it.
        let gpu = self.gpu_ask();
        let result = tasks::draft_set_now(&self.owner, self.client, draft_id, fields, preview, gpu);
        self.draft_set(result)
    }

    /// One `draft.set` answered, with the preview of the fields it accepted when one was asked for.
    pub(crate) fn draft_set(
        &mut self,
        result: Result<(Draft, Option<PreviewJob>, RoundTrip), String>,
    ) -> Task<Message> {
        // Only the gesture that sent it takes an answer up. The set runs synchronously, so no answer
        // outlives its gesture today; this keeps that true whatever path an answer takes, because
        // storing a discarded gesture's draft or queuing its frame would present what nobody holds.
        if self
            .core_gesture()
            .is_none_or(|gesture| gesture.draft.in_flight() != Some(Round::Set))
        {
            self.event(
                "draft_set_dropped",
                || json!({ "accepted": result.is_ok() }),
            );
            return Task::none();
        }
        match result {
            Ok((answered, job, round_trip)) => {
                // The answer moves into the session, which keeps it whole; what the tick reads of
                // it below is its identity and revisions, so a brush stroke's path is not copied.
                let set = identity(&answered);
                self.session.draft = Some(answered);
                let mut drawn_on_gpu = false;
                if let Some(mut job) = job {
                    if self.view_plan.released_draft.as_ref() != Some(&set.draft_id) {
                        self.note_view_motion();
                    }
                    // A tick the surface draws from the GPU plan the answer carries makes no
                    // preview job and no upload ([`super::gpu_preview`]); any other takes the
                    // CPU path as before.
                    let (tick, boundary) = self.gpu_tick(&set, job.gpu.take());
                    match tick {
                        super::gpu_preview::Tick::Gpu => {
                            // The mask overlay's coverage follows the tick through its own
                            // worker, as on the CPU path, over the frame on screen, whose
                            // geometry a colour or mask draft does not change.
                            let content = self.presentation.presented_content;
                            self.request_mask_coverage(&job, content);
                            self.gpu_ticked(&set);
                            drawn_on_gpu = true;
                        }
                        super::gpu_preview::Tick::Cpu => {
                            job.boundary = boundary;
                            let boundary = job.boundary.is_some();
                            let (generation, requested_at) = if self.log.diagnostics.is_some()
                                && self.mask_gesture().is_some()
                            {
                                self.request_mask_preview_timed(job)
                            } else {
                                (self.request_preview(job), None)
                            };
                            if boundary {
                                self.gpu_boundary_requested(generation);
                            }
                            self.gpu_cpu_tick(&set, generation);
                            self.presentation.preview_generation = generation;
                            self.set_previewed(set.draft_revision, round_trip, requested_at);
                        }
                    }
                }
                let task = self.drive(Event::Set(Ok(set)));
                if drawn_on_gpu {
                    self.gpu_tick_presented();
                }
                task
            }
            Err(error) => {
                self.set_unpreviewed(&error);
                let task = self.drive(Event::Set(Err(error)));
                // A drained gesture whose newest value has no frame of its own is still drained:
                // the frame on screen shows that, so the refused value is reported.
                if self
                    .core_gesture()
                    .is_some_and(|gesture| gesture.draft.drained())
                    && self
                        .slider_gesture()
                        .is_some_and(|slider| slider.unpreviewed)
                {
                    self.outcome(Outcome::DraftRefused);
                }
                task
            }
        }
    }

    /// The record that ties an input to the frame it will produce: the `draft.set` this answers
    /// carried the fields, and the preview job just queued for it is `generation`, which the
    /// `preview_displayed` event of its frame repeats. Without it a measurement can only guess
    /// which frame belongs to which input.
    fn set_previewed(
        &mut self,
        draft_revision: u64,
        round_trip: RoundTrip,
        requested_at: Option<std::time::Instant>,
    ) {
        let generation = self.presentation.preview_generation;
        let Some(gesture) = self.gesture.as_deref_mut() else {
            return;
        };
        let value = sent_value(gesture);
        match &mut gesture.kind {
            Kind::Slider(slider) => {
                slider.unpreviewed = false;
                let label = slider.label.clone();
                let now = std::time::Instant::now();
                let legs = round_trip.legs_ms(now);
                let timing = self.log.loop_timing.get();
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
                self.status.text = format!("Drafting {label}…");
                self.event(
                    "slider_draft_preview",
                    || json!({"generation":generation,"draft_revision":draft_revision,"value":value,"round_trip_ms":{"executor_wait":legs[0],"draft_set":legs[1],"preview_job":legs[2],"return":legs[3]},"loop":loop_timing}),
                );
            }
            Kind::Mask(mask) => {
                // `positions` is the path's length and not the path: a measurement needs to know
                // how much geometry the frame carries, and a log is not where a stroke is stored.
                let positions = mask.shape.brush().map(|stroke| stroke.captured());
                self.event("mask_draft_preview", || {
                    let mut detail = json!({
                        "generation":generation,
                        "draft_revision":draft_revision,
                        "positions":positions,
                    });
                    if let Some(requested_at) = requested_at {
                        let legs = round_trip.legs_ms(requested_at);
                        detail["round_trip_ms"] = json!({
                            "executor_wait":legs[0],
                            "draft_set":legs[1],
                            "preview_job":legs[2],
                            "return_to_queue":legs[3],
                        });
                    }
                    detail
                });
            }
            Kind::Crop(_) => {}
        }
    }

    /// The draft accepted the fields but its preview job was refused, or the set itself was.
    fn set_unpreviewed(&mut self, error: &str) {
        let Some(gesture) = self.gesture.as_deref_mut() else {
            return;
        };
        let draft_revision = gesture.draft.draft_revision;
        let value = sent_value(gesture);
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
                self.status.text = if error.starts_with(ErrorKind::PreparationRequired.code()) {
                    format!(
                        "{label} cannot be previewed until the RAW development is ready; it shows on release"
                    )
                } else {
                    error.to_owned()
                };
                self.event(
                    "slider_draft_unpreviewed",
                    || json!({"draft_revision":draft_revision,"value":value,"error":error}),
                );
            }
            _ => self.status.text = error.to_owned(),
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
        self.event(format_args!("{prefix}_commit"),
            || json!({"draft_id":draft_id.as_str(),"request_id":mutation.request_id,"expected_revision":expected_revision}),
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

    /// `draft.commit` answered. A real outcome merges into history like any other command; a no-op
    /// ends the gesture with no entry and no history refresh; a refusal keeps the draft, so it can
    /// be discarded or reapplied deliberately.
    fn draft_committed(
        &mut self,
        gesture: GestureId,
        draft_id: &DraftId,
        result: Result<Option<Refresh>, String>,
    ) -> Task<Message> {
        // A gesture's commit never set `busy`, so its answer leaves it to whatever did.
        if !self
            .core_gesture()
            .is_some_and(|open| open.draft.answers(gesture, draft_id))
        {
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
                self.view_plan.released_draft = None;
                // The commit may have landed before its read-back failed: read the log once.
                self.resync();
                let prefix = self
                    .core_gesture()
                    .map_or("draft", |open| open.kind.prefix());
                if error.starts_with(ErrorKind::Conflict.code())
                    && let Some(open) = self.core_gesture_mut()
                {
                    open.kind.interrupt();
                }
                // A refusal belongs in the evidence log beside the commit it answers: a run that
                // shows the request and not its outcome cannot be read afterwards.
                self.event(
                    format_args!("{prefix}_refused"),
                    || json!({ "reason": error }),
                );
                self.status.text = error.clone();
                self.outcome(Outcome::DraftRefused);
                return self.drive(Event::Committed(Err(error)));
            }
        };
        let Some(open) = self.gesture.take() else {
            return Task::none();
        };
        let cancelled = open.draft.cancel_requested();
        self.session.draft = None;
        self.presentation.displayed_draft_id = None;
        self.presentation.displayed_draft_revision = None;
        self.controls.dragging = None;
        match (open.kind, outcome) {
            (Kind::Slider(_), Some(refresh)) => {
                self.accept(refresh);
                Task::none()
            }
            (Kind::Slider(slider), None) => {
                self.status.text = format!("{} unchanged; nothing was committed", slider.label);
                self.event("slider_draft_noop", || json!({ "label": slider.label }));
                // The drafted pixels are still on screen and they are not the committed ones, so
                // what happened is the render that replaces them, or no new frame at all.
                match self.reseed_committed() {
                    Some(task) => task,
                    None => {
                        self.outcome(Outcome::NoNewFrame);
                        Task::none()
                    }
                }
            }
            (Kind::Mask(mask), Some(refresh)) => {
                self.mask_committed(mask.shape, refresh, cancelled)
            }
            (Kind::Mask(_), None) => {
                self.status.text = "The mask gesture changed nothing; nothing was committed".into();
                self.refresh_mask_coverage()
            }
            (Kind::Crop(crop), outcome) => self.crop_committed(&crop, &open.draft, outcome),
        }
    }

    /// After a gesture ended without committing, the drafted pixels are still on screen and the
    /// field still holds the drafted text: put both back to the committed state. The field is
    /// re-seeded from the values already fetched with the recipe, and one preview job replaces the
    /// pixels — no `asset.state` request and no history refresh. `None` when there is nothing to
    /// show.
    pub(crate) fn reseed_committed(&mut self) -> Option<Task<Message>> {
        let asset = self.document.state.as_ref()?.asset.id.clone();
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

    /// The synchronous `draft.reapply` answered: the draft is based on the current revision again
    /// and the fields this client set are re-sent in this same update, so the drafted preview
    /// returns. A refusal keeps the draft conflicted and says why.
    fn reapplied(&mut self, result: Result<Draft, String>) -> Task<Message> {
        if self.core_gesture().is_none() {
            return Task::none();
        }
        let mut transform = Task::none();
        match &result {
            Ok(rebased) => {
                self.session.draft = Some(rebased.clone());
                if let Some(open) = self.core_gesture_mut() {
                    open.kind.interrupt();
                }
                // The kept shape is in content coordinates. Its previous map belongs to the
                // displaced stack, so a rebased mask must acquire the new map before another
                // pointer gesture. A new identity also rejects an older transform still in flight.
                if self.mask_gesture().is_some() {
                    let id = self.next_gesture();
                    let open = self.core_gesture_mut().expect("the rebased gesture");
                    open.draft.gesture = id;
                    if let Kind::Mask(mask) = &mut open.kind {
                        mask.map = None;
                        mask.map_draft = Some(MaskMapDraft {
                            draft_id: rebased.draft_id.clone(),
                            base_revision: rebased.base_revision,
                            draft_revision: rebased.draft_revision,
                        });
                    }
                    transform = tasks::transform_task(
                        self.owner.clone(),
                        self.client,
                        id,
                        rebased.asset_id.clone(),
                        None,
                        Some(rebased.draft_id.clone()),
                    );
                }
                if let Some(slider) = self.slider_gesture() {
                    self.status.text = format!("Drafting {}…", slider.label);
                }
            }
            Err(error) => self.status.text = error.clone(),
        }
        let task = self.drive(Event::Reapplied(result));
        // A crop draft's Reapply is over once its draft is rebased and the rebased frame's stage
        // is on screen.
        self.report_crop_stage();
        Task::batch([task, transform])
    }
}

/// What a `draft.set` tick reads of the draft it answered with — its identity, revisions, target
/// and conflict state — without the fields, which the session keeps.
fn identity(answered: &Draft) -> Draft {
    let Draft {
        draft_id,
        action,
        asset_id,
        base_revision,
        draft_revision,
        fields: _,
        target,
        conflicted,
    } = answered;
    Draft {
        draft_id: draft_id.clone(),
        action: action.clone(),
        asset_id: asset_id.clone(),
        base_revision: *base_revision,
        draft_revision: *draft_revision,
        fields: serde_json::Map::new(),
        target: target.clone(),
        conflicted: *conflicted,
    }
}

/// The value a slider gesture's last `draft.set` carried for its one field. Only a slider reads back
/// what it sent, and only that field: a mask gesture's sent fields hold a whole stroke's path.
fn sent_value(gesture: &CoreGesture) -> Option<Value> {
    let Kind::Slider(slider) = &gesture.kind else {
        return None;
    };
    gesture
        .draft
        .sent()
        .and_then(|fields| fields.get(&slider.parameter))
        .cloned()
}
