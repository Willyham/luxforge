//! The crop-draft driver. Every crop change goes through `crop_update`, so the API-equivalent path
//! and the pointer path are the same code and only Apply ever commits.
use crate::{
    app::{
        Editor,
        evidence::Settle,
        fields::{decimals_of, number_text},
        message::{CropMessage, CropPointer, Message},
        tasks::{crop_preview_task, mutation},
    },
    crop_draft::{CropDraft, Modifiers as DraftModifiers},
    state::tools::crop_frame,
};
use iced::{Task, widget::operation};
use lightwell_core::{CropPayload, CropStage, LayerId, MAX_ANGLE, MIN_ANGLE, POINTER_MODE};
use lightwell_ui::geometry::{quantize, value_from_fraction};
use serde_json::{Value, json};

/// How far one nudge button moves the straightening angle, in degrees.
pub(crate) const ANGLE_STEP: f64 = 0.5;
/// How finely a drag on the angle's rail moves the angle: the fine nudge (Option with an arrow),
/// a tenth of [`ANGLE_STEP`], so the rail reaches exactly the angles the keyboard does. The crop
/// descriptor declares no step or precision for its angle, so this is the host's own.
pub(crate) const ANGLE_RAIL_STEP: f64 = ANGLE_STEP / 10.0;
/// The scrollable around the photo, so a Space drag can scroll it while drafting a crop.
pub(crate) const SURFACE_ID: &str = "lightwell.surface";

/// What a crop draft is waiting for its truncated preview to tell it: the layer it edits and the
/// payload it starts from are known from the stack, but the input stage is whatever that preview
/// renders, so the draft opens when its pixels arrive.
#[derive(Clone, Debug)]
pub(crate) struct PendingDraft {
    pub(crate) layer: Option<LayerId>,
    pub(crate) layer_index: usize,
    pub(crate) payload: Option<CropPayload>,
    pub(crate) base_revision: u64,
    /// A reapply rebases the existing draft instead of opening a new one.
    pub(crate) reapply: bool,
}

impl Editor {
    /// Every crop draft change goes through here, so the API-equivalent path and the pointer path
    /// are the same code.
    pub(crate) fn crop_update(&mut self, message: CropMessage) -> Task<Message> {
        match message {
            CropMessage::Option(option) => self.crop_option = option,
            CropMessage::Space(space) => self.crop_space = space,
            CropMessage::Guide(guide) => self.crop_guide = guide && self.crop.is_some(),
            CropMessage::Start => return self.crop_start(false),
            CropMessage::Reapply => return self.crop_start(true),
            CropMessage::PreviewReady(result) => match result {
                Ok(job) => {
                    self.draft_generation = Some(self.request_preview(*job));
                    self.status = "Rendering the crop's input stage…".into();
                }
                Err(error) => {
                    if error == "superseded preview" {
                        return Task::none();
                    }
                    self.crop_pending = None;
                    self.status = error;
                    self.settle_step(Settle::Draft);
                }
            },
            CropMessage::Pointer(pointer) => {
                let Some(draft) = &mut self.crop else {
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
                let Some(draft) = &mut self.crop else {
                    return Task::none();
                };
                let angle = value_from_fraction(MIN_ANGLE, MAX_ANGLE, ANGLE_RAIL_STEP, fraction);
                draft.set_angle(quantize(
                    angle,
                    MIN_ANGLE,
                    MAX_ANGLE,
                    ANGLE_RAIL_STEP,
                    decimals_of(ANGLE_RAIL_STEP),
                ));
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
                if let Some(draft) = &mut self.crop {
                    draft.set_angle(value);
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::NudgeAngle(step) => {
                if let Some(draft) = &mut self.crop {
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
                if let Some(draft) = &mut self.crop {
                    draft.set_preset(&preset, custom);
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::CustomWidth(text) => self.crop_custom.0 = text,
            CropMessage::CustomHeight(text) => self.crop_custom.1 = text,
            CropMessage::Swap => {
                if let Some(draft) = &mut self.crop {
                    draft.swap();
                }
                self.crop_changed("crop_draft_changed");
            }
            CropMessage::Lock => {
                if let Some(draft) = &mut self.crop {
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
                let Some(summary) = self.crop.as_ref().map(CropDraft::summary) else {
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
        if self.busy || !self.session.preview.can_edit() || reapply != self.crop.is_some() {
            return Task::none();
        }
        let Some((module_id, effect)) = crop_frame(&self.modules)
            .and_then(|frame| Some((frame.module.id.clone(), frame.effect()?.to_owned())))
            .filter(|_| self.state.is_some())
        else {
            return Task::none();
        };
        let state = self.state.as_ref().expect("filtered above");
        let layers = &state.current_entry.snapshot.recipe.layers;
        let found = layers.iter().position(|layer| layer.effect_id == effect);
        let pending = PendingDraft {
            layer: found.map(|index| layers[index].id.clone()),
            layer_index: found.unwrap_or(layers.len()),
            payload: found
                .and_then(|index| serde_json::from_value(layers[index].payload.clone()).ok()),
            base_revision: state.revision,
            reapply,
        };
        let asset = state.asset.id.clone();
        let layer_count = pending.layer_index;
        self.crop_pending = Some(pending);
        self.status = "Preparing the crop's input stage…".into();
        // Starting a draft by any route — the section's own button, `R`, the mode strip or a
        // scripted `draft.start` — asks the session to enter this module's mode, so the strip shows
        // Crop selected for the whole life of the draft. A reapply keeps the mode already set.
        if !reapply {
            self.mode_sync = Some(module_id);
        }
        crop_preview_task(self.owner.clone(), self.client, asset, layer_count)
    }

    /// The truncated preview arrived, so the crop layer's input stage is known: open or rebase the
    /// draft against it.
    pub(crate) fn open_draft(&mut self, input: CropStage) {
        let Some(pending) = self.crop_pending.take() else {
            return;
        };
        if pending.reapply {
            match &mut self.crop {
                Some(draft) => draft.rebase(
                    input,
                    pending.base_revision,
                    pending.layer,
                    pending.layer_index,
                ),
                None => return self.settle_step(Settle::Draft),
            }
        } else {
            self.crop = match (pending.layer, pending.payload) {
                (Some(layer), Some(payload)) => Some(CropDraft::from_layer(
                    input,
                    payload,
                    layer,
                    pending.layer_index,
                    pending.base_revision,
                )),
                // An unreadable payload is never silently replaced by a neutral crop: the stored
                // layer stays exactly as it is and the draft does not open.
                (Some(_), None) => {
                    self.draft_photo = None;
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
        }
        self.crop_changed(if pending.reapply {
            "crop_draft_changed"
        } else {
            "crop_draft_started"
        });
        self.status = format!(
            "Crop draft on the layer's {} × {} input stage",
            input.width, input.height
        );
        self.settle_step(Settle::Draft);
    }

    /// One draft change reached its end: the angle field follows the draft and the new state is
    /// logged. Pointer moves inside a gesture do not come through here.
    pub(crate) fn crop_changed(&mut self, event: &'static str) {
        let Some((angle, summary)) = self
            .crop
            .as_ref()
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
        self.crop = None;
        self.crop_pending = None;
        self.draft_photo = None;
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
        if self.crop.is_none() {
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
        let draft = self.crop.as_ref()?;
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
        message::Message,
        tasks::{ACTOR, SyncResult},
        testing::{CROP_ASPECTS, crop_layer, entry, finish, opened, refresh_for},
    };
    use crate::crop_draft::{Corner, Handle};
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
        let pending = editor.crop_pending.clone().expect("a pending draft");
        assert_eq!(pending.layer, Some(crop.id.clone()));
        assert_eq!(pending.layer_index, 1, "the preview truncates to one layer");
        assert_eq!(pending.payload, Some(payload));
        assert_eq!(pending.base_revision, 4);
        assert!(!pending.reapply);
        assert!(editor.crop.is_none(), "the draft waits for its input stage");

        editor.open_draft(stage());
        let draft = editor.crop.as_ref().expect("an opened draft");
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
        let draft = editor.crop.as_ref().expect("the draft stays open");
        assert_eq!(draft.stage.angle, 2.4);
        assert_eq!(editor.crop_angle, "2.4");
        let rect = draft.rect;
        let _ = editor.update(Message::Crop(CropMessage::AngleRailReleased));
        assert_eq!(editor.crop.as_ref().expect("still drafting").rect, rect);
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
        let _ = editor.update(Message::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        });
        assert!(editor.editing_angle());
        let _ = editor.update(Message::Crop(CropMessage::AngleText("two".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(editor.editing_angle(), "invalid text stays open");
        let _ = editor.update(Message::Crop(CropMessage::AngleText("3.5".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert!(!editor.editing_angle());
        assert_eq!(editor.crop.as_ref().expect("a draft").stage.angle, 3.5);
        finish(editor, catalog);
    }

    #[test]
    fn a_stack_without_a_crop_layer_drafts_a_neutral_crop_at_the_end() {
        let (mut editor, catalog, _, _) =
            opened(vec![lightwell_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let pending = editor.crop_pending.clone().expect("a pending draft");
        assert_eq!(pending.layer, None);
        assert_eq!(pending.layer_index, 1, "the whole stack is the input stage");
        editor.open_draft(stage());
        let draft = editor.crop.as_ref().expect("an opened draft");
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
            editor.crop_pending.as_ref().map(|pending| pending.payload),
            Some(None)
        );
        editor.open_draft(stage());
        assert!(editor.crop.is_none(), "no neutral crop replaced the layer");
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
        let start = editor.crop.as_ref().expect("a draft").rect;

        // A pointer gesture: begin, drag, end. Nothing changes until the drag arrives.
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Begin {
            handle: Handle::Corner(Corner::TopLeft),
            x: 0.0,
            y: 0.0,
        })));
        assert_eq!(editor.crop.as_ref().expect("a draft").rect, start);
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Drag {
            x: 80.0,
            y: 60.0,
            option: false,
        })));
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
        let dragged = editor.crop.as_ref().expect("a draft").rect;
        assert_eq!((dragged.x, dragged.y), (80.0, 60.0));

        // The angle field and its nudges.
        let _ = editor.update(Message::Crop(CropMessage::AngleText("11.5".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert_eq!(editor.crop.as_ref().expect("a draft").stage.angle, 11.5);
        let _ = editor.update(Message::Crop(CropMessage::NudgeAngle(-ANGLE_STEP)));
        assert_eq!(editor.crop.as_ref().expect("a draft").stage.angle, 11.0);
        assert_eq!(editor.crop_angle, "11");
        let _ = editor.update(Message::Crop(CropMessage::AngleText("sideways".into())));
        let _ = editor.update(Message::Crop(CropMessage::SubmitAngle));
        assert_eq!(editor.crop.as_ref().expect("a draft").stage.angle, 11.0);
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
        let draft = editor.crop.as_ref().expect("a draft");
        assert_eq!(draft.preset, "16:9");
        assert_eq!(draft.aspect.ratio(), Some(16.0 / 9.0));
        let _ = editor.update(Message::Crop(CropMessage::Swap));
        assert_eq!(
            editor.crop.as_ref().expect("a draft").aspect.ratio(),
            Some(9.0 / 16.0)
        );
        let _ = editor.update(Message::Crop(CropMessage::Lock));
        assert_eq!(editor.crop.as_ref().expect("a draft").aspect.ratio(), None);
        let _ = editor.update(Message::Crop(CropMessage::CustomWidth("5".into())));
        let _ = editor.update(Message::Crop(CropMessage::CustomHeight("4".into())));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("custom"))));
        assert_eq!(
            editor.crop.as_ref().expect("a draft").aspect.ratio(),
            Some(1.25)
        );
        let _ = editor.update(Message::Crop(CropMessage::CustomHeight("none".into())));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("1:1"))));
        let _ = editor.update(Message::Crop(CropMessage::Preset(index("custom"))));
        assert_eq!(
            editor.crop.as_ref().expect("a draft").aspect.ratio(),
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
        assert!(editor.crop.is_none());
        assert!(editor.draft_photo.is_none());
        assert!(!editor.crop_guide, "cancelling leaves no guide mode on");
        assert_eq!(
            editor.snapshot()["crop"],
            json!({"drafting":false,"pending":false})
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
        let payload = editor.crop.as_ref().expect("a draft").payload();
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
        let composed = editor.crop.as_ref().expect("a draft").rect;

        // Somebody else committed: the draft survives and says so, and Apply is refused.
        let newer = entry(&asset, 9, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Synced(Ok(SyncResult::Changed(Box::new(refresh)))));
        let draft = editor.crop.as_ref().expect("the draft is kept");
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
        let pending = editor.crop_pending.clone().expect("a pending rebase");
        assert!(pending.reapply);
        assert_eq!(pending.base_revision, 9);
        editor.open_draft(stage());
        let draft = editor.crop.as_ref().expect("the rebased draft");
        assert!(!draft.conflicted);
        assert_eq!(draft.base_revision, 9);
        assert_eq!(draft.stage.angle, 6.0, "the angle survives a rebase");
        assert!(editor.crop_request().is_some(), "Apply is possible again");
        finish(editor, catalog);
    }

    #[test]
    fn the_drafts_own_apply_ends_it_and_a_failed_apply_keeps_it() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        // A stale revision comes back as a conflict: the draft is kept and marked.
        editor.crop_applying = Some("desktop-1".into());
        let _ = editor.update(Message::Refreshed(Err("conflict: stale revision".into())));
        assert!(editor.crop.as_ref().expect("the draft is kept").conflicted);
        assert!(editor.crop_applying.is_none());

        // The draft's own successful Apply ends it and drops the extra texture.
        editor.crop.as_mut().expect("a draft").conflicted = false;
        editor.crop_applying = Some("desktop-2".into());
        let newer = entry(&asset, 5, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
        assert!(editor.crop.is_none());
        assert!(editor.draft_photo.is_none());
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
        // this message directly, never through `Message::SetMode`; starting still asks the session
        // to enter the crop mode.
        let _ = editor.dispatch(Message::Crop(CropMessage::Start));
        assert_eq!(
            editor.mode_sync.as_deref(),
            Some(crop_id.as_str()),
            "starting a draft by any route queues the session's own mode change"
        );
        // The public entry point folds that into the returned task and consumes the flag.
        editor.session.workspace.mode = crop_id.clone();
        let _ = editor.update(Message::PointerMoved(None));
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

    #[test]
    fn a_history_preview_pauses_the_draft_without_discarding_it() {
        let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(stage());
        let composed = editor.crop.as_ref().expect("a draft").rect;
        let mut session = ClientSession {
            revision: 3,
            ..ClientSession::default()
        };
        session.preview.selection = HistorySelection::Entry(entry_id);
        let _ = editor.update(Message::SessionUpdated(Ok((session, 1))));
        assert!(!editor.session.preview.can_edit());
        assert!(
            editor.crop.is_some(),
            "selecting a historical state keeps the draft"
        );
        assert!(!editor.drafting(), "the plain historical preview is shown");
        assert_eq!(editor.snapshot()["crop"]["paused"], json!(true));
        // Nothing can be applied or started while previewing history.
        let _ = editor.update(Message::Crop(CropMessage::Apply));
        assert!(editor.crop.is_some());
        assert!(editor.crop_applying.is_none());
        assert_eq!(editor.crop.as_ref().expect("a draft").rect, composed);
        let _ = std::hint::black_box(&asset);
        finish(editor, catalog);
    }
}
