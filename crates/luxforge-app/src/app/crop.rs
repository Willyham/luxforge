//! The crop gesture. A crop draft is a core draft ([`crate::app::gesture::Kind::Crop`]), driven by
//! the one draft driver like a slider's or a mask's: it commits, cancels and reapplies through the
//! shared [`crate::app::message::draft::DraftMessage`]s, `session.state` reports it and only Apply commits
//! it. Its frame opens at once, from the current entry's `recipe.describe` rows — the crop layer's
//! values, the stage it receives and the orientation ahead of it — and is drawn and dragged over
//! that stage once its pixels have rendered. Every change goes through `crop_update`, so the
//! API-equivalent path and the pointer path are the same code, and the end of each change is one
//! synchronous `draft.set` of the payload's declared fields.
use crate::app::Before;
use crate::{
    app::{
        Editor,
        draft::{CoreDraft, Event},
        evidence::Settle,
        gesture::{Kind, Starting},
        message::{Message, control::ControlMessage, crop::CropMessage, crop::CropPointer},
        tasks::{Refresh, crop_preview_task, mutation},
    },
    crop_draft::{CropDraft, Modifiers as DraftModifiers},
    state::{
        fields::{self, field_id},
        number::NumberSpec,
        tools::{crop_frame, crop_row},
    },
};
use iced::{Task, widget::operation};
use luxforge_core::{
    CropPayload, CropStage, LayerId, Orientation, POINTER_MODE, insertion_index_among,
};
use serde_json::{Map, Value, json};

/// The scrollable around the photo, so a Space drag can scroll it while drafting a crop.
pub(crate) const SURFACE_ID: &str = "luxforge.surface";

/// The crop frame editor's gesture on the core draft lifecycle: the declared action its draft
/// commits, the frame, and where the crop layer's input stage — the pixels the frame is drawn
/// over — has got to.
#[derive(Clone, Debug)]
pub(crate) struct CropGesture {
    pub(crate) action: String,
    pub(crate) frame: CropDraft,
    pub(crate) stage: StageView,
    /// The frames the input stage's phases have delivered, and which stack it was planned from.
    pub(crate) frames: StageFrames,
    /// The preview generation of the input stage's job on its way, which belongs to the draft
    /// rather than to the displayed state.
    pub(crate) generation: Option<u64>,
}

/// The crop layer's input stage as the draft holds it, by the one rule the photograph follows
/// ([`Editor::proxy_bounds_for`]): a display-size proxy of the layer prefix wherever the view draws
/// the stage smaller than it is, and the exact stage only at a percentage zoom that needs it. Each
/// frame is kept once delivered, so a zoom that crosses back hands it over again. The planned job
/// is never kept: it holds its stack, and a RAW development's planes hold the source worker's
/// memory gate, so a kept one would keep a development the owner needs waiting for as long as the
/// draft is open. A zoom that needs a phase no held frame serves plans the stage again, from the
/// stack's identity, on an owner task ([`StagePlan::Zoom`]). Everything here ends with the draft.
/// No frame here is ever reduced, sampled or committed: the draft commits its fields.
#[derive(Clone, Debug, Default)]
pub(crate) struct StageFrames {
    /// The stack the stage was planned from: the entry a zoom plans it again from, and what the
    /// plan it answers must be.
    planned: Option<Box<luxforge_core::analysis::AnalysisIdentity>>,
    /// A zoom's plan of the stage is on its way, so a second is not asked for.
    replanning: bool,
    /// The frame for a view that draws the stage smaller than it is: the prefix's proxy, or the
    /// exact stage when the worker declined one (the stage already fits the bounds, or a layer of
    /// the prefix has no proxy).
    bounded: Option<StageFrame>,
    /// The exact stage, for a percentage zoom that draws it at or above its size.
    exact: Option<luxforge_core::Raster>,
    /// What the presenter holds.
    shown: Option<Shown>,
}

#[derive(Clone, Debug)]
struct StageFrame {
    raster: luxforge_core::Raster,
    proxy: bool,
}

/// Which frame of the stage is on screen: one a bounded job delivered, and whether it is a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    bounded: bool,
    proxy: bool,
}

impl Shown {
    /// Whether this frame is what a view that does or does not bound the stage asks for.
    fn serves(self, bounded: bool) -> bool {
        if bounded { self.bounded } else { !self.proxy }
    }
}

/// Which plan of a crop draft's input stage an owner task answers
/// ([`crate::app::tasks::crop_preview_task`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StagePlan {
    /// The stage a start or a Reapply opens on, planned from the current entry.
    Open,
    /// The stage already on screen, planned again from its entry for a zoom that needs a phase
    /// no held frame serves.
    Zoom(luxforge_core::EntryId),
}

/// Where the crop layer's input stage has got to. The frame never waits for it: the section and
/// the draft are live from the start, and the canvas draws the frame once the stage is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageView {
    /// Being planned or rendered for the stack at `base_revision`: at the start, or after a
    /// Reapply rebased the frame onto the stage the rows now report.
    Rendering { reapply: bool, base_revision: u64 },
    /// On screen under the frame.
    Shown,
    /// A rebased frame's stage will not arrive. The draft is kept, still conflicted when it is, and
    /// the photograph is shown until a Reapply asks for the stage again.
    Missing,
    /// A starting draft's stage will not arrive. The draft has ended for everything but its core
    /// draft, which is discarded once the update is over ([`Editor::close_abandoned_crop`]).
    Abandoned,
}

impl CropGesture {
    /// Correlated evidence: the frame's geometry with the revision its core draft is based on and
    /// whether that draft is conflicted.
    pub(crate) fn summary(&self, draft: &CoreDraft) -> Value {
        let mut summary = self.frame.summary();
        summary["base_revision"] = json!(draft.base_revision);
        summary["conflicted"] = json!(draft.conflicted);
        summary
    }
}

/// What the current entry's `recipe.describe` rows say a crop draft opens on. The desktop reads
/// the stage, the orientation ahead and the committed frame from the rows, and folds no geometry
/// and parses no payload itself.
struct CropRow {
    /// The stack's first crop layer, with the frame its row's values hold: `None` when its
    /// provider could not read them.
    layer: Option<(LayerId, Option<CropPayload>)>,
    /// How many layers precede the crop: the preview truncation that shows its input stage.
    layer_index: usize,
    input: CropStage,
    ahead: Orientation,
}

impl CropRow {
    /// The committed straightening angle, or 0 without a crop layer.
    fn angle(&self) -> f64 {
        match &self.layer {
            Some((_, Some(payload))) => payload.angle,
            _ => 0.0,
        }
    }
}

/// A change a control of the crop section makes to an open draft. From the idle section the same
/// change first opens the draft, seeded from the committed crop exactly as Start seeds it. Turning
/// the Straighten guide off changes nothing without a draft, so only turning it on opens one.
fn changes_draft(message: &CropMessage) -> bool {
    matches!(
        message,
        CropMessage::Preset(_) | CropMessage::Lock | CropMessage::Swap | CropMessage::Guide(true)
    )
}

/// What the angle's stepper does to the frame. A rail drag and a held arrow key move the angle
/// live and log nothing, and their release is the one draft change, as a frame gesture's end is; a
/// button press, a submitted number and a reset are one draft change each.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AngleChange {
    Move(f64),
    Set(f64),
    Release,
}

/// The crop frame's declared angle field, read from its parameter: the spec its stepper steps,
/// snaps and formats with, and the default a reset returns it to.
struct AngleField {
    parameter: String,
    spec: NumberSpec,
    default: f64,
}

impl Editor {
    /// The crop gesture in the slot, whatever its stage has got to.
    pub(crate) fn crop_gesture(&self) -> Option<&CropGesture> {
        self.core_gesture()?.crop()
    }

    fn crop_gesture_mut(&mut self) -> Option<&mut CropGesture> {
        match &mut self.core_gesture_mut()?.kind {
            Kind::Crop(crop) => Some(crop),
            _ => None,
        }
    }

    /// The preview generation of the open crop draft's input stage job still on its way.
    pub(crate) fn draft_generation(&self) -> Option<u64> {
        self.crop_gesture().and_then(|crop| crop.generation)
    }

    /// Note which preview generation is the open crop draft's input stage, or that none is.
    pub(crate) fn set_draft_generation(&mut self, generation: Option<u64>) {
        if let Some(crop) = self.crop_gesture_mut() {
            crop.generation = generation;
        }
    }

    /// The open crop frame. A start whose stage will not arrive has none: it is ending.
    pub(crate) fn crop(&self) -> Option<&CropDraft> {
        self.crop_gesture()
            .filter(|crop| crop.stage != StageView::Abandoned)
            .map(|crop| &crop.frame)
    }

    pub(crate) fn crop_mut(&mut self) -> Option<&mut CropDraft> {
        self.crop_gesture_mut()
            .filter(|crop| crop.stage != StageView::Abandoned)
            .map(|crop| &mut crop.frame)
    }

    /// Where the open crop draft's input stage has got to.
    pub(crate) fn crop_stage(&self) -> Option<StageView> {
        self.crop_gesture().map(|crop| crop.stage)
    }

    /// Read what a crop draft opens on from the current entry's `recipe.describe` rows: the first
    /// crop layer's row, or, for a stack without one, the stage a crop the host appends at its
    /// insertion index would receive — that row's own, or the stack's output past the last row.
    /// The reason names what is missing when the rows cannot say.
    fn crop_row(&self) -> Result<CropRow, String> {
        let frame = crop_frame(&self.modules).ok_or("No module declares a crop frame")?;
        let effect = frame
            .effect()
            .ok_or("The crop module declares no geometry effect")?;
        let state = self
            .document
            .state
            .as_ref()
            .ok_or("No photograph is open")?;
        let recipe = self
            .document
            .current_recipe
            .as_ref()
            .filter(|recipe| recipe.entry_id == state.current_entry.id)
            .ok_or("The current recipe is still being read; crop again once it has arrived")?;
        let found = crop_row(&frame, recipe);
        let layer_index = found.map_or_else(
            || {
                insertion_index_among(
                    &self.modules,
                    &state.current_entry.snapshot.recipe.layers,
                    effect,
                )
            },
            |(index, _)| index,
        );
        let (stage, ahead) = match recipe.layers.get(layer_index) {
            Some(row) => (row.input_stage, row.input_orientation),
            None => (recipe.output_stage, recipe.output_orientation),
        };
        let (Some(stage), Some(ahead)) = (stage, ahead) else {
            return Err(
                "The crop's input stage cannot be known: a layer ahead of it cannot be read".into(),
            );
        };
        Ok(CropRow {
            layer: found.map(|(_, row)| (row.id.clone(), frame.payload(&row.values))),
            layer_index,
            input: CropStage {
                width: stage.width,
                height: stage.height,
                angle: 0.0,
            },
            ahead,
        })
    }

    /// The frame a draft opens with: the committed crop layer's own rectangle and angle, or a
    /// neutral crop of the whole stage for a stack without one. An unreadable crop layer is never
    /// silently replaced by a neutral crop: the stored layer stays exactly as it is and no draft
    /// opens.
    fn crop_opening(&self, row: CropRow) -> Result<CropDraft, String> {
        let presets = crop_frame(&self.modules)
            .map(|frame| frame.presets())
            .unwrap_or_default();
        let mut frame = match row.layer {
            Some((layer, Some(payload))) => {
                CropDraft::from_layer(row.input, payload, layer, row.layer_index, &presets)
            }
            Some((_, None)) => {
                return Err(
                    "The existing crop layer's payload cannot be read; no draft was opened".into(),
                );
            }
            None => CropDraft::neutral(row.input, row.layer_index),
        };
        frame.ahead = row.ahead;
        Ok(frame)
    }

    /// The input stage the crop gesture waited for will not arrive. A rebased frame keeps its
    /// draft; a start leaves the canvas at once and its core draft is discarded once the update is
    /// over ([`Self::close_abandoned_crop`]). Returns whether it was a reapply.
    pub(crate) fn crop_stage_lost(&mut self) -> bool {
        self.set_draft_generation(None);
        let Some(crop) = self.crop_gesture_mut() else {
            return false;
        };
        let StageView::Rendering { reapply, .. } = crop.stage else {
            return false;
        };
        crop.stage = if reapply {
            StageView::Missing
        } else {
            StageView::Abandoned
        };
        if !reapply {
            self.end_crop_view(None);
        }
        reapply
    }

    /// A start whose stage will not arrive has nothing left to show. Its core draft is discarded
    /// here, once per update, because the failure is often learned where no task can be returned —
    /// a preview request that replaced its stage.
    pub(crate) fn close_abandoned_crop(&mut self) -> Task<Message> {
        match self.crop_stage() {
            Some(StageView::Abandoned) => self.discard(),
            _ => Task::none(),
        }
    }

    /// Every crop draft change goes through here, so the API-equivalent path and the pointer path
    /// are the same code.
    pub(crate) fn crop_update(&mut self, message: CropMessage) -> Task<Message> {
        if self.crop().is_none() && changes_draft(&message) {
            return self.idle_change(message);
        }
        match message {
            CropMessage::Option(option) => self.crop_section.option = option,
            CropMessage::Space(space) => self.crop_section.space = space,
            CropMessage::Guide(guide) => self.crop_section.guide = guide && self.crop().is_some(),
            CropMessage::Start => return self.crop_start(),
            // The job goes straight to the preview worker, as a start's does, or is dropped: this
            // module names no planned job, so none is kept here (`desktop-keeps-no-preview-job`).
            CropMessage::PreviewReady(StagePlan::Zoom(entry), result) => {
                let planned = result.as_ref().map(|job| (&job.identity, job.layer_count));
                if self.crop_stage_replanned(&entry, planned)
                    && let Ok(job) = result
                {
                    let generation = self.request_preview(*job);
                    self.set_draft_generation(Some(generation));
                }
            }
            CropMessage::PreviewReady(StagePlan::Open, result) => match result {
                // The stack or the selection changed since the stage was asked for, so the input
                // stage the owner planned is not the one the frame is on, and nothing else will
                // arrive for it.
                Ok(job) if !self.crop_stage_current(&job.evaluation.entry().id) => {
                    self.draft_preview_superseded(None);
                }
                Ok(job) => {
                    // Only the stack's identity is kept, for a zoom that needs the stage's other
                    // phase: the job goes to the preview worker.
                    if let Some(crop) = self.crop_gesture_mut() {
                        crop.frames.planned = Some(Box::new(job.identity.clone()));
                    }
                    // A job an earlier start asked for is no longer the draft's once this one is
                    // requested, so this request superseding it ends nothing.
                    self.set_draft_generation(None);
                    let generation = self.request_preview(*job);
                    self.set_draft_generation(Some(generation));
                    self.status.text = "Rendering the crop's input stage…".into();
                }
                Err(error) => {
                    if matches!(self.crop_stage(), Some(StageView::Rendering { .. })) {
                        self.crop_stage_lost();
                        self.status.text = error;
                        self.settle_step(Settle::Draft);
                    }
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
                        return self.crop_changed("crop_draft_changed");
                    }
                }
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
                return self.crop_changed("crop_draft_changed");
            }
            CropMessage::CustomWidth(text) => self.crop_section.custom.0 = text,
            CropMessage::CustomHeight(text) => self.crop_section.custom.1 = text,
            CropMessage::Swap => {
                if let Some(draft) = self.crop_mut() {
                    draft.swap();
                }
                return self.crop_changed("crop_draft_changed");
            }
            CropMessage::Lock => {
                if let Some(draft) = self.crop_mut() {
                    draft.lock_toggle();
                }
                return self.crop_changed("crop_draft_changed");
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
        }
        Task::none()
    }

    /// Open a draft on the current stack. The frame opens now, from the current entry's rows, and
    /// the core draft's `draft.begin` and the input stage's truncated preview go out together: the
    /// section is live at once, and the canvas draws the frame when the stage's pixels arrive.
    pub(crate) fn crop_start(&mut self) -> Task<Message> {
        // The crop draft already open is what was asked for: nothing starts and nothing is refused.
        if self.crop().is_some() {
            return Task::none();
        }
        // One draft per client, the current state and no request in flight, in one refusal.
        if let Some(reason) = self.gesture_refusal(Starting::Crop) {
            self.status.text = reason;
            return Task::none();
        }
        let Some((module_id, action)) = crop_frame(&self.modules)
            .map(|frame| (frame.module.id.clone(), frame.action.to_owned()))
        else {
            return Task::none();
        };
        let frame = match self.crop_row().and_then(|row| self.crop_opening(row)) {
            Ok(frame) => frame,
            Err(reason) => {
                self.status.text = reason;
                return Task::none();
            }
        };
        let Some(state) = self.document.state.as_ref() else {
            return Task::none();
        };
        let (asset, base_revision) = (state.asset.id.clone(), state.revision);
        let layer_index = frame.layer_index;
        // Starting a draft by any route — the section's own button, `R`, the mode strip, a change
        // in the idle section or a scripted `draft.start` — asks the session to enter this
        // module's mode, so the strip shows Crop selected for the whole life of the draft.
        self.sync.mode = Some(module_id);
        let crop = CropGesture {
            action,
            frame,
            stage: StageView::Rendering {
                reapply: false,
                base_revision,
            },
            frames: StageFrames::default(),
            generation: None,
        };
        let gesture = self.next_gesture();
        let begin = self.open_core(gesture, Kind::Crop(crop), None);
        // A refused begin opened nothing and has said why.
        if self.crop().is_none() {
            return begin;
        }
        // The opened frame is the draft's first fields, sent at once: `draft.begin` has answered.
        let fields = self.crop_changed("crop_draft_started");
        let stage = crop_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            layer_index,
            StagePlan::Open,
        );
        self.status.text = "Preparing the crop's input stage…".into();
        Task::batch([begin, fields, stage])
    }

    /// The Changed elsewhere notice's Reapply for the crop draft: the frame is rebased at once onto
    /// the stage the current rows report — carried through a turn committed ahead of it, and on a
    /// stage of another size keeping its composition as far as it fits — the core draft is rebased
    /// by `draft.reapply` and the rebased frame's fields follow it, both in this update, and the new
    /// stage's pixels are asked for.
    pub(crate) fn crop_reapply(&mut self) -> Task<Message> {
        if self.busy || !self.session.preview.can_edit() || self.crop().is_none() {
            return Task::none();
        }
        let row = match self.crop_row() {
            Ok(row) => row,
            Err(reason) => {
                self.status.text = reason;
                return Task::none();
            }
        };
        let Some(state) = self.document.state.as_ref() else {
            return Task::none();
        };
        let (asset, base_revision) = (state.asset.id.clone(), state.revision);
        let Some(crop) = self.crop_gesture_mut() else {
            return Task::none();
        };
        crop.frame.rebase(
            row.input,
            row.layer.map(|(layer, _)| layer),
            row.layer_index,
            row.ahead,
        );
        crop.stage = StageView::Rendering {
            reapply: true,
            base_revision,
        };
        crop.frames = StageFrames::default();
        let rendering = crop.generation.take();
        // The stage on screen is the one the frame was rebased away from, and one still rendering
        // for it is stopped and held below the delivery floor, so neither is drawn under the
        // rebased frame nor taken up as the photograph.
        self.presentation.presenter.end_stage();
        if rendering.is_some() {
            self.presentation.preview_generation = self.cancel_preview_queue();
        }
        self.status.text = "Preparing the crop's input stage…".into();
        // The rebased frame's fields are offered first, and held while the draft is conflicted, so
        // the synchronous rebase sends them rather than the fields the frame was rebased away from.
        // A refused rebase says why in place of the line above.
        let fields = self.crop_changed("crop_draft_changed");
        let rebase = self.drive(Event::Reapply);
        let stage = crop_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            row.layer_index,
            StagePlan::Open,
        );
        Task::batch([fields, rebase, stage])
    }

    /// The input stage the draft asked for, planned from `entry`, is still the one its frame is
    /// on: the session still shows the current state, that entry is the current one this desktop
    /// holds, and the state has not moved since the stage was asked for. Decided from what the
    /// answer and the desktop already hold, so the job needs no currency request of its own.
    fn crop_stage_current(&self, entry: &luxforge_core::EntryId) -> bool {
        let (Some(state), Some(StageView::Rendering { base_revision, .. })) =
            (self.document.state.as_ref(), self.crop_stage())
        else {
            return false;
        };
        self.session.preview.can_edit()
            && &state.current_entry.id == entry
            && state.revision == base_revision
    }

    /// The crop layer's input stage is on screen, so the canvas draws the frame over it.
    pub(crate) fn crop_stage_shown(&mut self) {
        let Some(crop) = self.crop_gesture_mut() else {
            return;
        };
        if !matches!(crop.stage, StageView::Rendering { .. }) {
            return;
        }
        crop.stage = StageView::Shown;
        let input = crop.frame.stage;
        self.status.text = format!(
            "Crop draft on the layer's {} × {} input stage",
            input.width, input.height
        );
        self.settle_crop();
    }

    /// The bounds the crop draft's input stage is rendered at for the view now: the photograph's
    /// rule ([`Editor::proxy_bounds_for`]) over the stage the frame is on. `None` asks for the exact
    /// stage, as a percentage zoom that draws the stage at or above its size does.
    pub(crate) fn crop_stage_bounds(&self) -> Option<luxforge_core::ProxyBounds> {
        let stage = self.crop()?.stage;
        self.proxy_bounds_for(Some((stage.width, stage.height)))
    }

    /// Whether the crop draft's input stage, rather than the photograph, is what the view shows or
    /// is waiting for, so a zoom is answered by the stage.
    pub(crate) fn crop_stage_owns_view(&self) -> bool {
        self.session.preview.can_edit()
            && matches!(
                self.crop_stage(),
                Some(StageView::Rendering { .. } | StageView::Shown)
            )
    }

    /// One frame of the crop layer's input stage arrived: the layer prefix's proxy, or the exact
    /// prefix. `bounded` says its job offered display bounds, so the frame is what a bounded view
    /// shows even when the worker declined the proxy and rendered it exactly; `last` says the job
    /// has nothing more to deliver. The frame is kept for the view it serves, and shown when it is
    /// the draft's first frame or serves the view now; the other is then asked for if the view has
    /// moved on. Returns whether a frame was handed to the display, and the plan of that other
    /// phase when one is needed.
    pub(crate) fn crop_stage_ready(
        &mut self,
        raster: &luxforge_core::Raster,
        proxy: bool,
        bounded: bool,
        last: bool,
    ) -> (bool, Task<Message>) {
        if last {
            self.set_draft_generation(None);
        }
        let wants_bounded = self.crop_stage_bounds().is_some();
        let Some(crop) = self.crop_gesture_mut() else {
            return (false, Task::none());
        };
        if bounded {
            crop.frames.bounded = Some(StageFrame {
                raster: raster.clone(),
                proxy,
            });
        }
        if !proxy {
            crop.frames.exact = Some(raster.clone());
        }
        let shown = Shown { bounded, proxy };
        let first = matches!(crop.stage, StageView::Rendering { .. });
        if !first && !shown.serves(wants_bounded) {
            return (false, Task::none());
        }
        crop.frames.shown = Some(shown);
        if !self.presentation.presenter.show_stage(raster) {
            self.crop_stage_lost();
            self.status.text = "Could not show the crop's input stage".into();
            self.settle_step(Settle::Draft);
            return (true, Task::none());
        }
        self.crop_stage_shown();
        self.settle_crop();
        (true, self.present_crop_stage())
    }

    /// Put the input-stage frame the view wants on screen: the one the draft already holds, or
    /// else one plan of the stage once no stage job or plan is on its way — the one on its way
    /// lands first and this runs again. Answers every zoom while the stage owns the view and every
    /// stage frame, so a zoom that changed while the stage rendered is picked up. The stage is
    /// planned again from the entry it was planned from, as a start plans it
    /// ([`crop_preview_task`]), and its answer ([`Self::crop_stage_replanned`]) requests it. A
    /// scripted step waits for the frame it asks for.
    pub(crate) fn present_crop_stage(&mut self) -> Task<Message> {
        if !self.show_held_crop_stage() {
            return Task::none();
        }
        self.await_frame(Settle::Draft);
        let in_flight = self.draft_generation().is_some();
        let Some(crop) = self.crop_gesture_mut() else {
            return Task::none();
        };
        if in_flight || crop.frames.replanning {
            return Task::none();
        }
        let Some(planned) = &crop.frames.planned else {
            return Task::none();
        };
        let (asset, entry) = (planned.asset_id.clone(), planned.entry_id.clone());
        let layer_count = crop.frame.layer_index;
        crop.frames.replanning = true;
        crop_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            layer_count,
            StagePlan::Zoom(entry),
        )
    }

    /// Hand over the held frame the view wants when the one on screen does not serve it. Returns
    /// whether the view still wants a frame of the stage the draft does not hold.
    fn show_held_crop_stage(&mut self) -> bool {
        let wants_bounded = self.crop_stage_bounds().is_some();
        let Some(crop) = self.crop_gesture_mut() else {
            return false;
        };
        if crop.stage != StageView::Shown
            || crop
                .frames
                .shown
                .is_some_and(|shown| shown.serves(wants_bounded))
        {
            return false;
        }
        let held = if wants_bounded {
            crop.frames.bounded.as_ref().map(|frame| {
                (
                    frame.raster.clone(),
                    Shown {
                        bounded: true,
                        proxy: frame.proxy,
                    },
                )
            })
        } else {
            crop.frames.exact.clone().map(|raster| {
                (
                    raster,
                    Shown {
                        bounded: false,
                        proxy: false,
                    },
                )
            })
        };
        let Some((raster, shown)) = held else {
            return true;
        };
        crop.frames.shown = Some(shown);
        if !self.presentation.presenter.show_stage(&raster) {
            self.status.text = "Could not show the crop's input stage".into();
        }
        false
    }

    /// The stage a zoom planned again ([`Self::present_crop_stage`]) answered with the stack it
    /// planned and the layers it truncates to. Returns whether to request it, exactly as a start's
    /// is: when it answers the draft's own plan of the stack on screen and the view still wants a
    /// phase no held frame serves. Otherwise it is dropped: the draft ended, a Reapply moved the
    /// stage to another entry, or the zoom moved back.
    fn crop_stage_replanned(
        &mut self,
        entry: &luxforge_core::EntryId,
        answer: Result<(&luxforge_core::analysis::AnalysisIdentity, Option<usize>), &String>,
    ) -> bool {
        let Some(crop) = self.crop_gesture_mut() else {
            return false;
        };
        let Some(planned) = crop
            .frames
            .planned
            .as_ref()
            .filter(|planned| crop.frames.replanning && &planned.entry_id == entry)
        else {
            return false;
        };
        let current = answer.map_err(Clone::clone).and_then(|(identity, layers)| {
            if identity == &**planned && layers == Some(crop.frame.layer_index) {
                Ok(())
            } else {
                Err("The crop's input stage planned again is not the one on screen".to_owned())
            }
        });
        crop.frames.replanning = false;
        if let Err(error) = current {
            self.status.text = error;
            self.settle_step(Settle::Draft);
            return false;
        }
        if self.draft_generation().is_some() || !self.show_held_crop_stage() {
            self.settle_step(Settle::Draft);
            return false;
        }
        let bounded = self.crop_stage_bounds().is_some();
        self.event("crop_stage_requested", json!({ "bounded": bounded }));
        true
    }

    /// Correlated evidence of the input stage on screen: whether it is a proxy and the frame's
    /// size, and which frames the draft holds.
    pub(crate) fn crop_stage_frame_summary(&self) -> Value {
        let Some(frames) = self.crop_gesture().map(|crop| &crop.frames) else {
            return Value::Null;
        };
        json!({
            "phase": frames.shown.map(|shown| if shown.proxy { "proxy" } else { "exact" }),
            "size": self.presentation.presenter.stage().map(|frame| [frame.size().0, frame.size().1]),
            "held_bounded": frames.bounded.is_some(),
            "held_exact": frames.exact.is_some(),
        })
    }

    /// A scripted step waiting for the crop draft settles once the frame can be captured over its
    /// stage: the stage is on screen. A Reapply's `draft.reapply` answers in the update that sends
    /// it, so a captured frame never shows the frame rebased and the draft still conflicted.
    pub(crate) fn settle_crop(&mut self) {
        let ready = self.core_gesture().is_some_and(|gesture| {
            gesture
                .crop()
                .is_some_and(|crop| crop.stage == StageView::Shown)
        });
        if ready {
            self.settle_step(Settle::Draft);
        }
    }

    /// A control of the idle section changed: open the draft seeded from the committed crop, as
    /// Start does, and apply the change to it at once. Nothing commits until Apply.
    fn idle_change(&mut self, message: CropMessage) -> Task<Message> {
        let start = self.crop_start();
        let Some(draft) = self.crop() else {
            return start;
        };
        // Choosing the ratio the draft already shows chosen changes nothing, because refitting the
        // committed rectangle to it could only trim a pixel from it.
        if let CropMessage::Preset(index) = message
            && crop_frame(&self.modules)
                .and_then(|frame| frame.presets().get(index).cloned())
                .is_some_and(|preset| preset.option == draft.preset)
        {
            return start;
        }
        Task::batch([start, self.crop_update(message)])
    }

    /// Whether `action` is the crop frame's declared action, whose fields are the open frame's.
    pub(crate) fn is_crop_action(&self, action: &str) -> bool {
        crop_frame(&self.modules).is_some_and(|frame| frame.action == action)
    }

    /// The crop frame's angle field as its parameter declares it.
    fn angle_field(&self) -> Option<AngleField> {
        let frame = crop_frame(&self.modules)?;
        let declared = fields::declared(&self.modules, frame.action, frame.angle)?;
        Some(AngleField {
            parameter: frame.angle.to_owned(),
            spec: NumberSpec::of(declared)?,
            default: fields::parse_field(declared, &fields::seed_text(declared))
                .ok()
                .and_then(|value| value.as_f64())?,
        })
    }

    /// The angle the section shows: the open frame's, or the committed one while idle.
    fn shown_angle(&self) -> f64 {
        self.crop()
            .map_or_else(|| self.committed_angle(), |draft| draft.stage.angle)
    }

    /// A message from a control of the crop frame's action: the angle's stepper, which sends what
    /// every number control sends. Each becomes a change of the frame rather than a request of its
    /// own, from the idle section too, where the change first opens the draft. The frame's other
    /// fields have no control, so a message naming one changes nothing.
    pub(crate) fn crop_control(&mut self, message: ControlMessage) -> Task<Message> {
        let Some(angle) = self.angle_field() else {
            return Task::none();
        };
        if message
            .field()
            .is_none_or(|(_, parameter)| parameter != angle.parameter)
        {
            return Task::none();
        }
        let spec = angle.spec;
        let change = match message {
            ControlMessage::Field {
                action,
                parameter,
                text,
            } => {
                self.controls.fields.set(&action, &parameter, text);
                self.controls.editing = Some((action, parameter));
                return Task::none();
            }
            // The box opens on the angle the section shows, whatever the text last held.
            ControlMessage::EditValue { action, parameter } => {
                let text = spec.format(self.shown_angle());
                self.controls.fields.set(&action, &parameter, text);
                let id = field_id(&action, &parameter, None);
                self.controls.editing = Some((action, parameter));
                return operation::focus(iced::widget::Id::from(id));
            }
            ControlMessage::Submit { .. } => match self.typed_angle() {
                Some(value) => AngleChange::Set(value),
                None => return Task::none(),
            },
            ControlMessage::Fraction { fraction, .. } => {
                let Some(value) = spec.at_fraction(fraction).as_f64() else {
                    return Task::none();
                };
                AngleChange::Move(value)
            }
            ControlMessage::KeyNudge {
                direction,
                shift,
                option,
                ..
            } => AngleChange::Move(spec.nudged(self.shown_angle(), direction, shift, option)),
            ControlMessage::Step { direction, .. } => {
                AngleChange::Set(spec.nudged(self.shown_angle(), direction, false, false))
            }
            ControlMessage::Released { .. } => AngleChange::Release,
            ControlMessage::ResetField { .. } => AngleChange::Set(angle.default),
            _ => return Task::none(),
        };
        self.angle_change(change)
    }

    /// Apply one change of the angle's stepper to the open frame, or open the draft for it.
    fn angle_change(&mut self, change: AngleChange) -> Task<Message> {
        if self.crop().is_none() {
            return self.idle_angle_change(change);
        }
        match change {
            // The frame follows the angle, rectangle and all, but a move logs nothing.
            AngleChange::Move(angle) => {
                if let Some(draft) = self.crop_mut() {
                    draft.set_angle(angle);
                }
                self.show_crop_angle();
                Task::none()
            }
            AngleChange::Set(angle) => self.set_crop_angle(angle),
            AngleChange::Release => self.crop_changed("crop_draft_changed"),
        }
    }

    /// An angle change from the idle section opens the draft, as the section's other controls do,
    /// and applies to it at once. A change that leaves the committed angle as it is opens nothing:
    /// a release with no move, or a number, a step or a reset that lands on the committed angle.
    fn idle_angle_change(&mut self, change: AngleChange) -> Task<Message> {
        match change {
            AngleChange::Release => return Task::none(),
            AngleChange::Set(angle) if angle == self.committed_angle() => return Task::none(),
            AngleChange::Move(_) | AngleChange::Set(_) => {}
        }
        let start = self.crop_start();
        if self.crop().is_none() {
            return start;
        }
        Task::batch([start, self.angle_change(change)])
    }

    /// The angle typed into the box, read as its parameter declares it. Text that is not an angle
    /// in range keeps the box open for correcting, with the reason; an angle closes it.
    fn typed_angle(&mut self) -> Option<f64> {
        let frame = crop_frame(&self.modules)?;
        let declared = fields::declared(&self.modules, frame.action, frame.angle)?;
        match self.controls.fields.get_value(frame.action, declared) {
            Ok(value) => {
                if self.editing_angle() {
                    self.controls.editing = None;
                }
                value.as_f64()
            }
            Err(reason) => {
                self.status.text = reason;
                None
            }
        }
    }

    /// Straighten the open frame to `degrees`: one draft change.
    fn set_crop_angle(&mut self, degrees: f64) -> Task<Message> {
        if let Some(draft) = self.crop_mut() {
            draft.set_angle(degrees);
        }
        self.crop_changed("crop_draft_changed")
    }

    /// The straightening angle of the current entry's crop layer as its `recipe.describe` row
    /// reports it, which the idle section shows and a draft opened on it starts at; 0 without one.
    fn committed_angle(&self) -> f64 {
        self.crop_row().map_or(0.0, |row| row.angle())
    }

    /// The angle's text follows the open frame, so a box open for typing shows the angle every
    /// change leaves.
    fn show_crop_angle(&mut self) {
        let (Some(frame), Some(draft)) = (crop_frame(&self.modules), self.crop()) else {
            return;
        };
        let Some(spec) =
            fields::declared(&self.modules, frame.action, frame.angle).and_then(NumberSpec::of)
        else {
            return;
        };
        let (action, parameter) = (frame.action.to_owned(), frame.angle.to_owned());
        let text = spec.format(draft.stage.angle);
        self.controls.fields.set(&action, &parameter, text);
    }

    /// One draft change reached its end: the angle's text follows the frame, the new state is
    /// logged and the core draft is offered the payload's declared fields, which it sets now, on
    /// this thread, unless a round trip is in flight or the draft is conflicted. Pointer moves
    /// inside a gesture do not come through here.
    pub(crate) fn crop_changed(&mut self, event: &'static str) -> Task<Message> {
        let Some(gesture) = self.core_gesture() else {
            return Task::none();
        };
        let (Some(crop), Some(frame)) = (gesture.crop(), crop_frame(&self.modules)) else {
            return Task::none();
        };
        let fields = Value::Object(frame.params(&crop.frame.payload()).into_iter().collect());
        let summary = crop.summary(&gesture.draft);
        self.show_crop_angle();
        self.event(event, summary);
        self.drive(Event::Offer(fields))
    }

    /// The crop angle's box is open for typing.
    fn editing_angle(&self) -> bool {
        crop_frame(&self.modules).is_some_and(|frame| {
            self.controls
                .editing
                .as_ref()
                .is_some_and(|(action, parameter)| {
                    action == frame.action && parameter == frame.angle
                })
        })
    }

    /// Put the canvas back as it was before the draft: drop the input stage it displayed, and a
    /// stage still on its way (`rendering`, the ended gesture's generation), and return the session
    /// to pointer. Every way a crop draft ends — Apply, Cancel, a scripted cancel or apply, a start
    /// that failed — comes through here.
    pub(crate) fn end_crop_view(&mut self, rendering: Option<u64>) {
        self.presentation.presenter.end_stage();
        // A stage still rendering is stopped and held below the delivery floor, so it can never be
        // taken up as the photograph once nothing marks it as the draft's.
        if rendering.is_some() {
            self.presentation.preview_generation = self.cancel_preview_queue();
        }
        self.crop_section.guide = false;
        if self.editing_angle() {
            self.controls.editing = None;
        }
        self.sync.mode = Some(POINTER_MODE.into());
    }

    /// The crop gesture was discarded: the frame leaves the screen and says so, unless it is a
    /// start whose stage never arrived, which already said why it ended.
    pub(crate) fn crop_discarded(&mut self, crop: &CropGesture, draft: &CoreDraft) {
        self.end_crop_view(crop.generation);
        if crop.stage != StageView::Abandoned {
            self.event("crop_draft_discarded", crop.summary(draft));
            self.status.text = "Crop draft discarded".into();
        }
    }

    /// The crop gesture ended without a draft: its `draft.begin` was refused.
    pub(crate) fn crop_ended(&mut self) {
        self.end_crop_view(None);
        self.settle_step(Settle::Draft);
    }

    /// The crop draft's `draft.commit` answered with an entry, or with none because the frame is
    /// the committed crop. Either way the draft is over and the committed photograph returns.
    pub(crate) fn crop_committed(
        &mut self,
        crop: &CropGesture,
        draft: &CoreDraft,
        outcome: Option<Refresh>,
    ) -> Task<Message> {
        let summary = crop.summary(draft);
        self.end_crop_view(crop.generation);
        let Some(refresh) = outcome else {
            self.status.text = "Crop unchanged; nothing was committed".into();
            self.event("crop_draft_noop", summary);
            return match self.reseed_committed() {
                Some(task) => task,
                None => {
                    self.settle_step(Settle::Preview);
                    Task::none()
                }
            };
        };
        let (entry, sequence, revision) = (
            refresh.state.current_entry.id.clone(),
            refresh.state.current_entry.sequence,
            refresh.state.revision,
        );
        let request_id = refresh.request.clone();
        self.accept(refresh);
        self.event(
            "crop_draft_applied",
            json!({"request_id":request_id,"entry_id":entry.as_str(),"revision":revision,"draft":summary}),
        );
        self.status.text = format!("Crop applied \u{b7} entry {sequence}");
        Task::none()
    }

    /// The `custom` preset's two extents as typed, or `None` when either is not a positive number.
    fn custom_ratio(&self) -> Option<(f64, f64)> {
        let width: f64 = self.crop_section.custom.0.trim().parse().ok()?;
        let height: f64 = self.crop_section.custom.1.trim().parse().ok()?;
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some((width, height))
    }

    /// The `edit.<action>` request an independent client would send for the open draft's frame,
    /// against the revision the draft is based on, built from the canvas descriptor's own parameter
    /// names: what Copy as JSON request puts on the clipboard. The validation message stops it;
    /// `None` means there is nothing to copy.
    pub(crate) fn crop_copy_request(&self) -> Option<Result<(String, Value), String>> {
        let frame = crop_frame(&self.modules)?;
        let (gesture, draft, state) = (
            self.core_gesture()?,
            self.crop()?,
            self.document.state.as_ref()?,
        );
        if let Err(error) = draft.output() {
            return Some(Err(error.to_string()));
        }
        let mut request = Map::new();
        request.insert("asset_id".into(), json!(state.asset.id));
        request.insert(
            "mutation".into(),
            json!(mutation(gesture.draft.base_revision)),
        );
        request.extend(frame.params(&draft.payload()));
        Some(Ok((
            format!("edit.{}", frame.action),
            Value::Object(request),
        )))
    }
}

/// After every message: a start whose stage will not arrive is discarded
/// ([`Editor::close_abandoned_crop`]).
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.close_abandoned_crop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        draft::Round,
        message::{
            Message, control::ControlMessage, draft::DraftMessage, pointer::PointerMessage,
            sync::SyncMessage, view::ViewMessage,
        },
        tasks::SyncResult,
        testing::{
            CROP_ASPECTS, CROP_SOURCE, answer_commit, core_draft, crop_layer, described_at, entry,
            finish, open_crop, opened, refresh_for,
        },
    };
    use crate::crop_draft::{Corner, Handle};
    use crate::state::tools::{ControlModel, NumberControlStyle, SliderControl};
    use luxforge_core::{ClientSession, HistoryEntry, HistorySelection};

    /// The crop angle's declared step: one press of its − or + button.
    const STEP: f64 = 0.5;

    /// One message of the angle's stepper, naming the crop action's declared angle as the widget
    /// does.
    fn angle_message(
        editor: &Editor,
        message: impl FnOnce(String, String) -> ControlMessage,
    ) -> Message {
        let frame = crop_frame(&editor.modules).expect("a crop frame");
        Message::Control(message(frame.action.to_owned(), frame.angle.to_owned()))
    }

    /// Type `text` into the angle's box and press Enter.
    fn submit_angle(editor: &mut Editor, text: &str) {
        let typed = angle_message(editor, |action, parameter| ControlMessage::Field {
            action,
            parameter,
            text: text.to_owned(),
        });
        let _ = editor.update(typed);
        let enter = angle_message(editor, |action, parameter| ControlMessage::Submit {
            action,
            parameter: Some(parameter),
        });
        let _ = editor.update(enter);
    }

    /// One press of the angle's − (`-1`) or + (`1`) button.
    fn step_angle(editor: &mut Editor, direction: i8) {
        let press = angle_message(editor, |action, parameter| ControlMessage::Step {
            action,
            parameter,
            direction,
        });
        let _ = editor.update(press);
    }

    /// A drag on the angle's rail through `fractions`, not yet released.
    fn drag_angle(editor: &mut Editor, fractions: &[f64]) {
        for &fraction in fractions {
            let moved = angle_message(editor, |action, parameter| ControlMessage::Fraction {
                action,
                parameter,
                fraction,
            });
            let _ = editor.update(moved);
        }
    }

    /// The end of a drag on the angle's rail.
    fn release_angle(editor: &mut Editor) {
        let released = angle_message(editor, |action, parameter| ControlMessage::Released {
            action,
            parameter,
        });
        let _ = editor.update(released);
    }

    /// The crop section's angle control as the panel draws it.
    fn angle_control(editor: &Editor) -> SliderControl {
        editor
            .workspace
            .tools
            .all()
            .flat_map(|section| section.controls.iter())
            .find_map(|control| match control {
                ControlModel::CropFrame(model) => model.angle.clone(),
                _ => None,
            })
            .expect("the crop section's angle")
    }

    /// The angle the crop section's box shows, as a captured frame records it.
    fn section_angle(editor: &Editor) -> Value {
        editor.snapshot()["crop"]["section"]["angle"].clone()
    }

    /// The stage the rows of [`opened`] give a crop with no geometry ahead of it.
    fn stage() -> CropStage {
        CropStage {
            width: CROP_SOURCE.0,
            height: CROP_SOURCE.1,
            angle: 0.0,
        }
    }

    fn draft_message(editor: &mut Editor, message: DraftMessage) {
        let _ = editor.update(Message::Draft(message));
    }

    /// Another client's commit of `newer`, read back with the rows the owner describes for it.
    fn committed_elsewhere(
        editor: &mut Editor,
        asset: &luxforge_core::AssetId,
        newer: &HistoryEntry,
    ) {
        let mut refresh = refresh_for(asset, newer, vec![newer.clone()], &[newer], false);
        refresh.recipe = described_at(newer, CROP_SOURCE);
        let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
            refresh,
        )))));
    }

    /// The frame opens in the update that starts the draft, from the current entry's row for the
    /// stack's first crop layer: its layer, its index, the stage it receives and the rectangle and
    /// angle its values hold, exactly as committed. The stage's pixels and the core draft's
    /// `draft.begin` are still on their way.
    #[test]
    fn a_draft_opens_at_once_on_the_existing_crop_layers_row() {
        let payload = CropPayload {
            angle: 7.0,
            x: 0.2,
            y: 0.25,
            width: 0.4,
            height: 0.3,
        };
        let earlier = luxforge_core::Layer::pixel(0, 0, [1, 2, 3]);
        let crop = crop_layer(payload);
        let (mut editor, catalog, _, _) = opened(
            vec![earlier, crop.clone(), crop_layer(CropPayload::NEUTRAL)],
            4,
        );
        // Only the first crop layer is the one being edited.
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let draft = editor.crop().expect("the frame opened with the start");
        assert_eq!(draft.layer, Some(crop.id.clone()));
        assert_eq!(draft.layer_index, 1, "the stage is the pixel edit's output");
        assert_eq!((draft.stage.width, draft.stage.height), CROP_SOURCE);
        assert_eq!(draft.stage.angle, 7.0);
        let reopened = CropStage {
            angle: 7.0,
            ..stage()
        };
        assert_eq!(
            draft.output().expect("a valid draft"),
            payload.output_rect(&reopened).expect("a valid payload"),
            "reopening shows exactly the rectangle the payload committed"
        );
        assert_eq!(section_angle(&editor), json!("7.0"));
        assert_eq!(
            editor.crop_stage(),
            Some(StageView::Rendering {
                reapply: false,
                base_revision: 4
            }),
            "the stage's pixels are on their way"
        );
        assert!(
            core_draft(&editor).is_some_and(CoreDraft::drained),
            "the draft opened and took the frame's fields in the start's own update"
        );
        assert_eq!(editor.snapshot()["crop"]["layer_index"], json!(1));
        open_crop(&mut editor);
        assert_eq!(editor.crop_stage(), Some(StageView::Shown));
        assert_eq!(core_draft(&editor).expect("a core draft").base_revision, 4);
        finish(editor, catalog);
    }

    /// The angle's rail is a continuous gesture on the draft: every move follows the rail on its
    /// step and refits the rectangle, only the release logs a draft change, and nothing commits.
    #[test]
    fn a_drag_on_the_angle_rail_drafts_the_angle_and_logs_once_on_release() {
        let (mut editor, catalog, _, _) =
            opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
        let log = crate::app::testing::attach_log(&mut editor);
        // 2.4° is 47.4 of the rail's 90°; a fraction a hair off it snaps to the angle's declared
        // 0.05° fine step.
        drag_angle(&mut editor, &[0.6, 0.5 + 2.4 / 90.0 + 1e-4]);
        let draft = editor.crop().expect("the draft stays open");
        assert_eq!(draft.stage.angle, 2.4);
        let rect = draft.rect;
        // The generic stepper with its rail, reading 2.4°, its handle live for the open draft.
        let control = angle_control(&editor);
        assert_eq!(control.style, NumberControlStyle::Stepper { rail: true });
        assert_eq!(
            (control.display.as_str(), control.unit.as_deref()),
            ("2.4", Some("\u{b0}"))
        );
        assert_eq!((control.value, control.dragging), (2.4, true));
        assert_eq!((control.spec.step, control.spec.fine_step), (0.5, 0.05));
        release_angle(&mut editor);
        assert_eq!(editor.crop().expect("still drafting").rect, rect);
        assert_eq!(editor.document.state.as_ref().expect("a state").revision, 2);
        let changes: Vec<Value> = crate::app::testing::logged(&mut editor, &log)
            .into_iter()
            .filter(|record| record["event"] == "crop_draft_changed")
            .collect();
        assert_eq!(changes.len(), 1, "one draft change, at the release");
        assert_eq!(changes[0]["detail"]["angle"], json!(2.4));
        finish(editor, catalog);
    }

    /// A double-click on the angle's rail sends the generic `ResetField`, which puts the angle back
    /// to its declared default of 0 and sends it, as a button press sends its angle: the frame,
    /// the angle's box and the client's core draft all read 0, and exactly one draft change is
    /// logged and set on the core draft. Nothing commits.
    #[test]
    fn a_double_click_on_the_angle_rail_resets_the_angle_as_one_change() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-crop-reset-{}-{}.sqlite",
            std::process::id(),
            crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let (mut editor, _, _) = crate::app::testing::real_photo(&catalog);
        let (owner, client) = (editor.owner.clone(), editor.client);
        let draft = || {
            crate::app::tasks::call(&owner, client, "session.state", json!({}))
                .expect("a session")
                .0["draft"]
                .clone()
        };
        let revision = editor
            .document
            .state
            .as_ref()
            .expect("a photograph")
            .revision;
        let _ = editor.update(Message::Crop(CropMessage::Start));
        step_angle(&mut editor, 1);
        assert_eq!(editor.crop().expect("a frame").stage.angle, STEP);
        let before = draft();
        assert_eq!(before["fields"]["angle"], json!(STEP));

        let log = crate::app::testing::attach_log(&mut editor);
        let reset = angle_message(&editor, |action, parameter| ControlMessage::ResetField {
            action,
            parameter,
        });
        let _ = editor.update(reset);
        assert_eq!(editor.crop().expect("still drafting").stage.angle, 0.0);
        assert_eq!(section_angle(&editor), json!("0.0"));
        let after = draft();
        assert_eq!(after["fields"]["angle"], json!(0.0));
        assert_eq!(
            after["draft_revision"].as_u64(),
            before["draft_revision"]
                .as_u64()
                .map(|revision| revision + 1),
            "one change reached the core draft"
        );
        let changes: Vec<Value> = crate::app::testing::logged(&mut editor, &log)
            .into_iter()
            .filter(|record| record["event"] == "crop_draft_changed")
            .collect();
        assert_eq!(changes.len(), 1, "one draft change");
        assert_eq!(changes[0]["detail"]["angle"], json!(0.0));
        assert_eq!(
            editor
                .document
                .state
                .as_ref()
                .expect("a photograph")
                .revision,
            revision,
            "nothing committed"
        );
        finish(editor, catalog);
    }

    /// The angle's box shows the angle with its unit until it is pressed; a submitted number closes
    /// it again, and text that is not a number keeps it open for correcting.
    #[test]
    fn the_angle_box_opens_for_typing_and_closes_on_a_submitted_number() {
        let (mut editor, catalog, _, _) =
            opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
        let frame = crop_frame(&editor.modules).expect("a crop frame");
        let key = (frame.action.to_owned(), frame.angle.to_owned());
        assert!(!editor.editing_angle());
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        }));
        assert!(editor.editing_angle());
        submit_angle(&mut editor, "two");
        assert!(editor.editing_angle(), "invalid text stays open");
        // The box says why, as every number field does, and so does the status line.
        assert_eq!(
            angle_control(&editor).invalid.as_deref(),
            Some("angle must be a number from -45 to 45")
        );
        assert_eq!(editor.status.text, "angle must be a number from -45 to 45");
        submit_angle(&mut editor, "46");
        assert!(editor.editing_angle(), "an angle out of range stays open");
        assert_eq!(editor.crop().expect("a draft").stage.angle, 0.0);
        submit_angle(&mut editor, "3.5");
        assert!(!editor.editing_angle());
        assert_eq!(editor.crop().expect("a draft").stage.angle, 3.5);
        finish(editor, catalog);
    }

    #[test]
    fn a_stack_without_a_crop_layer_drafts_a_neutral_crop_at_the_end() {
        let (mut editor, catalog, _, _) =
            opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let draft = editor.crop().expect("an opened draft");
        assert!(draft.layer.is_none());
        assert_eq!(draft.layer_index, 1, "the whole stack is the input stage");
        assert_eq!(
            (draft.stage.width, draft.stage.height),
            CROP_SOURCE,
            "the stack's output stage, which no row reports"
        );
        assert_eq!(draft.payload(), CropPayload::NEUTRAL);
        finish(editor, catalog);
    }

    /// A crop layer whose row carries no readable values is never silently replaced by a neutral
    /// crop: the start is refused before any draft begins, and says why.
    #[test]
    fn an_unreadable_crop_payload_refuses_the_draft_and_keeps_the_layer() {
        let mut broken = crop_layer(CropPayload::NEUTRAL);
        broken.payload = json!({"angle":"sideways"});
        let (mut editor, catalog, _, _) = opened(vec![broken], 1);
        let task = editor.dispatch(Message::Crop(CropMessage::Start));
        assert_eq!(task.units(), 0, "nothing was sent");
        assert!(editor.gesture.is_none(), "no draft began");
        assert!(
            editor.crop().is_none(),
            "no neutral crop replaced the layer"
        );
        assert_eq!(editor.sync.mode, None, "the mode did not change");
        assert!(
            editor.status.text.contains("cannot be read"),
            "{}",
            editor.status.text
        );
        finish(editor, catalog);
    }

    #[test]
    fn every_draft_change_is_reachable_as_a_message() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 3);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
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

        // The angle's stepper: its box, its buttons and the arrow keys, each held key a live move
        // that its release makes one change. Shift moves ten steps and Option the fine step.
        submit_angle(&mut editor, "11.5");
        assert_eq!(editor.crop().expect("a draft").stage.angle, 11.5);
        step_angle(&mut editor, -1);
        assert_eq!(editor.crop().expect("a draft").stage.angle, 11.0);
        assert_eq!(section_angle(&editor), json!("11.0"));
        for (shift, option, expected) in [(true, false, 6.0), (false, true, 5.95)] {
            let key = angle_message(&editor, |action, parameter| ControlMessage::KeyNudge {
                action,
                parameter,
                direction: -1,
                shift,
                option,
            });
            let _ = editor.update(key);
            release_angle(&mut editor);
            let angle = editor.crop().expect("a draft").stage.angle;
            assert!((angle - expected).abs() < 1e-9, "{angle} is not {expected}");
        }
        assert_eq!(section_angle(&editor), json!("5.95"));
        submit_angle(&mut editor, "sideways");
        assert!((editor.crop().expect("a draft").stage.angle - 5.95).abs() < 1e-9);
        assert!(
            editor.status.text.contains("angle must be a number"),
            "{}",
            editor.status.text
        );
        submit_angle(&mut editor, "0");

        // Ratio presets, swap, lock and the custom extents, all by index into the declared list.
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("16:9"))));
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
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("custom"))));
        assert_eq!(editor.crop().expect("a draft").aspect.ratio(), Some(1.25));
        let _ = editor.update(Message::Crop(CropMessage::CustomHeight("none".into())));
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("1:1"))));
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("custom"))));
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
            assert_eq!(editor.crop_section.option, read);
        }
        let _ = editor.update(Message::Crop(CropMessage::Space(true)));
        assert!(editor.crop_section.space);
        let _ = editor.update(Message::Crop(CropMessage::Guide(true)));
        assert!(editor.crop_section.guide);

        // Cancel is the one draft lifecycle's, as it is for every gesture.
        draft_message(&mut editor, DraftMessage::Cancel);
        assert!(editor.crop().is_none());
        assert!(editor.presentation.presenter.stage().is_none());
        assert!(
            !editor.crop_section.guide,
            "cancelling leaves no guide mode on"
        );
        let summary = editor.snapshot()["crop"].clone();
        assert_eq!(summary["drafting"], json!(false));
        // The idle section is back, reading the stack the draft never committed to.
        assert_eq!(
            summary["section"],
            json!({"drafting":false,"enabled":true,"chosen":"Free","locked":false,"can_swap":false,"angle":"0.0","rail":0.0,"guide":false})
        );
        finish(editor, catalog);
    }

    /// Every change's end sets the payload's declared fields on the core draft, which `session.state`
    /// reports; Apply commits that draft against the revision it is based on, and Copy as JSON
    /// request offers the `edit.crop` request an independent client would send for the same frame.
    #[test]
    fn apply_commits_the_core_draft_the_frame_has_set() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
        let log = crate::app::testing::attach_log(&mut editor);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
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
        let set = editor
            .session
            .draft
            .clone()
            .expect("the session holds the draft");
        let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
        let payload = editor.crop().expect("a draft").payload();
        let held = editor
            .session
            .draft
            .clone()
            .expect("the session holds the draft");
        assert_eq!(
            held.draft_revision,
            set.draft_revision + 1,
            "a drag is one draft.set, at its end"
        );
        assert_eq!(held.action, "crop");
        assert_eq!(held.fields["angle"], json!(payload.angle));
        assert_eq!(held.fields["x"], json!(payload.x));
        assert_eq!(held.fields["width"], json!(payload.width));
        assert!(
            held.fields.get("aspect").is_none(),
            "only the declared five"
        );
        assert_eq!(editor.snapshot()["draft"]["fields"], json!(held.fields));

        let (method, request) = editor
            .crop_copy_request()
            .expect("a request")
            .expect("a valid draft");
        assert_eq!(method, "edit.crop");
        assert_eq!(request["asset_id"], json!(asset));
        assert_eq!(request["mutation"]["expected_revision"], json!(6));
        assert_eq!(request["angle"], json!(payload.angle));

        draft_message(&mut editor, DraftMessage::Commit);
        assert_eq!(
            core_draft(&editor).and_then(CoreDraft::in_flight),
            Some(Round::Commit)
        );
        let records = crate::app::testing::logged(&mut editor, &log);
        let commits = crate::app::testing::draft_events(&records, "crop_draft_commit");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["expected_revision"], json!(6));
        assert_eq!(
            crate::app::testing::draft_events(&records, "crop_draft_set").len(),
            2,
            "the opened frame and the drag"
        );
        finish(editor, catalog);
    }

    /// Another client's commit conflicts the draft, which the one Changed elsewhere notice says;
    /// the shared Reapply rebases the frame at once, onto the stage the new rows report, and the
    /// core draft through `draft.reapply`, in the same update.
    #[test]
    fn an_external_commit_marks_the_draft_conflicted_and_reapply_rebases_it() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
        submit_angle(&mut editor, "6");
        let composed = editor.crop().expect("a draft").rect;

        // Somebody else committed: the draft survives and says so, and Apply is refused.
        committed_elsewhere(&mut editor, &asset, &entry(&asset, 9, None));
        assert!(core_draft(&editor).expect("the draft is kept").conflicted);
        assert_eq!(
            editor.crop().expect("the draft is kept").rect,
            composed,
            "the composition is untouched"
        );
        assert_eq!(
            editor.release_refusal().as_deref(),
            Some("Changed elsewhere: discard the crop draft or reapply it"),
            "Apply is refused"
        );
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["crop"]["conflicted"], json!(true));
        assert_eq!(
            snapshot["notices"],
            json!(["Changed elsewhere"]),
            "the captured frame records the chrome it drew"
        );
        assert_eq!(snapshot["render_error"], json!(null));
        assert_eq!(snapshot["compare"], json!(false));

        // Reapply re-reads the rows, rebases the frame onto the new input stage and the core draft
        // onto the new revision, sends the rebased frame's fields, all in this update, and asks
        // for the new stage's pixels.
        editor.busy = false;
        let log = crate::app::testing::attach_log(&mut editor);
        draft_message(&mut editor, DraftMessage::Reapply);
        assert_eq!(
            editor.crop_stage(),
            Some(StageView::Rendering {
                reapply: true,
                base_revision: 9
            })
        );
        let draft = core_draft(&editor).expect("the rebased draft");
        assert!(!draft.conflicted && draft.drained());
        assert_eq!(draft.base_revision, 9);
        let payload = editor.crop().expect("a frame").payload();
        let records = crate::app::testing::logged(&mut editor, &log);
        let sets = crate::app::testing::draft_events(&records, "crop_draft_set");
        assert_eq!(
            sets.len(),
            1,
            "one draft.set, of the rebased frame, follows the rebase: {sets:?}"
        );
        assert_eq!(sets[0]["fields"]["width"], json!(payload.width));
        assert_eq!(
            editor.crop().expect("a frame").stage.angle,
            6.0,
            "the angle survives a rebase"
        );
        assert!(
            editor.release_refusal().is_none(),
            "Apply is possible again"
        );
        finish(editor, catalog);
    }

    /// Against a real owner: the open crop draft is this client's core draft, so `session.state`
    /// reports it with the payload's declared fields as each change ends; another client can
    /// neither see nor commit it; Apply commits it through `draft.commit` as one entry on the crop
    /// layer, and Cancel ends it with nothing committed.
    #[test]
    fn the_crop_draft_is_the_clients_core_draft_in_its_session() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-crop-{}-{}.sqlite",
            std::process::id(),
            crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let (mut editor, _, agent) = crate::app::testing::real_photo(&catalog);
        let owner = editor.owner.clone();
        let client = editor.client;
        let session = |client| {
            crate::app::tasks::call(&owner, client, "session.state", json!({}))
                .expect("a session")
                .0
        };
        let revision = editor
            .document
            .state
            .as_ref()
            .expect("a photograph")
            .revision;

        let _ = editor.update(Message::Crop(CropMessage::Start));
        step_angle(&mut editor, 1);
        let payload = editor.crop().expect("an open frame").payload();
        let held = session(client);
        assert_eq!(held["draft"]["action"], json!("crop"));
        assert_eq!(held["draft"]["base_revision"], json!(revision));
        assert_eq!(held["draft"]["conflicted"], json!(false));
        assert_eq!(held["draft"]["fields"]["angle"], json!(payload.angle));
        assert_eq!(held["draft"]["fields"]["width"], json!(payload.width));
        assert_eq!(
            held["draft"]["draft_revision"],
            json!(2),
            "the opened frame and the nudge"
        );
        assert_eq!(
            session(agent)["draft"],
            Value::Null,
            "a draft is one client's"
        );
        let foreign = crate::app::tasks::call(
            &owner,
            agent,
            "draft.commit",
            json!({"draft_id": held["draft"]["draft_id"], "mutation": mutation(revision)}),
        );
        assert!(
            foreign.is_err_and(|error| error.contains("unknown draft")),
            "another client cannot commit it"
        );

        draft_message(&mut editor, DraftMessage::Commit);
        assert!(crate::app::testing::run_commit(&mut editor));
        assert!(editor.gesture.is_none() && editor.crop().is_none());
        assert_eq!(session(client)["draft"], Value::Null);
        let state = editor.document.state.as_ref().expect("a photograph");
        assert_eq!(state.revision, revision + 1, "one entry");
        let layer = state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == luxforge_core::CROP_EFFECT)
            .expect("the crop layer");
        assert_eq!(
            serde_json::from_value::<CropPayload>(layer.payload.clone()).expect("a payload"),
            payload
        );

        // A second draft, opened on the committed crop's row and discarded: nothing commits and
        // the session holds no draft afterwards.
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert_eq!(
            editor.crop().expect("a second frame").payload(),
            payload,
            "the frame reopens at what was committed"
        );
        assert_eq!(session(client)["draft"]["action"], json!("crop"));
        draft_message(&mut editor, DraftMessage::Cancel);
        assert!(editor.gesture.is_none());
        assert_eq!(session(client)["draft"], Value::Null);
        assert_eq!(
            editor
                .document
                .state
                .as_ref()
                .expect("a photograph")
                .revision,
            revision + 1
        );
        finish(editor, catalog);
    }

    /// A crop's input stage is everything ahead of it, the transforms included: a draft on a stack
    /// turned after it was cropped opens on the turned stage the crop's row reports, after the
    /// orientation layer the host keeps ahead of the crop, and remembers the turn the row says it
    /// opened behind.
    #[test]
    fn a_draft_opens_behind_every_transform_ahead_of_its_crop() {
        let right = Orientation::NEUTRAL.then(luxforge_core::Transform::RotateRight);
        let crop = crop_layer(CropPayload {
            angle: 0.0,
            x: 0.25,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        });
        let (mut editor, catalog, _, _) = opened(
            vec![
                luxforge_core::Layer::pixel(0, 0, [1, 2, 3]),
                luxforge_core::Layer::orientation(right),
                crop.clone(),
            ],
            3,
        );
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.layer, Some(crop.id));
        assert_eq!(
            draft.layer_index, 2,
            "the pixel edit and the turn are shown"
        );
        assert_eq!(draft.ahead, right);
        assert_eq!(
            (draft.stage.width, draft.stage.height),
            (CROP_SOURCE.1, CROP_SOURCE.0)
        );
        finish(editor, catalog);
    }

    /// Without a crop layer the draft shows the stage the host would give a new one, which is
    /// before any finish layer: a post-crop effect acts on the crop's output, not on its input.
    #[test]
    fn a_first_draft_stops_before_a_finish_layer() {
        let finish_layer = luxforge_core::Layer {
            id: LayerId::new(),
            effect_id: luxforge_core::VIGNETTE_EFFECT.into(),
            effect_format: 1,
            payload: json!({"amount": -40}),
            artifacts: Vec::new(),
            mask: None,
        };
        let (mut editor, catalog, _, _) = opened(
            vec![luxforge_core::Layer::pixel(0, 0, [1, 2, 3]), finish_layer],
            2,
        );
        let finishing = luxforge_core::ModuleDescriptor {
            id: "luxforge.vignette".into(),
            effects: vec![luxforge_core::EffectDescriptor {
                id: luxforge_core::VIGNETTE_EFFECT.into(),
                format: 1,
                stage: luxforge_core::EffectStage::Finish,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
                sources: Vec::new(),
            }],
            ..luxforge_core::ModuleDescriptor::default()
        };
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
            crate::app::testing::crop_descriptor(),
            finishing,
        ]))));
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.layer, None);
        assert_eq!(
            draft.layer_index, 1,
            "the vignette is not part of the input"
        );
        finish(editor, catalog);
    }

    /// A turn committed while drafting, here as another client would commit it, goes ahead of the
    /// crop and turns its input stage. Reapply carries the draft through the turn the rows report,
    /// so the frame keeps selecting what it did instead of landing on whatever the same box numbers
    /// now show.
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
        open_crop(&mut editor);
        let before = editor.crop().expect("a draft").rect;
        assert_eq!(
            (before.x, before.y, before.width, before.height),
            (0.0, 0.0, 240.0, 160.0)
        );

        let right = Orientation::NEUTRAL.then(luxforge_core::Transform::RotateRight);
        let mut newer = entry(&asset, 5, None);
        for layer in [
            luxforge_core::Layer::orientation(right),
            luxforge_core::Layer {
                payload: serde_json::to_value(
                    serde_json::from_value::<CropPayload>(crop.payload.clone())
                        .unwrap()
                        .carried(CROP_SOURCE, right)
                        .unwrap(),
                )
                .unwrap(),
                ..crop.clone()
            },
        ] {
            newer.snapshot = newer.snapshot.append(layer).expect("a valid stack");
        }
        committed_elsewhere(&mut editor, &asset, &newer);
        assert!(core_draft(&editor).expect("the draft is kept").conflicted);

        editor.busy = false;
        draft_message(&mut editor, DraftMessage::Reapply);
        let draft = editor.crop().expect("the rebased frame");
        assert_eq!((draft.layer_index, draft.ahead), (1, right));
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
        assert!(!core_draft(&editor).expect("the rebased draft").conflicted);
        finish(editor, catalog);
    }

    #[test]
    fn the_drafts_own_apply_ends_it_and_a_failed_apply_keeps_it() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
        // A stale revision comes back as a conflict: the draft is kept and marked.
        draft_message(&mut editor, DraftMessage::Commit);
        answer_commit(&mut editor, Err("conflict: stale revision".into()));
        assert!(editor.crop().is_some(), "the draft is kept");
        assert!(core_draft(&editor).expect("the draft is kept").conflicted);
        assert!(editor.release_refusal().is_some());

        // The draft's own successful Apply ends it and drops the extra texture.
        editor.core_gesture_mut().expect("a draft").draft.conflicted = false;
        draft_message(&mut editor, DraftMessage::Commit);
        let newer = entry(&asset, 5, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        answer_commit(&mut editor, Ok(Some(refresh)));
        assert!(editor.crop().is_none() && editor.gesture.is_none());
        assert!(editor.session.draft.is_none());
        assert!(editor.presentation.presenter.stage().is_none());
        assert!(
            editor.status.text.contains("Crop applied"),
            "{}",
            editor.status.text
        );
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
            editor.sync.mode.as_deref(),
            Some(crop_id.as_str()),
            "starting a draft by any route queues the session's own mode change"
        );
        // The public entry point folds that into the returned task and consumes the flag.
        editor.session.workspace.mode = crop_id.clone();
        let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
        assert_eq!(editor.sync.mode, None, "the wrapper always consumes it");

        open_crop(&mut editor);
        assert!(editor.workspace.canvas.modes[0].id == POINTER_MODE);
        assert!(
            !editor.workspace.canvas.modes[0].selected,
            "pointer is not selected while the session reports the crop mode"
        );

        // Ending it, by Cancel here (Apply and a scripted cancel go through the same `end_crop_view`),
        // returns the session to pointer.
        let _ = editor.dispatch(Message::Draft(DraftMessage::Cancel));
        assert_eq!(
            editor.sync.mode.as_deref(),
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
            assert_eq!(editor.sync.mode, None, "{case}: no mode change is queued");
            assert!(editor.crop().is_none());
            assert_eq!(editor.session.workspace.mode, mode, "{case}");
        };

        // A historical preview cannot be edited.
        let current = editor.session.preview.selection.clone();
        editor.session.preview.selection = HistorySelection::Entry(entry_id);
        refused(&mut editor, "a historical preview");
        editor.session.preview.selection = current;

        // Nothing refuses it: the draft starts and asks the session for the mode once.
        let task = editor.dispatch(Message::View(ViewMessage::SetMode(crop_id.clone())));
        assert_eq!(
            task.units(),
            1,
            "its input stage's truncated preview: the draft's begin answered in this update"
        );
        assert!(editor.crop().is_some());
        assert_eq!(editor.sync.mode.as_deref(), Some(crop_id.as_str()));
        finish(editor, catalog);
    }

    #[test]
    fn a_history_preview_pauses_the_draft_without_discarding_it() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
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
        draft_message(&mut editor, DraftMessage::Commit);
        assert!(editor.crop().is_some());
        assert_eq!(editor.status.text, "Return to the current state to apply");
        assert_eq!(
            core_draft(&editor).and_then(CoreDraft::in_flight),
            None,
            "nothing was committed"
        );
        assert_eq!(editor.crop().expect("a draft").rect, composed);
        finish(editor, catalog);
    }

    /// Apply reads enabled exactly when the app would commit: the draft bar and the crop section
    /// both show the app's one release refusal, whatever it is — none, another request in flight,
    /// a historical preview on screen or a conflicted draft — so neither can read enabled while
    /// Enter or the button's own press would be refused.
    #[test]
    fn apply_reads_enabled_exactly_when_the_app_would_commit() {
        let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        open_crop(&mut editor);
        let agree = |editor: &mut Editor, case: &str| -> Option<String> {
            let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
            let refusal = editor.release_refusal();
            let bar = editor
                .workspace
                .canvas
                .draft_bar
                .clone()
                .expect("the draft bar");
            assert_eq!(bar.apply_reason, refusal, "{case}: the bar's reason");
            assert_eq!(bar.can_apply, refusal.is_none(), "{case}: the bar's Apply");
            let section = editor
                .workspace
                .tools
                .all()
                .flat_map(|section| section.controls.iter())
                .find_map(|control| match control {
                    crate::state::tools::ControlModel::CropFrame(model) => Some(model.can_apply),
                    _ => None,
                })
                .expect("the crop section");
            assert_eq!(section, refusal.is_none(), "{case}: the section's Apply");
            refusal
        };
        assert_eq!(agree(&mut editor, "a fresh draft"), None);

        editor.busy = true;
        assert_eq!(
            agree(&mut editor, "a request in flight").as_deref(),
            Some("Waiting for the last request")
        );
        draft_message(&mut editor, DraftMessage::Commit);
        assert_eq!(
            core_draft(&editor).and_then(CoreDraft::in_flight),
            None,
            "the refused Apply sent nothing"
        );
        editor.busy = false;

        let current = editor.session.preview.selection.clone();
        editor.session.preview.selection = HistorySelection::Entry(entry_id);
        assert_eq!(
            agree(&mut editor, "a historical preview").as_deref(),
            Some("Return to the current state to apply")
        );
        editor.session.preview.selection = current;

        committed_elsewhere(&mut editor, &asset, &entry(&asset, 4, None));
        assert_eq!(
            agree(&mut editor, "a conflicted draft").as_deref(),
            Some("Changed elsewhere: discard the crop draft or reapply it")
        );
        finish(editor, catalog);
    }

    /// The committed 16:9 crop the idle tests start from, fitted on [`stage`] by the core's own
    /// geometry exactly as `crop-fit` or the 16:9 chip would fit it.
    fn committed_wide() -> CropPayload {
        let stage = stage();
        let whole = luxforge_core::BoxRect {
            x: 0.0,
            y: 0.0,
            width: 480.0,
            height: 320.0,
        };
        stage
            .fit_about_center(luxforge_core::largest_with_ratio_inside(whole, 16.0 / 9.0))
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
    /// does, and applies itself at once: the canvas enters crop mode with the frame at the new
    /// ratio, the log shows the seeded draft and then the one change, and nothing commits.
    #[test]
    fn an_idle_change_opens_the_draft_seeded_from_the_committed_crop_and_applies_it() {
        let crop = crop_layer(committed_wide());
        let (mut editor, catalog, _, _) = opened(vec![crop.clone()], 5);
        let log = crate::app::testing::attach_log(&mut editor);
        let _ = editor.dispatch(Message::Crop(CropMessage::Preset(option("1:1"))));
        let draft = editor.crop().expect("the change opened a draft");
        assert_eq!(draft.layer, Some(crop.id));
        assert_eq!(draft.preset, "1:1");
        assert_eq!(draft.aspect.ratio(), Some(1.0));
        assert_eq!(draft.rect.width, draft.rect.height);
        assert_eq!(
            editor.sync.mode.as_deref(),
            Some("luxforge.crop"),
            "the canvas enters crop mode"
        );
        assert_eq!(
            editor.document.state.as_ref().expect("a state").revision,
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

    /// The draft is live before its stage's pixels arrive: changes made then — a ratio, a nudge and
    /// a rail drag — apply to the frame at once, each one draft change, with no queue between
    /// them and the frame; the stage then arrives under the frame they made.
    #[test]
    fn a_change_made_before_the_stage_arrives_applies_at_once() {
        let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
        let log = crate::app::testing::attach_log(&mut editor);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert_eq!(section_angle(&editor), json!("0.0"), "the committed angle");
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("4:3"))));
        step_angle(&mut editor, 1);
        assert_eq!(editor.crop().expect("an open frame").stage.angle, 0.5);
        assert_eq!(section_angle(&editor), json!("0.5"));
        drag_angle(&mut editor, &[0.52, 0.55, 0.5 + 2.4 / 90.0 + 1e-4]);
        release_angle(&mut editor);
        let draft = editor.crop().expect("an open frame");
        assert_eq!(draft.preset, "4:3");
        assert_eq!(draft.stage.angle, 2.4, "the rail's last position");
        assert_eq!(section_angle(&editor), json!("2.4"));
        assert!(
            matches!(editor.crop_stage(), Some(StageView::Rendering { .. })),
            "the stage is still on its way"
        );
        open_crop(&mut editor);
        assert_eq!(editor.crop().expect("an open frame").stage.angle, 2.4);
        assert_eq!(editor.document.state.as_ref().expect("a state").revision, 5);
        let records = crate::app::testing::logged(&mut editor, &log);
        assert_eq!(
            events(&records, "crop_draft_changed").len(),
            3,
            "the ratio, the nudge and the rail's release"
        );
        finish(editor, catalog);
    }

    /// Cancel on a draft whose stage has not yet rendered cancels it outright: the frame leaves,
    /// the stage's job is stopped, the session returns to pointer, and the core draft is cancelled,
    /// all in the update of the press.
    #[test]
    fn cancel_while_the_stage_is_rendering_cancels_it_outright() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert!(matches!(
            editor.crop_stage(),
            Some(StageView::Rendering { .. })
        ));
        let _ = editor.dispatch(Message::Draft(DraftMessage::Cancel));
        assert!(editor.crop().is_none(), "the frame left at once");
        assert!(editor.gesture.is_none(), "and its core draft with it");
        assert_eq!(editor.draft_generation(), None);
        assert_eq!(editor.sync.mode.as_deref(), Some(POINTER_MODE));
        assert_eq!(editor.status.text, "Crop draft discarded");
        assert_eq!(editor.document.state.as_ref().expect("a state").revision, 2);
        finish(editor, catalog);
    }

    /// What an idle control does that is not a change: the committed angle submitted again only
    /// closes the box, text that is not a number stays open with its reason, a rail release with no
    /// drag does nothing, and pressing the chip already chosen opens the draft without refitting
    /// the committed rectangle to it. An open slider gesture refuses the draft, and a start whose
    /// input stage fails ends with the change it made.
    #[test]
    fn an_idle_control_that_changes_nothing_opens_no_draft_or_leaves_the_crop_as_it_is() {
        let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
        let frame = crop_frame(&editor.modules).expect("a crop frame");
        let key = (frame.action.to_owned(), frame.angle.to_owned());
        editor.controls.fields.set(&key.0, &key.1, "31".into());
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        }));
        assert_eq!(
            editor.controls.fields.get(&key.0, &key.1),
            Some("0.0"),
            "the idle box opens at the committed angle"
        );
        let _ = editor.update(Message::Control(ControlMessage::Submit {
            action: key.0.clone(),
            parameter: Some(key.1.clone()),
        }));
        assert!(editor.gesture.is_none() && !editor.editing_angle());
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: key.0.clone(),
            parameter: key.1.clone(),
        }));
        submit_angle(&mut editor, "level");
        assert!(editor.gesture.is_none() && editor.editing_angle());
        assert!(
            editor.status.text.contains("angle must be a number"),
            "{}",
            editor.status.text
        );
        // A release with no drag, and a double-click reset of an angle already at its default of
        // 0, change nothing, so nothing opens: the reset needs no case of its own.
        release_angle(&mut editor);
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: key.0,
            parameter: key.1,
        }));
        let _ = editor.update(Message::Crop(CropMessage::Guide(false)));
        assert!(editor.gesture.is_none());

        // A slider gesture is finished deliberately, never displaced by the crop draft.
        crate::app::testing::hold_slider(&mut editor, "set-basic", "exposure");
        let _ = editor.update(Message::Crop(CropMessage::Lock));
        assert!(editor.crop().is_none());
        assert!(
            editor.status.text.contains("slider draft"),
            "{}",
            editor.status.text
        );
        editor.gesture = None;

        // A start whose input stage cannot be prepared ends, taking the change it made with it.
        let _ = editor.update(Message::Crop(CropMessage::Swap));
        assert!(editor.crop().is_some(), "the swap opened a draft");
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Open,
            Err("the source is gone".into()),
        )));
        assert!(editor.crop().is_none(), "nothing is left open");
        assert_eq!(editor.status.text, "the source is gone");
        assert!(
            editor.gesture.is_none(),
            "its core draft is cancelled in the same update"
        );

        // The chip already chosen opens the draft and leaves the committed rectangle exactly.
        let _ = editor.update(Message::Crop(CropMessage::Preset(option("16:9"))));
        let draft = editor.crop().expect("an opened draft");
        assert_eq!(draft.preset, "16:9");
        assert_eq!(draft.payload(), committed_wide());
        finish(editor, catalog);
    }

    /// At Fit a crop draft's input stage is the layer prefix's display-size proxy, rendered alone:
    /// its exact phase is never rendered there. A percentage zoom that draws the stage at its own
    /// size asks for the exact stage once, and a zoom back to Fit hands the held proxy over again
    /// without a render, exactly as the photograph's proxy and exact frames behave.
    #[test]
    fn the_input_stage_is_a_proxy_at_fit_and_exact_only_at_a_percentage_zoom() {
        use luxforge_core::Zoom;
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-crop-stage-proxy-{}-{}.sqlite",
            std::process::id(),
            crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
        // A photo surface smaller than the 480 × 320 photograph, so Fit draws it smaller than it is.
        editor.session.workspace.state_panel = false;
        editor.session.workspace.tools_panel = false;
        editor.view_state.window = (360.0, 300.0);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let count = editor.crop().expect("a frame").layer_index;
        let input = editor.crop().expect("a frame").stage;
        let job = editor
            .owner
            .preview_job(
                luxforge_core::PreviewRequest::new(editor.client, asset.clone()).layers(count),
            )
            .expect("the input stage's job");
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Open,
            Ok(Box::new(job)),
        )));
        let shown = |editor: &mut Editor, phase: &str| -> Value {
            luxforge_testbase::wait_until(&format!("the {phase} stage"), || {
                let _ = editor.update(Message::Preview(
                    crate::app::message::preview::PreviewMessage::Poll,
                ));
                editor.snapshot()["crop"]["input_stage_frame"]["phase"] == json!(phase)
            });
            editor.snapshot()["crop"]["input_stage_frame"].clone()
        };

        let fit = shown(&mut editor, "proxy");
        assert_eq!(editor.crop_stage(), Some(StageView::Shown));
        let bounds = editor.crop_stage_bounds().expect("Fit bounds the stage");
        let (width, height) = (
            fit["size"][0].as_u64().unwrap() as u32,
            fit["size"][1].as_u64().unwrap() as u32,
        );
        assert!(
            width < input.width && height < input.height,
            "a {width}x{height} proxy of the {}x{} stage",
            input.width,
            input.height
        );
        assert!(width <= bounds.width && height <= bounds.height);
        assert_eq!(fit["held_exact"], json!(false), "no exact phase at Fit");
        assert_eq!(
            editor.draft_generation(),
            None,
            "nothing more is on its way"
        );

        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        let plan = editor.zoom_changed(&Zoom::Fit);
        assert_eq!(plan.units(), 1, "the stage is planned again");
        let _ = editor.update(stage_replanned(&editor, &asset, count).0);
        assert!(
            editor.draft_generation().is_some(),
            "the exact stage is asked for"
        );
        let exact = shown(&mut editor, "exact");
        assert_eq!(exact["size"], json!([input.width, input.height]));
        assert_eq!(editor.draft_generation(), None);

        editor.session.preview.view.zoom = Zoom::Fit;
        let plan = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
        assert_eq!(plan.units(), 0, "nothing is planned");
        let back = editor.snapshot()["crop"]["input_stage_frame"].clone();
        assert_eq!(back["phase"], json!("proxy"), "the held proxy, at once");
        assert_eq!(back["size"], fit["size"]);
        assert_eq!(editor.draft_generation(), None, "nothing is rendered");
        finish(editor, catalog);
    }

    /// The owner task's answer to the plan a zoom asked for ([`Editor::present_crop_stage`]): the
    /// stage on screen planned again from its entry, over a new allocation of its pixels, and a
    /// handle that says whether anything still holds them.
    fn stage_replanned(
        editor: &Editor,
        asset: &luxforge_core::AssetId,
        count: usize,
    ) -> (Message, std::sync::Weak<Vec<u8>>) {
        let crop = editor.crop_gesture().expect("a draft");
        let entry = crop
            .frames
            .planned
            .as_ref()
            .expect("a planned stage")
            .entry_id
            .clone();
        let job = crate::app::tasks::crop_preview(
            &editor.owner,
            editor.client,
            asset.clone(),
            Some(entry.clone()),
            count,
        )
        .expect("the stage planned again");
        let (evaluation, pixels) = crate::app::testing::fresh_stack(&job.evaluation);
        let job = luxforge_core::PreviewJob { evaluation, ..job };
        (
            Message::Crop(CropMessage::PreviewReady(
                StagePlan::Zoom(entry),
                Ok(Box::new(job)),
            )),
            pixels,
        )
    }

    /// An open crop draft holds frames only. Once its input stage is delivered nothing of the
    /// planned stack is left on the desktop: a RAW development's planes would hold the source
    /// worker's memory gate, so a development the owner needs would wait on the draft. A zoom that
    /// needs a phase no held frame serves plans the stage again, once, from the entry it was
    /// planned from; its answer is requested only while the view still wants that phase, and an
    /// answer to no plan the draft is waiting for is dropped.
    #[test]
    fn an_open_crop_draft_keeps_no_stack_once_its_stage_is_delivered() {
        use crate::app::message::preview::PreviewMessage;
        use luxforge_core::Zoom;
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-crop-stage-stack-{}-{}.sqlite",
            std::process::id(),
            crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
        editor.session.workspace.state_panel = false;
        editor.session.workspace.tools_panel = false;
        editor.view_state.window = (360.0, 300.0);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let count = editor.crop().expect("a frame").layer_index;
        let job = editor
            .owner
            .preview_job(
                luxforge_core::PreviewRequest::new(editor.client, asset.clone()).layers(count),
            )
            .expect("the input stage's job");
        let (evaluation, pixels) = crate::app::testing::fresh_stack(&job.evaluation);
        let job = Box::new(luxforge_core::PreviewJob { evaluation, ..job });
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Open,
            Ok(job),
        )));
        let phase =
            |editor: &Editor| editor.snapshot()["crop"]["input_stage_frame"]["phase"].clone();
        let settled = |editor: &mut Editor, wanted: &str| {
            luxforge_testbase::wait_until(&format!("the {wanted} stage"), || {
                let _ = editor.update(Message::Preview(PreviewMessage::Poll));
                phase(editor) == json!(wanted) && !editor.presentation.queue.is_busy()
            });
        };
        settled(&mut editor, "proxy");
        assert_eq!(
            pixels.strong_count(),
            0,
            "the draft keeps its stage's frame, not its stack"
        );

        // A zoom that needs the exact stage plans it again, once, and asks for no render yet.
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1, "one plan");
        assert!(editor.crop_gesture().expect("a draft").frames.replanning);
        editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
        let again = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
        assert_eq!(again.units(), 0, "a plan is already on its way");
        assert_eq!(editor.draft_generation(), None);

        // The zoom moved back to Fit before the answer: the held proxy serves, and the answer is
        // dropped without a render.
        editor.session.preview.view.zoom = Zoom::Fit;
        assert_eq!(
            editor.zoom_changed(&Zoom::Percent { value: 200.0 }).units(),
            0
        );
        let (answer, pixels) = stage_replanned(&editor, &asset, count);
        let _ = editor.update(answer);
        assert_eq!(editor.draft_generation(), None, "nothing is rendered");
        assert!(!editor.crop_gesture().expect("a draft").frames.replanning);
        assert_eq!(phase(&editor), json!("proxy"));
        assert_eq!(pixels.strong_count(), 0, "a dropped answer is not kept");

        // An answer to another entry's plan is not this draft's; one for this entry that is not
        // the stage on screen is said and not rendered.
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1);
        let (answer, _) = stage_replanned(&editor, &asset, count);
        let Message::Crop(CropMessage::PreviewReady(StagePlan::Zoom(entry), Ok(job))) = answer
        else {
            unreachable!()
        };
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Zoom(luxforge_core::EntryId::new()),
            Ok(job.clone()),
        )));
        assert!(
            editor.crop_gesture().expect("a draft").frames.replanning,
            "still waiting for its own plan"
        );
        let other = luxforge_core::PreviewJob {
            layer_count: Some(count + 1),
            ..*job
        };
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Zoom(entry),
            Ok(Box::new(other)),
        )));
        assert_eq!(editor.draft_generation(), None, "nothing is rendered");
        assert!(!editor.crop_gesture().expect("a draft").frames.replanning);
        assert_eq!(
            editor.status.text,
            "The crop's input stage planned again is not the one on screen"
        );

        // The next zoom plans it again, and its answer is requested as a start's is and shown.
        editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
        assert_eq!(
            editor.zoom_changed(&Zoom::Percent { value: 100.0 }).units(),
            1
        );
        let (answer, pixels) = stage_replanned(&editor, &asset, count);
        let _ = editor.update(answer);
        assert!(
            editor.draft_generation().is_some(),
            "the exact stage is asked for"
        );
        settled(&mut editor, "exact");
        // The worker releases the stack with its job. With no layer ahead of the crop the exact
        // stage is the source's own pixels, shared by the frame the draft holds and the one on
        // screen, and by nothing else; a frame holds no plane lease.
        let exact = editor.crop_gesture().expect("a draft").frames.exact.clone();
        let exact = exact.expect("the exact stage is held");
        let shared = usize::from(std::ptr::eq(
            std::sync::Arc::as_ptr(&exact.rgba),
            pixels.as_ptr(),
        ));
        drop(exact);
        assert_eq!(pixels.strong_count(), 2 * shared, "only the frames hold it");

        // Once the draft has ended its frames are gone, and an answer finds nothing to show.
        editor.session.preview.view.zoom = Zoom::Fit;
        assert_eq!(
            editor.zoom_changed(&Zoom::Percent { value: 200.0 }).units(),
            0
        );
        let (answer, dropped) = stage_replanned(&editor, &asset, count);
        draft_message(&mut editor, DraftMessage::Cancel);
        assert!(editor.crop_gesture().is_none());
        assert_eq!(pixels.strong_count(), 0, "the frames end with the draft");
        let _ = editor.update(answer);
        assert_eq!(editor.draft_generation(), None, "nothing is rendered");
        assert!(editor.presentation.presenter.stage().is_none());
        assert_eq!(dropped.strong_count(), 0, "a dropped answer is not kept");
        finish(editor, catalog);
    }

    /// With the real owner and a real RAW, while a crop draft is open over its input stage — the
    /// proxy at Fit, the exact stage planned again for 100%, and the proxy again at Fit — another
    /// client's white balance and another photograph each prepare their development within a
    /// bounded wait. Each runs on a thread of its own, so a development that waits on planes the
    /// draft holds fails the test instead of hanging it.
    ///
    /// `LUXFORGE_RAW_FIXTURE=/path/to/file.NEF cargo test --release -p luxforge-app --bin luxforge \
    ///   a_raw_develops_again_while_a_crop_draft_is_open -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a private RAW fixture: set LUXFORGE_RAW_FIXTURE"]
    fn a_raw_develops_again_while_a_crop_draft_is_open() {
        use crate::app::{
            message::preview::PreviewMessage,
            tasks::{self, Scope},
        };
        use luxforge_core::Zoom;
        use std::time::{Duration, Instant};
        /// Far above a release development of any supported camera, far below a hang.
        const DEADLINE: Duration = Duration::from_secs(60);
        let raw = std::path::PathBuf::from(
            std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"),
        );
        let catalog =
            std::env::temp_dir().join(format!("luxforge-crop-gate-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&catalog);
        let (mut editor, asset, other) = crate::app::testing::real_photo_at(&catalog, &raw);
        let (owner, client) = (editor.owner.clone(), editor.client);
        fn bounded<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
            let started = Instant::now();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(work());
            });
            let done = receiver.recv_timeout(DEADLINE).unwrap_or_else(|_| {
                panic!("{what}: the development did not finish in {DEADLINE:?}")
            });
            eprintln!("{what}: {:?}", started.elapsed());
            done
        }
        let phase =
            |editor: &Editor| editor.snapshot()["crop"]["input_stage_frame"]["phase"].clone();
        let settled = |editor: &mut Editor, wanted: &str| {
            luxforge_testbase::wait_until(&format!("the {wanted} stage"), || {
                let _ = editor.update(Message::Preview(PreviewMessage::Poll));
                phase(editor) == json!(wanted) && !editor.presentation.queue.is_busy()
            });
        };
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let count = editor.crop().expect("a frame").layer_index;
        let plan = |entry: Option<luxforge_core::EntryId>| {
            let (owner, asset) = (owner.clone(), asset.clone());
            bounded("the input stage's plan", move || {
                tasks::crop_preview(&owner, client, asset, entry, count)
            })
            .map(Box::new)
        };
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Open,
            plan(None),
        )));
        settled(&mut editor, "proxy");
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1, "planned again");
        let entry = editor
            .crop_gesture()
            .expect("a draft")
            .frames
            .planned
            .as_ref();
        let entry = entry.expect("a planned stage").entry_id.clone();
        let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
            StagePlan::Zoom(entry.clone()),
            plan(Some(entry)),
        )));
        settled(&mut editor, "exact");
        editor.session.preview.view.zoom = Zoom::Fit;
        assert_eq!(
            editor.zoom_changed(&Zoom::Percent { value: 100.0 }).units(),
            0
        );
        assert_eq!(phase(&editor), json!("proxy"));

        // Another client's white balance, read back as the desktop reads it: its refresh plans the
        // new entry's preview and waits for the development it needs.
        let revision = editor
            .document
            .state
            .as_ref()
            .expect("an open photo")
            .revision;
        tasks::call(
            &owner,
            other,
            "edit.set-raw",
            json!({"asset_id": asset, "temperature": 3500.0,
                   "mutation": tasks::mutation(revision)}),
        )
        .expect("another client's white balance");
        let refresh = {
            let (owner, asset) = (owner.clone(), asset.clone());
            bounded("another client's white balance", move || {
                tasks::refresh(&owner, client, asset, Scope::Elsewhere, None)
            })
            .expect("the refresh")
        };
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
        assert!(editor.crop().is_some(), "the draft is kept");

        // Another photograph: its original's preparation retains no development of this one.
        let jpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg");
        let (queued, _) = tasks::call(
            &owner,
            client,
            "catalog.import",
            json!({"path": jpeg, "mutation": tasks::request()}),
        )
        .expect("an import");
        let job = queued["job_id"].as_str().expect("a source job").to_owned();
        let import_owner = owner.clone();
        bounded("another photograph", move || {
            tasks::wait_source_job(&import_owner, client, &job)
        })
        .expect("another photograph prepares");
        assert!(editor.crop().is_some(), "the draft is still open");
        finish(editor, catalog);
    }
}
