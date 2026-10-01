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
        gesture::{Kind, Starting},
        message::{Message, control::ControlMessage, crop::CropMessage, crop::CropPointer},
        outcome::{Outcome, Requested},
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
                        self.outcome(Outcome::CropStage);
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
                        self.crop_section.guide = false;
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
        // Keep the exact disclosure choices, including sections still following their defaults.
        // Repeated starts and Reapply never replace this snapshot; every exit restores it once.
        self.crop_section.previous_expanded = Some(self.controls.expanded.clone());
        for module in &self.modules {
            self.controls.expanded.insert(module.id.clone(), false);
        }
        if let Some(frame) = crop_frame(&self.modules) {
            self.controls.expanded.insert(frame.module.id.clone(), true);
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
        let focus = operation::snap_to(
            crate::view::tools_panel::scroll_id(),
            iced::widget::scrollable::RelativeOffset { x: 0.0, y: 0.0 },
        );
        Task::batch([begin, fields, stage, focus])
    }

    /// The Changed elsewhere notice's Reapply for the crop draft: the frame is rebased at once onto
    /// the stage the current rows report — carried through a turn committed ahead of it, and on a
    /// stage of another size keeping its composition as far as it fits — the core draft is rebased
    /// by `draft.reapply` and the rebased frame's fields follow it, both in this update, and the new
    /// stage's pixels are asked for.
    pub(crate) fn crop_reapply(&mut self) -> Task<Message> {
        if self.busy || !self.at_current() || self.crop().is_none() {
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
        self.at_current() && &state.current_entry.id == entry && state.revision == base_revision
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
        self.report_crop_stage();
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
        self.at_current()
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
            self.outcome(Outcome::CropStage);
            return (true, Task::none());
        }
        self.crop_stage_shown();
        self.report_crop_stage();
        (true, self.present_crop_stage())
    }

    /// Put the input-stage frame the view wants on screen: the one the draft already holds, or
    /// else one plan of the stage once no stage job or plan is on its way — the one on its way
    /// lands first and this runs again. Answers every zoom while the stage owns the view and every
    /// stage frame, so a zoom that changed while the stage rendered is picked up. The stage is
    /// planned again from the entry it was planned from, as a start plans it
    /// ([`crop_preview_task`]), and its answer ([`Self::crop_stage_replanned`]) requests it, and
    /// the frame it asks for is reported.
    pub(crate) fn present_crop_stage(&mut self) -> Task<Message> {
        if !self.show_held_crop_stage() {
            return Task::none();
        }
        self.outcome(Outcome::FrameRequested(Requested::CropStage));
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
            self.outcome(Outcome::CropStage);
            return false;
        }
        if self.draft_generation().is_some() || !self.show_held_crop_stage() {
            self.outcome(Outcome::CropStage);
            return false;
        }
        let bounded = self.crop_stage_bounds().is_some();
        self.event("crop_stage_requested", || json!({ "bounded": bounded }));
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

    /// Report the crop draft's input stage once it is on screen, so the frame can be read over it.
    /// A Reapply's `draft.reapply` answers in the update that sends it, so the stage it reports
    /// never shows the frame rebased and the draft still conflicted.
    pub(crate) fn report_crop_stage(&mut self) {
        let ready = self.core_gesture().is_some_and(|gesture| {
            gesture
                .crop()
                .is_some_and(|crop| crop.stage == StageView::Shown)
        });
        if ready {
            self.outcome(Outcome::CropStage);
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
        self.show_crop_angle();
        self.event(event, || {
            self.core_gesture()
                .and_then(|gesture| Some(gesture.crop()?.summary(&gesture.draft)))
                .unwrap_or(Value::Null)
        });
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
        if let Some(expanded) = self.crop_section.previous_expanded.take() {
            self.controls.expanded = expanded;
        }
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
            self.event("crop_draft_discarded", || crop.summary(draft));
            self.status.text = "Crop draft discarded".into();
        }
    }

    /// The crop gesture ended without a draft: its `draft.begin` was refused.
    pub(crate) fn crop_ended(&mut self) {
        self.end_crop_view(None);
        self.outcome(Outcome::CropStage);
    }

    /// The crop draft's `draft.commit` answered with an entry, or with none because the frame is
    /// the committed crop. Either way the draft is over and the committed photograph returns.
    pub(crate) fn crop_committed(
        &mut self,
        crop: &CropGesture,
        draft: &CoreDraft,
        outcome: Option<Refresh>,
    ) -> Task<Message> {
        self.end_crop_view(crop.generation);
        let Some(refresh) = outcome else {
            self.status.text = "Crop unchanged; nothing was committed".into();
            self.event("crop_draft_noop", || crop.summary(draft));
            return match self.reseed_committed() {
                Some(task) => task,
                None => {
                    self.outcome(Outcome::NoNewFrame);
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
            || json!({"request_id":request_id,"entry_id":entry.as_str(),"revision":revision,"draft":crop.summary(draft)}),
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
#[path = "crop_tests.rs"]
mod tests;
