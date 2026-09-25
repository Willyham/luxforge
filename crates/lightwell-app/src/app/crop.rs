//! The crop-draft driver. Every crop change goes through `crop_update`, so the API-equivalent path
//! and the pointer path are the same code and only Apply ever commits.
use crate::{
    app::{
        Editor,
        evidence::Settle,
        gesture::Starting,
        message::{CropMessage, CropPointer, Message},
        tasks::{crop_preview_task, mutation},
    },
    crop_draft::{ANGLE_RAIL_STEP, CropDraft, Modifiers as DraftModifiers},
    state::{
        fields::{decimals_of, number_text},
        tools::crop_frame,
    },
};
use iced::{Task, widget::operation};
use lightwell_core::{
    CropPayload, CropStage, Layer, LayerId, MAX_ANGLE, MIN_ANGLE, ORIENTATION_EFFECT, Orientation,
    POINTER_MODE, insertion_index_among,
};
use lightwell_ui::geometry::{quantize, value_from_fraction};
use serde_json::{Value, json};

/// The scrollable around the photo, so a Space drag can scroll it while drafting a crop.
pub(crate) const SURFACE_ID: &str = "lightwell.surface";
/// How many changes from the idle section a starting draft holds for when it opens. A drag on the
/// angle's rail replaces its own queued position rather than adding to it, so only discrete changes
/// count, and a person cannot make this many in the one truncated preview a start waits for.
pub(crate) const QUEUED_CHANGES: usize = 32;

/// What a crop draft is waiting for its truncated preview to tell it: the layer it edits and the
/// payload it starts from are known from the stack, but the input stage is whatever that preview
/// renders, so the draft opens when its pixels arrive.
#[derive(Clone, Debug)]
pub(crate) struct PendingDraft {
    pub(crate) layer: Option<LayerId>,
    pub(crate) layer_index: usize,
    /// The orientation the layers ahead of the crop give its input stage.
    pub(crate) ahead: Orientation,
    pub(crate) payload: Option<CropPayload>,
    pub(crate) base_revision: u64,
    /// A reapply rebases the existing draft instead of opening a new one.
    pub(crate) reapply: bool,
    /// Changes made from the idle section while this draft is starting, applied in order once it
    /// opens, so none is lost to the truncated preview's round trip. At most [`QUEUED_CHANGES`].
    pub(crate) queued: Vec<CropMessage>,
}

/// A change a control of the crop section makes to an open draft. From the idle section the same
/// change first opens the draft, seeded from the committed crop exactly as Start seeds it. Turning
/// the Straighten guide off changes nothing without a draft, so only turning it on opens one.
fn changes_draft(message: &CropMessage) -> bool {
    matches!(
        message,
        CropMessage::Preset(_)
            | CropMessage::Lock
            | CropMessage::Swap
            | CropMessage::NudgeAngle(_)
            | CropMessage::AngleRail(_)
            | CropMessage::AngleRailReleased
            | CropMessage::SubmitAngle
            | CropMessage::Guide(true)
    )
}

/// The orientation the exact transforms among `layers` compose into, in stack order: what they do
/// to the stage a crop after them receives. A payload that does not read as an orientation adds
/// nothing, since such a stack does not render and no draft opens on it.
fn orientation_ahead(layers: &[Layer]) -> Orientation {
    layers
        .iter()
        .filter(|layer| layer.effect_id == ORIENTATION_EFFECT)
        .filter_map(|layer| serde_json::from_value::<Orientation>(layer.payload.clone()).ok())
        .fold(Orientation::NEUTRAL, Orientation::followed_by)
}

/// The angle a drag on the rail reaches at this fraction: on the rail's own step, within range.
fn rail_angle(fraction: f64) -> f64 {
    let angle = value_from_fraction(MIN_ANGLE, MAX_ANGLE, ANGLE_RAIL_STEP, fraction);
    quantize(
        angle,
        MIN_ANGLE,
        MAX_ANGLE,
        ANGLE_RAIL_STEP,
        decimals_of(ANGLE_RAIL_STEP),
    )
}

impl Editor {
    /// Every crop draft change goes through here, so the API-equivalent path and the pointer path
    /// are the same code.
    pub(crate) fn crop_update(&mut self, message: CropMessage) -> Task<Message> {
        if self.crop().is_none() && changes_draft(&message) {
            return self.idle_change(message);
        }
        match message {
            CropMessage::Option(option) => self.crop_option = option,
            CropMessage::Space(space) => self.crop_space = space,
            CropMessage::Guide(guide) => self.crop_guide = guide && self.crop().is_some(),
            CropMessage::Start => return self.crop_start(false),
            CropMessage::Reapply => return self.crop_start(true),
            CropMessage::PreviewReady(result) => match result {
                // The stack or the selection changed since the draft started, so the input stage
                // the owner planned is not the one the draft would open on, and nothing else will
                // arrive for it.
                Ok(job) if !self.crop_stage_current(&job.entry.id) => {
                    self.draft_preview_superseded(None);
                }
                Ok(job) => {
                    // A job an earlier start asked for is no longer the draft's once this one is
                    // requested, so this request superseding it ends nothing.
                    self.draft_generation = None;
                    self.draft_generation = Some(self.request_preview(*job));
                    self.status = "Rendering the crop's input stage…".into();
                }
                Err(error) => {
                    self.set_crop_pending(None);
                    self.status = error;
                    self.settle_step(Settle::Draft);
                }
            },
            CropMessage::Pointer(pointer) => {
                let Some(draft) = self.crop_mut() else {
                    return Task::none();
                };
                match pointer {
                    CropPointer::Begin { handle, x, y } => draft.begin(handle, (x, y)),
                    // A pointer move never calls an API and never logs: only the end of the gesture
                    // is one draft change.
                    CropPointer::Drag { x, y, option } => {
                        draft.drag((x, y), DraftModifiers { option })
                    }
                    CropPointer::End => {
                        draft.end();
                        self.crop_changed("crop_draft_changed");
                    }
                }
            }
            CropMessage::AngleText(text) => self.crop_angle = text,
            // A drag on the angle's rail: the fraction becomes an angle on the rail's step and the
            // draft follows it, rectangle and all, but a move logs nothing. The release is the one
            // draft change, as a frame gesture's end is.
            CropMessage::AngleRail(fraction) => {
                let Some(draft) = self.crop_mut() else {
                    return Task::none();
                };
                draft.set_angle(rail_angle(fraction));
                self.crop_angle = number_text(draft.stage.angle);
            }
            CropMessage::AngleRailReleased => self.crop_changed("crop_draft_changed"),
            CropMessage::SubmitAngle => {
                // Text that is not a number stays open for correcting; a number closes the box.
                let Ok(value) = self.crop_angle.trim().parse::<f64>() else {
                    self.status =
                        format!("Angle must be a number from {MIN_ANGLE} to {MAX_ANGLE} degrees");
                    return Task::none();
                };
                if self.editing_angle() {
                    self.editing = None;
                }
                if let Some(draft) = self.crop_mut() {
                    draft.set_angle(value);
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::NudgeAngle(step) => {
                if let Some(draft) = self.crop_mut() {
                    draft.nudge_angle(step);
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::Preset(index) => {
                let Some(preset) = crop_frame(&self.modules)
                    .map(|frame| frame.presets())
                    .and_then(|presets| presets.get(index).cloned())
                else {
                    return Task::none();
                };
                let custom = self.custom_ratio();
                if let Some(draft) = self.crop_mut() {
                    draft.set_preset(&preset, custom);
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::CustomWidth(text) => self.crop_custom.0 = text,
            CropMessage::CustomHeight(text) => self.crop_custom.1 = text,
            CropMessage::Swap => {
                if let Some(draft) = self.crop_mut() {
                    draft.swap();
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::Lock => {
                if let Some(draft) = self.crop_mut() {
                    draft.lock_toggle();
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::Pan { dx, dy } => {
                if dx == 0.0 && dy == 0.0 {
                    return Task::none();
                }
                return operation::scroll_by(
                    SURFACE_ID,
                    operation::AbsoluteOffset { x: dx, y: dy },
                );
            }
            CropMessage::Apply => match self.crop_apply() {
                Ok(task) => return task,
                Err(reason) => self.status = reason,
            },
            CropMessage::Cancel => {
                let Some(summary) = self.crop().map(CropDraft::summary) else {
                    return Task::none();
                };
                self.end_draft();
                self.event("crop_draft_discarded", summary);
                self.status = "Crop draft discarded".into();
            }
        }
        Task::none()
    }

    /// Open a draft, or re-read the stack for a reapply. The layer identity, its stored payload and
    /// the preview truncation come from the current stack; the input stage comes from that preview.
    fn crop_start(&mut self, reapply: bool) -> Task<Message> {
        if self.busy || !self.session.preview.can_edit() || reapply != self.crop().is_some() {
            return Task::none();
        }
        // One draft per client: another gesture is finished deliberately, never displaced. A
        // reapply rebases the crop draft that already holds the slot.
        if !reapply && let Some(reason) = self.gesture_refusal(Starting::Crop) {
            self.status = reason;
            return Task::none();
        }
        let Some((module_id, effect)) = crop_frame(&self.modules)
            .and_then(|frame| Some((frame.module.id.clone(), frame.effect()?.to_owned())))
            .filter(|_| self.state.is_some())
        else {
            return Task::none();
        };
        // An armed brush gives its core draft up to the crop; the truncated preview's own task
        // cancels it before anything else.
        let displaced = if reapply { None } else { self.claim_slot() };
        let Some(state) = self.state.as_ref() else {
            return Task::none();
        };
        let layers = &state.current_entry.snapshot.recipe.layers;
        let found = layers.iter().position(|layer| layer.effect_id == effect);
        // Without a crop layer the draft shows the stage the host would give a new one: after
        // every transform and edit, and before any finish layer, which acts on the crop's output.
        let layer_index =
            found.unwrap_or_else(|| insertion_index_among(&self.modules, layers, &effect));
        let pending = PendingDraft {
            layer: found.map(|index| layers[index].id.clone()),
            layer_index,
            ahead: orientation_ahead(&layers[..layer_index]),
            payload: found
                .and_then(|index| serde_json::from_value(layers[index].payload.clone()).ok()),
            base_revision: state.revision,
            reapply,
            queued: Vec::new(),
        };
        let asset = state.asset.id.clone();
        let layer_count = pending.layer_index;
        // The section keeps showing the angle the draft will open at while it starts.
        if !reapply {
            self.crop_angle = number_text(pending.payload.map_or(0.0, |payload| payload.angle));
        }
        self.set_crop_pending(Some(pending));
        self.status = "Preparing the crop's input stage…".into();
        // Starting a draft by any route — the section's own button, `R`, the mode strip or a
        // scripted `draft.start` — asks the session to enter this module's mode, so the strip shows
        // Crop selected for the whole life of the draft. A reapply keeps the mode already set.
        if !reapply {
            self.mode_sync = Some(module_id);
        }
        crop_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            layer_count,
            displaced,
        )
    }

    /// A starting or reapplied draft's input stage, planned from `entry`, is still the one it would
    /// open on: the session still shows the current state, that entry is the current one this
    /// desktop holds, and the state has not moved since the draft started. Decided from what the
    /// answer and the desktop already hold, so the job needs no currency request of its own.
    fn crop_stage_current(&self, entry: &lightwell_core::EntryId) -> bool {
        let (Some(state), Some(pending)) = (self.state.as_ref(), self.crop_pending()) else {
            return false;
        };
        self.session.preview.can_edit()
            && &state.current_entry.id == entry
            && state.revision == pending.base_revision
    }

    /// The truncated preview arrived, so the crop layer's input stage is known: open or rebase the
    /// draft against it.
    pub(crate) fn open_draft(&mut self, input: CropStage) {
        let Some(mut pending) = self.take_crop_pending() else {
            return;
        };
        let queued = std::mem::take(&mut pending.queued);
        if pending.reapply {
            match self.crop_mut() {
                Some(draft) => draft.rebase(
                    input,
                    pending.base_revision,
                    pending.layer,
                    pending.layer_index,
                    pending.ahead,
                ),
                None => return self.settle_step(Settle::Draft),
            }
        } else {
            let opened = match (pending.layer, pending.payload) {
                (Some(layer), Some(payload)) => Some(CropDraft::from_layer(
                    input,
                    payload,
                    layer,
                    pending.layer_index,
                    pending.base_revision,
                    &crop_frame(&self.modules)
                        .map(|frame| frame.presets())
                        .unwrap_or_default(),
                )),
                // An unreadable payload is never silently replaced by a neutral crop: the stored
                // layer stays exactly as it is and the draft does not open.
                (Some(_), None) => {
                    self.presenter.end_stage();
                    self.draft_generation = None;
                    self.status =
                        "The existing crop layer's payload cannot be read; no draft was opened"
                            .into();
                    return self.settle_step(Settle::Draft);
                }
                (None, _) => Some(CropDraft::neutral(
                    input,
                    pending.base_revision,
                    pending.layer_index,
                )),
            };
            self.set_crop(opened.map(|mut draft| {
                draft.ahead = pending.ahead;
                draft
            }));
        }
        self.crop_changed(if pending.reapply {
            "crop_draft_changed"
        } else {
            "crop_draft_started"
        });
        self.replay(queued);
        self.status = format!(
            "Crop draft on the layer's {} × {} input stage",
            input.width, input.height
        );
        self.settle_step(Settle::Draft);
    }

    /// A control of the idle section changed: open the draft seeded from the committed crop, as
    /// Start does, and hold the change for when the draft is ready, so the canvas enters crop mode
    /// with the frame already showing it. A draft that is still starting takes the change onto its
    /// queue instead. Nothing commits until Apply.
    fn idle_change(&mut self, message: CropMessage) -> Task<Message> {
        let mut changes = vec![message];
        if matches!(changes[0], CropMessage::SubmitAngle) {
            // Text that is not a number stays open for correcting, as it does while drafting; the
            // committed angle itself is no change, so the box just closes.
            let Ok(value) = self.crop_angle.trim().parse::<f64>() else {
                self.status =
                    format!("Angle must be a number from {MIN_ANGLE} to {MAX_ANGLE} degrees");
                return Task::none();
            };
            if self.editing_angle() {
                self.editing = None;
            }
            if self.crop_pending().is_none() && value == self.committed_crop_angle() {
                return Task::none();
            }
            changes.insert(0, CropMessage::AngleText(self.crop_angle.clone()));
        }
        let mut task = Task::none();
        if self.crop_pending().is_none() {
            // A release with no drag ahead of it has nothing to finish.
            if matches!(changes[0], CropMessage::AngleRailReleased) {
                return task;
            }
            // A start another gesture refuses says so and queues nothing.
            task = self.crop_start(false);
            if self.crop_pending().is_none() {
                return task;
            }
        }
        for change in changes {
            self.queue(change);
        }
        task
    }

    /// Hold one change for the starting draft. The section's angle box and rail show the angle the
    /// queued changes lead to, which the angle's text carries until the draft opens.
    fn queue(&mut self, change: CropMessage) {
        let angle = self.crop_angle.trim().parse::<f64>().unwrap_or(0.0);
        let prospective = match &change {
            CropMessage::AngleRail(fraction) => Some(rail_angle(*fraction)),
            CropMessage::NudgeAngle(step) => Some(angle + step),
            CropMessage::AngleText(text) => text.trim().parse::<f64>().ok(),
            _ => None,
        }
        .filter(|angle| angle.is_finite())
        .map(|angle| angle.clamp(MIN_ANGLE, MAX_ANGLE));
        let Some(pending) = self.crop_pending_mut() else {
            return;
        };
        // A rail drag is one change however far it moves: only its latest position matters.
        if matches!(
            (pending.queued.last(), &change),
            (Some(CropMessage::AngleRail(_)), CropMessage::AngleRail(_))
        ) {
            pending.queued.pop();
        } else if pending.queued.len() >= QUEUED_CHANGES {
            self.status = "The crop draft is still starting; that change was not applied".into();
            return;
        }
        pending.queued.push(change);
        if let Some(angle) = prospective {
            self.crop_angle = number_text(angle);
        }
    }

    /// Apply the changes the idle section queued, in order, to the draft that just opened: each
    /// through the drafting path, so each is one logged draft change. Choosing the ratio the draft
    /// already shows chosen changes nothing, because refitting the committed rectangle to it could
    /// only trim a pixel from it.
    fn replay(&mut self, queued: Vec<CropMessage>) {
        for change in queued {
            let Some(draft) = self.crop() else {
                return;
            };
            if let CropMessage::Preset(index) = change
                && crop_frame(&self.modules)
                    .and_then(|frame| frame.presets().get(index).cloned())
                    .is_some_and(|preset| preset.option == draft.preset)
            {
                continue;
            }
            // Every change the idle section queues is handled by a drafting arm that starts no
            // task, so nothing is dropped here.
            let _ = self.crop_update(change);
        }
    }

    /// The committed straightening angle of the current stack's crop layer, which the idle section
    /// shows and a draft opened on it starts at; 0 without one.
    fn committed_crop_angle(&self) -> f64 {
        let (Some(frame), Some(state)) = (crop_frame(&self.modules), &self.state) else {
            return 0.0;
        };
        let Some(effect) = frame.effect() else {
            return 0.0;
        };
        state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == effect)
            .and_then(|layer| serde_json::from_value::<CropPayload>(layer.payload.clone()).ok())
            .map_or(0.0, |payload| payload.angle)
    }

    /// The idle section's angle box opened for typing: it starts from the committed angle, since
    /// the angle's text otherwise holds whatever the last draft left in it.
    pub(crate) fn seed_idle_angle(&mut self) {
        if self.crop().is_none() && self.crop_pending().is_none() && self.editing_angle() {
            self.crop_angle = number_text(self.committed_crop_angle());
        }
    }

    /// One draft change reached its end: the angle field follows the draft and the new state is
    /// logged. Pointer moves inside a gesture do not come through here.
    pub(crate) fn crop_changed(&mut self, event: &'static str) {
        let Some((angle, summary)) = self
            .crop()
            .map(|draft| (number_text(draft.stage.angle), draft.summary()))
        else {
            return;
        };
        self.crop_angle = angle;
        self.event(event, summary);
    }

    /// The crop angle's box is open for typing.
    fn editing_angle(&self) -> bool {
        crop_frame(&self.modules).is_some_and(|frame| {
            self.editing.as_ref().is_some_and(|(action, parameter)| {
                action == frame.action && parameter == frame.angle
            })
        })
    }

    /// Drop the draft and the extra texture it displayed. Ending a draft by any route — Apply,
    /// Cancel or a scripted `draft.cancel`/`draft.apply` — returns the session to pointer.
    pub(crate) fn end_draft(&mut self) {
        self.set_crop(None);
        self.set_crop_pending(None);
        self.presenter.end_stage();
        self.draft_generation = None;
        self.crop_applying = None;
        self.crop_guide = false;
        if self.editing_angle() {
            self.editing = None;
        }
        self.mode_sync = Some(POINTER_MODE.into());
    }

    /// The `custom` preset's two extents as typed, or `None` when either is not a positive number.
    fn custom_ratio(&self) -> Option<(f64, f64)> {
        let width: f64 = self.crop_custom.0.trim().parse().ok()?;
        let height: f64 = self.crop_custom.1.trim().parse().ok()?;
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some((width, height))
    }

    /// Commit the draft: the one path Apply, Enter and a scripted apply all take. The error is the
    /// reason nothing was sent, so a caller can report it or record it.
    pub(crate) fn crop_apply(&mut self) -> Result<Task<Message>, String> {
        if self.crop().is_none() {
            return Err("No crop draft is open".into());
        }
        if self.busy {
            return Err("A request is already in flight".into());
        }
        if !self.session.preview.can_edit() {
            return Err("Return to the current state before applying a crop".into());
        }
        match self.crop_request() {
            None => Err("Discard or reapply the conflicted crop draft first".into()),
            Some(Err(message)) => Err(message),
            Some(Ok((method, request, request_id))) => {
                self.crop_applying = Some(request_id);
                Ok(self.command(method, request))
            }
        }
    }

    /// The request Apply would send, built from the canvas descriptor's own parameter names, or the
    /// validation message that stops it. `None` means there is nothing to apply.
    pub(crate) fn crop_request(&self) -> Option<Result<(String, Value, String), String>> {
        let frame = crop_frame(&self.modules)?;
        let draft = self.crop()?;
        let state = self.state.as_ref()?;
        if draft.conflicted {
            return None;
        }
        if let Err(error) = draft.output() {
            return Some(Err(error.to_string()));
        }
        let mutation = mutation(draft.base_revision);
        let request_id = mutation.request_id.clone();
        let mut request = json!({"asset_id":state.asset.id,"mutation":mutation});
        request
            .as_object_mut()
            .expect("the envelope is an object")
            .extend(frame.params(&draft.payload()));
        Some(Ok((format!("edit.{}", frame.action), request, request_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        message::{ControlMessage, Message, PointerMessage, SyncMessage, ViewMessage},
        tasks::{ACTOR, SyncResult},
        testing::{CROP_ASPECTS, crop_layer, entry, finish, opened, refresh_for},
    };
    use crate::crop_draft::{ANGLE_STEP, Corner, Handle};
    use lightwell_core::{ClientSession, HistorySelection};

    fn stage() -> CropStage {
        CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        }
    }

    #[test]
    fn a_draft_opens_on_the_existing_crop_layer_and_its_truncated_input_stage() {
        let payload = CropPayload {
            angle: 7.0,
            x: 0.2,
            y: 0.25,
            width: 0.4,
            height: 0.3,
        };
        let earlier = lightwell_core::Layer::pixel(0, 0, [1, 2, 3]);
        let crop = crop_layer(payload);
        let (mut editor, catalog, _, _) = opened(
            vec![earlier, crop.clone(), crop_layer(CropPayload::NEUTRAL)],
            4,
        );
        // Only the first crop layer is the one being edited.
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let pending = editor.crop_pending().cloned().expect("a pending draft");
        assert_eq!(pending.layer, Some(crop.id.clone()));
        assert_eq!(pending.layer_index, 1, "the preview truncates to one layer");
        assert_eq!(pending.payload, Some(payload));
        assert_eq!(pending.base_revision, 4);
        assert!(!pending.reapply);
        assert!(
            editor.crop().is_none(),
            "the draft waits for its input stage"
        );

        editor.open_draft(stage());
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.layer, Some(crop.id));
        assert_eq!(draft.layer_index, 1);
        assert_eq!(draft.base_revision, 4);
        assert_eq!(draft.stage.angle, 7.0);
        assert_eq!(editor.crop_angle, "7");
        // Reopening shows exactly the rectangle the payload committed.
        let reopened = CropStage {
            angle: 7.0,
            ..stage()
        };
        assert_eq!(
            draft.output().expect("a valid draft"),
            payload.output_rect(&reopened).expect("a valid payload")
        );
        assert_eq!(editor.snapshot()["crop"]["layer_index"], json!(1));
        finish(editor, catalog);
    }

    /// The angle's rail is a continuous gesture on the draft: every move follows the rail on its
    /// step and refits the rectangle, only the release logs a draft change, and nothing commits.
    #[test]
    fn a_drag_on_the_angle_rail_drafts_the_angle_and_logs_once_on_release() {
        let (mut editor, catalog, _, _) =
            opened(vec![lightwell_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let log = crate::app::testing::attach_log(&mut editor);
        // 2.4° is 47.4 of the rail's 90°; a fraction a hair off it snaps to the rail's 0.05° step.
        for fraction in [0.6, 0.5 + 2.4 / 90.0 + 1e-4] {
            let _ = editor.update(Message::Crop(CropMessage::AngleRail(fraction)));
        }
        let draft = editor.crop().expect("the draft stays open");
        assert_eq!(draft.stage.angle, 2.4);
        assert_eq!(editor.crop_angle, "2.4");
        let rect = draft.rect;
        let _ = editor.update(Message::Crop(CropMessage::AngleRailReleased));
        assert_eq!(editor.crop().expect("still drafting").rect, rect);
        assert_eq!(editor.state.as_ref().expect("a state").revision, 2);
        let changes: Vec<Value> = crate::app::testing::logged(&mut editor, &log)
            .into_iter()
            .filter(|record| record["event"] == "crop_draft_changed")
            .collect();
        assert_eq!(changes.len(), 1, "one draft change, at the release");
        assert_eq!(changes[0]["detail"]["angle"], json!(2.4));
        finish(editor, catalog);
    }

    /// The angle's box shows the angle with its unit until it is pressed; a submitted number closes
    /// it again, and text that is not a number keeps it open for correcting.
    #[test]
    fn the_angle_box_opens_for_typing_and_closes_on_a_submitted_number() {
        let (mut editor, catalog, _, _) =
            opened(vec![lightwell_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let frame = crop_frame(&editor.modules).expect("a crop frame");
        let key = (frame.action.to_owned(), frame.angle.to_owned());
        assert!(!editor.editing_angle());
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        }));
        assert!(editor.editing_angle());
        let _ = editor.update(Message::Crop(CropMessage::AngleText("two".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(editor.editing_angle(), "invalid text stays open");
        let _ = editor.update(Message::Crop(CropMessage::AngleText("3.5".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(!editor.editing_angle());
        assert_eq!(editor.crop().expect("a draft").stage.angle, 3.5);
        finish(editor, catalog);
    }

    #[test]
    fn a_stack_without_a_crop_layer_drafts_a_neutral_crop_at_the_end() {
        let (mut editor, catalog, _, _) =
            opened(vec![lightwell_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let pending = editor.crop_pending().cloned().expect("a pending draft");
        assert_eq!(pending.layer, None);
        assert_eq!(pending.layer_index, 1, "the whole stack is the input stage");
        editor.open_draft(stage());
        let draft = editor.crop().expect("an opened draft");
        assert!(draft.layer.is_none());
        assert_eq!(draft.payload(), CropPayload::NEUTRAL);
        finish(editor, catalog);
    }

    #[test]
    fn an_unreadable_crop_payload_refuses_the_draft_and_keeps_the_layer() {
        let mut broken = crop_layer(CropPayload::NEUTRAL);
        broken.payload = json!({"angle":"sideways"});
        let (mut editor, catalog, _, _) = opened(vec![broken], 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert_eq!(
            editor.crop_pending().map(|pending| pending.payload),
            Some(None)
        );
        editor.open_draft(stage());
        assert!(
            editor.crop().is_none(),
            "no neutral crop replaced the layer"
        );
        assert!(
            editor.status.contains("cannot be read"),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    #[test]
    fn every_draft_change_is_reachable_as_a_message() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 3);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let start = editor.crop().expect("a draft").rect;

        // A pointer gesture: begin, drag, end. Nothing changes until the drag arrives.
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Begin {
            handle: Handle::Corner(Corner::TopLeft),
            x: 0.0,
            y: 0.0,
        })));
        assert_eq!(editor.crop().expect("a draft").rect, start);
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Drag {
            x: 80.0,
            y: 60.0,
            option: false,
        })));
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
        let dragged = editor.crop().expect("a draft").rect;
        assert_eq!((dragged.x, dragged.y), (80.0, 60.0));

        // The angle field and its nudges.
        let _ = editor.update(Message::Crop(CropMessage::AngleText("11.5".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert_eq!(editor.crop().expect("a draft").stage.angle, 11.5);
        let _ = editor.update(Message::Crop(CropMessage::NudgeAngle(-ANGLE_STEP)));
        assert_eq!(editor.crop().expect("a draft").stage.angle, 11.0);
        assert_eq!(editor.crop_angle, "11");
        let _ = editor.update(Message::Crop(CropMessage::AngleText("sideways".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert_eq!(editor.crop().expect("a draft").stage.angle, 11.0);
        assert!(editor.status.contains("Angle must be"), "{}", editor.status);
        let _ = editor.update(Message::Crop(CropMessage::AngleText("0".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));

        // Ratio presets, swap, lock and the custom extents, all by index into the declared list.
        let index = |option: &str| {
            CROP_ASPECTS
                .iter()
                .position(|candidate| *candidate == option)
                .expect("a declared option")
        };
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("16:9"))));
        let draft = editor.crop().expect("a draft");
        assert_eq!(draft.preset, "16:9");
        assert_eq!(draft.aspect.ratio(), Some(16.0 / 9.0));
        let _ = editor.update(Message::Crop(CropMessage::Swap));
        assert_eq!(
            editor.crop().expect("a draft").aspect.ratio(),
            Some(9.0 / 16.0)
        );
        let _ = editor.update(Message::Crop(CropMessage::Lock));
        assert_eq!(editor.crop().expect("a draft").aspect.ratio(), None);
        let _ = editor.update(Message::Crop(CropMessage::CustomWidth("5".into())));
        let _ = editor.update(Message::Crop(CropMessage::CustomHeight("4".into())));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("custom"))));
        assert_eq!(editor.crop().expect("a draft").aspect.ratio(), Some(1.25));
        let _ = editor.update(Message::Crop(CropMessage::CustomHeight("none".into())));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("1:1"))));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("custom"))));
        assert_eq!(
            editor.crop().expect("a draft").aspect.ratio(),
            Some(1.0),
            "an unreadable custom extent changes nothing"
        );

        // The modifier and guide state the canvas reads is app state, reachable by message.
        for (message, read) in [
            (CropMessage::Option(true), true),
            (CropMessage::Option(false), false),
        ] {
            let _ = editor.update(Message::Crop(message));
            assert_eq!(editor.crop_option, read);
        }
        let _ = editor.update(Message::Crop(CropMessage::Space(true)));
        assert!(editor.crop_space);
        let _ = editor.update(Message::Crop(CropMessage::Guide(true)));
        assert!(editor.crop_guide);

        let _ = editor.update(Message::Crop(CropMessage::Cancel));
        assert!(editor.crop().is_none());
        assert!(editor.presenter.stage().is_none());
        assert!(!editor.crop_guide, "cancelling leaves no guide mode on");
        let summary = editor.snapshot()["crop"].clone();
        assert_eq!(
            (&summary["drafting"], &summary["pending"]),
            (&json!(false), &json!(false))
        );
        // The idle section is back, reading the stack the draft never committed to.
        assert_eq!(
            summary["section"],
            json!({"drafting":false,"pending":false,"enabled":true,"chosen":"Free","locked":false,"can_swap":false,"angle":"0","rail":0.0,"guide":false})
        );
        finish(editor, catalog);
    }

    #[test]
    fn apply_builds_the_declared_request_against_the_drafts_own_revision() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Begin {
            handle: Handle::Corner(Corner::TopLeft),
            x: 0.0,
            y: 0.0,
        })));
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Drag {
            x: 48.0,
            y: 32.0,
            option: false,
        })));
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
        let payload = editor.crop().expect("a draft").payload();
        let (method, request, request_id) = editor
            .crop_request()
            .expect("a request")
            .expect("a valid draft");
        assert_eq!(method, "edit.crop");
        assert_eq!(request["asset_id"], json!(asset));
        assert_eq!(request["mutation"]["expected_revision"], json!(6));
        assert_eq!(request["mutation"]["actor"], json!(ACTOR));
        assert_eq!(request["mutation"]["request_id"], json!(request_id));
        assert_eq!(request["angle"], json!(payload.angle));
        assert_eq!(request["x"], json!(payload.x));
        assert_eq!(request["width"], json!(payload.width));
        assert!(request.get("aspect").is_none(), "only the declared five");
        finish(editor, catalog);
    }

    #[test]
    fn an_external_commit_marks_the_draft_conflicted_and_reapply_rebases_it() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let _ = editor.update(Message::Crop(CropMessage::AngleText("6".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        let composed = editor.crop().expect("a draft").rect;

        // Somebody else committed: the draft survives and says so, and Apply is refused.
        let newer = entry(&asset, 9, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
            refresh,
        )))));
        let draft = editor.crop().expect("the draft is kept");
        assert!(draft.conflicted);
        assert_eq!(draft.rect, composed, "the composition is untouched");
        assert!(editor.crop_request().is_none(), "Apply is refused");
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["crop"]["conflicted"], json!(true));
        assert_eq!(
            snapshot["notices"],
            json!(["Changed elsewhere"]),
            "the captured frame records the chrome it drew"
        );
        assert_eq!(snapshot["render_error"], json!(null));
        assert_eq!(snapshot["compare"], json!(false));

        // Reapply re-reads the stack and rebases onto the new revision and input stage.
        editor.busy = false;
        let _ = editor.update(Message::Crop(CropMessage::Reapply));
        let pending = editor.crop_pending().cloned().expect("a pending rebase");
        assert!(pending.reapply);
        assert_eq!(pending.base_revision, 9);
        editor.open_draft(stage());
        let draft = editor.crop().expect("the rebased draft");
        assert!(!draft.conflicted);
        assert_eq!(draft.base_revision, 9);
        assert_eq!(draft.stage.angle, 6.0, "the angle survives a rebase");
        assert!(editor.crop_request().is_some(), "Apply is possible again");
        finish(editor, catalog);
    }

    /// A crop's input stage is everything ahead of it, the transforms included: a draft on a stack
    /// turned after it was cropped truncates after the orientation layer, which the host keeps
    /// ahead of the crop, and remembers the turn it opened behind.
    #[test]
    fn a_draft_opens_behind_every_transform_ahead_of_its_crop() {
        let turn = lightwell_core::Layer::orientation(
            Orientation::NEUTRAL.then(lightwell_core::Transform::RotateRight),
        );
        let crop = crop_layer(CropPayload {
            angle: 0.0,
            x: 0.25,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        });
        let (mut editor, catalog, _, _) = opened(
            vec![
                lightwell_core::Layer::pixel(0, 0, [1, 2, 3]),
                turn,
                crop.clone(),
            ],
            3,
        );
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let pending = editor.crop_pending().cloned().expect("a pending draft");
        assert_eq!(pending.layer, Some(crop.id));
        assert_eq!(
            pending.layer_index, 2,
            "the pixel edit and the turn are shown"
        );
        assert_eq!(
            pending.ahead,
            Orientation::NEUTRAL.then(lightwell_core::Transform::RotateRight)
        );
        editor.open_draft(CropStage {
            width: 320,
            height: 480,
            angle: 0.0,
        });
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.ahead, pending.ahead);
        assert_eq!((draft.stage.width, draft.stage.height), (320, 480));
        finish(editor, catalog);
    }

    /// Without a crop layer the draft shows the stage the host would give a new one, which is
    /// before any finish layer: a post-crop effect acts on the crop's output, not on its input.
    #[test]
    fn a_first_draft_stops_before_a_finish_layer() {
        let finish_layer = lightwell_core::Layer {
            id: LayerId::new(),
            effect_id: lightwell_core::VIGNETTE_EFFECT.into(),
            effect_format: 1,
            payload: json!({"amount": -40}),
            artifacts: Vec::new(),
            mask: None,
        };
        let (mut editor, catalog, _, _) = opened(
            vec![lightwell_core::Layer::pixel(0, 0, [1, 2, 3]), finish_layer],
            2,
        );
        let finishing = lightwell_core::ModuleDescriptor {
            id: "lightwell.vignette".into(),
            effects: vec![lightwell_core::EffectDescriptor {
                id: lightwell_core::VIGNETTE_EFFECT.into(),
                format: 1,
                stage: lightwell_core::EffectStage::Finish,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
            }],
            ..lightwell_core::ModuleDescriptor::default()
        };
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
            crate::app::testing::crop_descriptor(),
            finishing,
        ]))));
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let pending = editor.crop_pending().cloned().expect("a pending draft");
        assert_eq!(pending.layer, None);
        assert_eq!(
            pending.layer_index, 1,
            "the vignette is not part of the input"
        );
        finish(editor, catalog);
    }

    /// A turn committed while drafting, here as another client would commit it, goes ahead of the
    /// crop and turns its input stage. Reapply carries the draft through the turn, so the frame
    /// keeps selecting what it did instead of landing on whatever the same box numbers now show.
    #[test]
    fn reapply_after_a_turn_carries_the_draft_with_the_photograph() {
        let crop = crop_layer(CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        });
        let (mut editor, catalog, asset, _) = opened(vec![crop.clone()], 4);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let before = editor.crop().expect("a draft").rect;
        assert_eq!(
            (before.x, before.y, before.width, before.height),
            (0.0, 0.0, 240.0, 160.0)
        );

        let right = Orientation::NEUTRAL.then(lightwell_core::Transform::RotateRight);
        let mut newer = entry(&asset, 5, None);
        for layer in [
            lightwell_core::Layer::orientation(right),
            lightwell_core::Layer {
                payload: serde_json::to_value(
                    serde_json::from_value::<CropPayload>(crop.payload.clone())
                        .unwrap()
                        .carried((480, 320), right)
                        .unwrap(),
                )
                .unwrap(),
                ..crop.clone()
            },
        ] {
            newer.snapshot = newer.snapshot.append(layer).expect("a valid stack");
        }
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
            refresh,
        )))));
        assert!(editor.crop().expect("the draft is kept").conflicted);

        editor.busy = false;
        let _ = editor.update(Message::Crop(CropMessage::Reapply));
        let pending = editor.crop_pending().cloned().expect("a pending rebase");
        assert_eq!((pending.layer_index, pending.ahead), (1, right));
        editor.open_draft(CropStage {
            width: 320,
            height: 480,
            angle: 0.0,
        });
        let draft = editor.crop().expect("the rebased draft");
        assert!(!draft.conflicted);
        // The top-left quarter of the photograph, turned clockwise, is its top-right quarter.
        assert_eq!(
            (
                draft.rect.x,
                draft.rect.y,
                draft.rect.width,
                draft.rect.height
            ),
            (160.0, 0.0, 160.0, 240.0)
        );
        finish(editor, catalog);
    }

    #[test]
    fn the_drafts_own_apply_ends_it_and_a_failed_apply_keeps_it() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        // A stale revision comes back as a conflict: the draft is kept and marked.
        editor.crop_applying = Some("desktop-1".into());
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Err(
            "conflict: stale revision".into(),
        ))));
        assert!(editor.crop().expect("the draft is kept").conflicted);
        assert!(editor.crop_applying.is_none());

        // The draft's own successful Apply ends it and drops the extra texture.
        editor.crop_mut().expect("a draft").conflicted = false;
        editor.crop_applying = Some("desktop-2".into());
        let newer = entry(&asset, 5, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
        assert!(editor.crop().is_none());
        assert!(editor.presenter.stage().is_none());
        assert!(editor.status.contains("Crop applied"), "{}", editor.status);
        finish(editor, catalog);
    }

    #[test]
    fn starting_or_ending_a_draft_by_any_route_asks_the_session_to_follow_it() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
        let crop_id = crop_frame(&editor.modules)
            .expect("a declared crop frame")
            .module
            .id
            .to_owned();

        // The section's own "Crop & straighten" button, `R` and a scripted `draft.start` all send
        // this message directly, never through `ViewMessage::SetMode`; starting still asks the session
        // to enter the crop mode.
        let _ = editor.dispatch(Message::Crop(CropMessage::Start));
        assert_eq!(
            editor.mode_sync.as_deref(),
            Some(crop_id.as_str()),
            "starting a draft by any route queues the session's own mode change"
        );
        // The public entry point folds that into the returned task and consumes the flag.
        editor.session.workspace.mode = crop_id.clone();
        let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
        assert_eq!(editor.mode_sync, None, "the wrapper always consumes it");

        editor.open_draft(stage());
        assert!(editor.workspace.canvas.modes[0].id == POINTER_MODE);
        assert!(
            !editor.workspace.canvas.modes[0].selected,
            "pointer is not selected while the session reports the crop mode"
        );

        // Ending it, by Cancel here (Apply and a scripted cancel go through the same `end_draft`),
        // returns the session to pointer.
        let _ = editor.dispatch(Message::Crop(CropMessage::Cancel));
        assert_eq!(
            editor.mode_sync.as_deref(),
            Some(POINTER_MODE),
            "ending a draft by any route queues the session's return to pointer"
        );
        finish(editor, catalog);
    }

    /// The mode strip's Crop entry enters the crop mode only by starting the draft: a start that is
    /// refused sends no `workspace.set`, so the session never reports a crop mode the desktop is not
    /// in. A start that goes ahead asks for the mode once, through the update's own catch-up.
    #[test]
    fn a_refused_crop_start_sends_no_workspace_change() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
        let crop_id = crop_frame(&editor.modules)
            .expect("a declared crop frame")
            .module
            .id
            .to_owned();
        let mode = editor.session.workspace.mode.clone();
        let refused = |editor: &mut Editor, case: &str| {
            let task = editor.dispatch(Message::View(ViewMessage::SetMode(crop_id.clone())));
            assert_eq!(task.units(), 0, "{case}: nothing is sent");
            assert_eq!(editor.mode_sync, None, "{case}: no mode change is queued");
            assert!(editor.crop_pending().is_none() && editor.crop().is_none());
            assert_eq!(editor.session.workspace.mode, mode, "{case}");
        };

        // A discarded draft still closing holds the slot: the start is refused with its reason.
        let (draft, _) = crate::app::draft::CoreDraft::open(editor.next_gesture(), 1, None);
        editor.gesture = Some(crate::app::gesture::Gesture::Closing {
            draft,
            reseed: false,
        });
        refused(&mut editor, "a draft closing");
        assert_eq!(
            editor.status,
            "Wait for the discarded draft to close before cropping"
        );
        editor.gesture = None;

        // A historical preview cannot be edited.
        let current = editor.session.preview.selection.clone();
        editor.session.preview.selection = HistorySelection::Entry(entry_id);
        refused(&mut editor, "a historical preview");
        editor.session.preview.selection = current;

        // Nothing refuses it: the draft starts and asks the session for the mode once.
        let task = editor.dispatch(Message::View(ViewMessage::SetMode(crop_id.clone())));
        assert_eq!(task.units(), 1, "the truncated preview's own task");
        assert!(editor.crop_pending().is_some());
        assert_eq!(editor.mode_sync.as_deref(), Some(crop_id.as_str()));
        finish(editor, catalog);
    }

    #[test]
    fn a_history_preview_pauses_the_draft_without_discarding_it() {
        let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let composed = editor.crop().expect("a draft").rect;
        let mut session = ClientSession {
            revision: 3,
            ..ClientSession::default()
        };
        session.preview.selection = HistorySelection::Entry(entry_id);
        let _ = editor.update(Message::View(ViewMessage::SessionUpdated(Ok(session))));
        assert!(!editor.session.preview.can_edit());
        assert!(
            editor.crop().is_some(),
            "selecting a historical state keeps the draft"
        );
        assert!(!editor.drafting(), "the plain historical preview is shown");
        assert_eq!(editor.snapshot()["crop"]["paused"], json!(true));
        // Nothing can be applied or started while previewing history.
        let _ = editor.update(Message::Crop(CropMessage::Apply));
        assert!(editor.crop().is_some());
        assert!(editor.crop_applying.is_none());
        assert_eq!(editor.crop().expect("a draft").rect, composed);
        let _ = std::hint::black_box(&asset);
        finish(editor, catalog);
    }

    /// The committed 16:9 crop the idle tests start from, fitted on [`stage`] by the core's own
    /// geometry exactly as `crop-fit` or the 16:9 chip would fit it.
    fn committed_wide() -> CropPayload {
        let stage = stage();
        let whole = lightwell_core::BoxRect {
            x: 0.0,
            y: 0.0,
            width: 480.0,
            height: 320.0,
        };
        stage
            .fit_about_center(lightwell_core::largest_with_ratio_inside(whole, 16.0 / 9.0))
            .normalized(&stage)
    }

    fn option(option: &str) -> usize {
        CROP_ASPECTS
            .iter()
            .position(|candidate| *candidate == option)
            .expect("a declared option")
    }

    fn events(records: &[Value], name: &str) -> Vec<Value> {
        records
            .iter()
            .filter(|record| record["event"] == name)
            .map(|record| record["detail"].clone())
            .collect()
    }

    /// A chip pressed in the idle section opens the draft seeded from the committed crop, as Start
    /// does, and applies itself once the draft's input stage arrives: the canvas enters crop mode
    /// with the frame at the new ratio, the log shows the seeded draft and then the one change, and
    /// nothing commits.
    #[test]
    fn an_idle_change_opens_the_draft_seeded_from_the_committed_crop_and_applies_it() {
        let crop = crop_layer(committed_wide());
        let (mut editor, catalog, _, _) = opened(vec![crop.clone()], 5);
        let log = crate::app::testing::attach_log(&mut editor);
        let _ = editor.dispatch(Message::Crop(CropMessage::Preset(option("1:1"))));
        let pending = editor
            .crop_pending()
            .cloned()
            .expect("the change opened a draft");
        assert_eq!(pending.layer, Some(crop.id.clone()));
        assert_eq!(pending.payload, Some(committed_wide()));
        assert!(
            matches!(pending.queued.as_slice(), [CropMessage::Preset(index)] if *index == option("1:1")),
            "{:?}",
            pending.queued
        );
        assert!(
            editor.crop().is_none(),
            "the change waits for the input stage"
        );
        assert_eq!(
            editor.mode_sync.as_deref(),
            Some("lightwell.crop"),
            "the canvas enters crop mode"
        );

        editor.open_draft(stage());
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.layer, Some(crop.id));
        assert_eq!(draft.preset, "1:1");
        assert_eq!(draft.aspect.ratio(), Some(1.0));
        assert_eq!(draft.rect.width, draft.rect.height);
        assert!(editor.crop_pending().is_none());
        assert_eq!(
            editor.state.as_ref().expect("a state").revision,
            5,
            "nothing commits until Apply"
        );
        let records = crate::app::testing::logged(&mut editor, &log);
        let started = events(&records, "crop_draft_started");
        assert_eq!(started.len(), 1);
        assert_eq!(
            started[0]["preset"],
            json!("16:9"),
            "the draft seeds the ratio the committed crop reads as"
        );
        let changed = events(&records, "crop_draft_changed");
        assert_eq!(changed.len(), 1, "the one idle change: {changed:?}");
        assert_eq!(changed[0]["preset"], json!("1:1"));
        finish(editor, catalog);
    }

    /// Changes made while the draft is still starting — here after the mode strip's plain Start —
    /// are queued in order and applied once it opens, none lost: a ratio, a nudge and a rail drag
    /// that is one change however far it moves. Meanwhile the section's angle shows where the
    /// queued changes lead.
    #[test]
    fn a_change_made_while_the_draft_is_starting_is_applied_once_it_opens() {
        let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert_eq!(editor.crop_angle, "0", "the committed angle while starting");
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("4:3"))));
        let _ = editor.update(Message::Crop(CropMessage::NudgeAngle(ANGLE_STEP)));
        assert_eq!(editor.crop_angle, "0.5");
        for fraction in [0.52, 0.55, 0.5 + 2.4 / 90.0 + 1e-4] {
            let _ = editor.update(Message::Crop(CropMessage::AngleRail(fraction)));
        }
        let _ = editor.update(Message::Crop(CropMessage::AngleRailReleased));
        assert_eq!(editor.crop_angle, "2.4");
        let queued = &editor.crop_pending().expect("still starting").queued;
        assert!(
            matches!(
                queued.as_slice(),
                [
                    CropMessage::Preset(_),
                    CropMessage::NudgeAngle(_),
                    CropMessage::AngleRail(_),
                    CropMessage::AngleRailReleased
                ]
            ),
            "the rail's moves are one queued change: {queued:?}"
        );
        assert!(editor.crop().is_none());

        editor.open_draft(stage());
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.preset, "4:3");
        assert_eq!(draft.stage.angle, 2.4, "the rail's last position wins");
        assert_eq!(editor.crop_angle, "2.4");
        assert_eq!(editor.state.as_ref().expect("a state").revision, 5);
        finish(editor, catalog);
    }

    /// What an idle control does that is not a change: the committed angle submitted again only
    /// closes the box, text that is not a number stays open with its reason, a rail release with no
    /// drag does nothing, and pressing the chip already chosen opens the draft without refitting
    /// the committed rectangle to it. An open slider gesture refuses the draft, and a start whose
    /// input stage fails takes its queued changes with it.
    #[test]
    fn an_idle_control_that_changes_nothing_opens_no_draft_or_leaves_the_crop_as_it_is() {
        let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
        let frame = crop_frame(&editor.modules).expect("a crop frame");
        let key = (frame.action.to_owned(), frame.angle.to_owned());
        editor.crop_angle = "31".into();
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        }));
        assert_eq!(
            editor.crop_angle, "0",
            "the idle box opens at the committed angle"
        );
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(editor.crop_pending().is_none() && !editor.editing_angle());
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0,
            parameter: key.1,
        }));
        let _ = editor.update(Message::Crop(CropMessage::AngleText("level".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(editor.crop_pending().is_none() && editor.editing_angle());
        assert!(editor.status.contains("Angle must be"), "{}", editor.status);
        let _ = editor.update(Message::Control(ControlMessage::CancelEdit));
        let _ = editor.update(Message::Crop(CropMessage::AngleRailReleased));
        let _ = editor.update(Message::Crop(CropMessage::Guide(false)));
        assert!(editor.crop_pending().is_none());

        // A slider gesture is finished deliberately, never displaced by the crop draft.
        crate::app::testing::hold_slider(&mut editor, "set-basic", "exposure");
        let _ = editor.update(Message::Crop(CropMessage::Lock));
        assert!(editor.crop_pending().is_none());
        assert!(editor.status.contains("slider draft"), "{}", editor.status);
        editor.gesture = None;

        // A start whose input stage cannot be prepared drops what it queued.
        let _ = editor.update(Message::Crop(CropMessage::Swap));
        assert_eq!(
            editor.crop_pending().map(|pending| pending.queued.len()),
            Some(1)
        );
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(Err(
            "the source is gone".into(),
        ))));
        assert!(editor.crop_pending().is_none());
        editor.open_draft(stage());
        assert!(editor.crop().is_none(), "nothing opens after the failure");

        // The chip already chosen opens the draft and leaves the committed rectangle exactly.
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("16:9"))));
        editor.open_draft(stage());
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.preset, "16:9");
        assert_eq!(draft.payload(), committed_wide());
        finish(editor, catalog);
    }
}
