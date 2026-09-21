//! Evidence mode: import queued files in order, capture a frame after each outcome, run any script
//! steps with a frame each, then exit. Every step goes through the same messages and owner calls the
//! controls use, so a script proves the real paths rather than a parallel implementation.
use crate::{
    app::{
        Editor,
        fields::number_text,
        message::{CropMessage, CropPointer, Message, PaletteAction},
        tasks::{mutation, workspace_task},
    },
    crop_draft::{Corner, Handle},
    state::tools::crop_frame,
};
use iced::Task;
use serde_json::{Map, Value, json};
use std::{collections::VecDeque, path::PathBuf, time::Duration};

/// An evidence run that has not finished by then is stuck; exit so the harness reaps nothing.
pub(crate) const EVIDENCE_DEADLINE: Duration = Duration::from_secs(25);

/// The most steps one evidence run accepts, so a script cannot outlive the evidence deadline
/// unnoticed.
const MAX_SCRIPT_STEPS: usize = 64;

pub(crate) struct Evidence {
    pub(crate) dir: PathBuf,
    pub(crate) queue: VecDeque<PathBuf>,
    /// How many files were queued, so script frames are numbered after the open frames.
    pub(crate) opens: u64,
    /// Steps still to run, in order.
    pub(crate) script: VecDeque<Step>,
    /// The one-based index of the running step; zero while the opens are still going.
    pub(crate) step: u64,
    /// What the running step waits for before its frame is captured.
    pub(crate) awaiting: Option<Settle>,
    /// The running step's record, written into its frame and into `result.json`.
    pub(crate) current: Option<Value>,
    /// Every step record in order, successes and failures alike.
    pub(crate) steps: Vec<Value>,
    pub(crate) frames: Vec<Value>,
    pub(crate) capture_pending: bool,
    pub(crate) saving: bool,
    pub(crate) had_errors: bool,
}

/// One step of an evidence script. Steps run in order after the last `--open` outcome, each followed
/// by exactly one captured frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// One owner request. The desktop fills `asset_id` and the mutation envelope itself, so a script
    /// never carries a revision or a request id.
    Api {
        method: String,
        params: Map<String, Value>,
    },
    Draft(DraftStep),
    /// One slider gesture on a generated control: the exact messages a drag sends.
    Slider(SliderStep),
    /// What one generated field is typed into, and whether Enter is pressed in it.
    Field(FieldStep),
    /// A module or group reset, through the control that declares it.
    Reset(ResetStep),
    /// The decision an open slider draft's Changed elsewhere notice offers.
    SliderDraft(SliderDraftStep),
    View(ViewStep),
    Workspace(WorkspaceStep),
    Preview(PreviewStep),
    Palette(PaletteStep),
    /// Move the pointer to one pixel of the displayed raster, exactly as the canvas reports a
    /// hover, and wait for the readout `render.sample` answers with.
    Hover {
        x: u32,
        y: u32,
    },
}

/// One slider gesture. Every value becomes one `SliderMoved` with a tick between them, exactly as
/// a pointer drag and the gated subscription produce them; the gesture then ends the way `end`
/// says, or stays open when it says nothing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) values: Vec<f64>,
    pub(crate) end: SliderEnd,
}

/// How a scripted gesture ends: released (committed), Escape (cancelled), or left open so the
/// frame shows the draft mid-gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliderEnd {
    Release,
    Cancel,
    Open,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldStep {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) text: String,
    /// Enter in the field, which commits that one field without a draft.
    pub(crate) submit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResetStep {
    pub(crate) module: String,
    /// A control group's label; without one the module's own header reset runs.
    pub(crate) group: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliderDraftStep {
    Discard,
    Reapply,
}

/// Any of the panels, mode or thirds; every field is optional, exactly as `workspace.set` takes
/// them. At least one field is required.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WorkspaceStep {
    pub(crate) state_panel: Option<bool>,
    pub(crate) tools_panel: Option<bool>,
    pub(crate) mode: Option<String>,
    pub(crate) thirds: Option<bool>,
    /// The two clipping overlays, per-client view state like every other field here.
    pub(crate) clip_shadows: Option<bool>,
    pub(crate) clip_highlights: Option<bool>,
}

/// Select a loaded history entry by its sequence number, or return to the current state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PreviewStep {
    Sequence(u64),
    Current,
}

/// Open the command palette with this query, or open it, run the query and run its first match.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteStep {
    Query(String),
    Run(String),
}

/// One crop-draft change, each mapped to the [`CropMessage`] the panel or the canvas would send.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DraftStep {
    Start,
    Reapply,
    Angle(f64),
    Nudge(f64),
    /// A declared `aspect` option, by name; an undeclared one fails the step.
    Preset(String),
    /// A rectangle in box pixels, applied as two corner gestures.
    Rect([f64; 4]),
    Swap,
    Lock,
    Option(bool),
    Guide(bool),
    Apply,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ViewStep {
    Fit,
    Percent(f32),
}

impl Step {
    /// The step as the script wrote it, recorded beside the frame it produced.
    pub(crate) fn record(&self) -> Value {
        match self {
            Self::Api { method, params } => json!({"api":{"method":method,"params":params}}),
            Self::Draft(draft) => json!({"draft":draft.record()}),
            Self::Slider(slider) => json!({"slider":{
                "action": slider.action,
                "parameter": slider.parameter,
                "values": slider.values,
                "release": slider.end == SliderEnd::Release,
                "cancel": slider.end == SliderEnd::Cancel,
            }}),
            Self::Field(field) => json!({"field":{
                "action": field.action,
                "parameter": field.parameter,
                "text": field.text,
                "submit": field.submit,
            }}),
            Self::Reset(reset) => match &reset.group {
                Some(group) => json!({"reset":{"module":reset.module,"group":group}}),
                None => json!({"reset":{"module":reset.module}}),
            },
            Self::SliderDraft(SliderDraftStep::Discard) => json!({"slider_draft":"discard"}),
            Self::SliderDraft(SliderDraftStep::Reapply) => json!({"slider_draft":"reapply"}),
            Self::View(ViewStep::Fit) => json!({"view":{"zoom":"fit"}}),
            Self::View(ViewStep::Percent(value)) => json!({"view":{"zoom":value}}),
            Self::Workspace(workspace) => json!({"workspace":workspace.record()}),
            Self::Preview(PreviewStep::Sequence(sequence)) => {
                json!({"preview":{"sequence":sequence}})
            }
            Self::Preview(PreviewStep::Current) => json!({"preview":"current"}),
            Self::Palette(PaletteStep::Query(query)) => json!({"palette":{"query":query}}),
            Self::Palette(PaletteStep::Run(query)) => json!({"palette":{"run":query}}),
            Self::Hover { x, y } => json!({"hover":{"x":x,"y":y}}),
        }
    }
}

impl WorkspaceStep {
    fn record(&self) -> Value {
        let mut object = Map::new();
        if let Some(value) = self.state_panel {
            object.insert("state_panel".into(), Value::from(value));
        }
        if let Some(value) = self.tools_panel {
            object.insert("tools_panel".into(), Value::from(value));
        }
        if let Some(mode) = &self.mode {
            object.insert("mode".into(), Value::from(mode.clone()));
        }
        if let Some(value) = self.thirds {
            object.insert("thirds".into(), Value::from(value));
        }
        if let Some(value) = self.clip_shadows {
            object.insert("clip_shadows".into(), Value::from(value));
        }
        if let Some(value) = self.clip_highlights {
            object.insert("clip_highlights".into(), Value::from(value));
        }
        Value::Object(object)
    }
}

impl DraftStep {
    fn record(&self) -> Value {
        match self {
            Self::Start => json!({"start":true}),
            Self::Reapply => json!({"reapply":true}),
            Self::Angle(value) => json!({"angle":value}),
            Self::Nudge(value) => json!({"nudge":value}),
            Self::Preset(option) => json!({"preset":option}),
            Self::Rect(rect) => json!({"rect":rect}),
            Self::Swap => json!({"swap":true}),
            Self::Lock => json!({"lock":true}),
            Self::Option(on) => json!({"option":on}),
            Self::Guide(on) => json!({"guide":on}),
            Self::Apply => json!({"apply":true}),
            Self::Cancel => json!({"cancel":true}),
        }
    }
}

/// What the running script step waits for before its frame is captured. A request step waits for
/// the ordinary `render_ready` outcome instead, which is the same correlation an `--open` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settle {
    /// The crop layer's truncated preview must reach the GPU and open the draft.
    Draft,
    /// One session round trip, for a view or workspace change.
    Session,
    /// A history or current-state selection's pixels must reach the GPU.
    Preview,
    /// An open slider gesture must have drained: the preview on screen is the one rendered from
    /// its newest settings, with nothing in flight and nothing waiting. A refused or conflicted
    /// gesture settles here too, because its frame is the evidence of the refusal.
    SliderDraft,
    /// A clipping overlay was switched on: its own bounded texture must reach the GPU before the
    /// frame is captured, or the capture would show the photograph without the mask.
    Overlay,
    /// The pointer readout must come back from `render.sample`.
    Readout,
}

impl Editor {
    /// Run the next script step, or finish the run when the script is exhausted. One step is in
    /// flight at a time and every step ends in exactly one captured frame.
    pub(crate) fn next_step(&mut self) -> Task<Message> {
        let Some(step) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.script.pop_front())
        else {
            return self.finish_evidence();
        };
        let record = {
            let evidence = self.evidence.as_mut().expect("evidence mode");
            evidence.step += 1;
            evidence.awaiting = None;
            let record = json!({"step":evidence.step,"status":"sent","request":step.record()});
            evidence.current = Some(record.clone());
            record
        };
        self.event("script_step", record);
        match step {
            Step::Api { method, params } => self.api_step(method, params),
            Step::Draft(draft) => self.draft_step(draft),
            Step::Slider(slider) => self.slider_step(slider),
            Step::Field(field) => self.field_step(field),
            Step::Reset(reset) => self.reset_step(reset),
            Step::SliderDraft(decision) => self.slider_draft_step(decision),
            Step::View(view) => self.view_step(view),
            Step::Workspace(workspace) => self.workspace_step(workspace),
            Step::Preview(preview) => self.preview_step(preview),
            Step::Palette(palette) => self.palette_step(palette),
            Step::Hover { x, y } => self.hover_step(x, y),
        }
    }

    /// One owner request with the desktop's own envelope: the current revision and a fresh request
    /// id, exactly as a control would send it. The frame is captured when its pixels arrive.
    fn api_step(&mut self, method: String, params: Map<String, Value>) -> Task<Message> {
        let Some((asset, revision)) = self
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
        else {
            return self.fail_step("no photograph is open");
        };
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        let mutation = mutation(revision);
        self.note_step(
            json!({"expected_revision":mutation.expected_revision,"request_id":mutation.request_id}),
        );
        let mut request = json!({"asset_id":asset,"mutation":mutation});
        request
            .as_object_mut()
            .expect("the envelope is an object")
            .extend(params);
        self.begin_request();
        self.command(method, request)
    }

    /// One crop-draft change through its own message, captured on the next rendered frame. Opening a
    /// draft waits for the truncated preview; Apply is a mutation and waits for its pixels.
    fn draft_step(&mut self, step: DraftStep) -> Task<Message> {
        let drafting = self.crop.is_some();
        let message = match &step {
            DraftStep::Start | DraftStep::Reapply => {
                if drafting == matches!(step, DraftStep::Start) {
                    return self.fail_step(if drafting {
                        "a draft is already open"
                    } else {
                        "no draft is open to reapply"
                    });
                }
                self.await_step(Settle::Draft);
                let task = self.crop_update(if matches!(step, DraftStep::Start) {
                    CropMessage::Start
                } else {
                    CropMessage::Reapply
                });
                // A refused start never reaches the draft, so the step would wait for a frame that
                // nothing arms; the preview request is the only thing that can settle it.
                if self.crop_pending.is_none() {
                    return self.fail_step("the draft could not be prepared");
                }
                return task;
            }
            DraftStep::Apply => {
                return match self.crop_apply() {
                    Ok(task) => {
                        self.begin_request();
                        task
                    }
                    Err(reason) => self.fail_step(reason),
                };
            }
            DraftStep::Rect(rect) => return self.rect_step(*rect),
            DraftStep::Option(on) => CropMessage::Option(*on),
            DraftStep::Guide(on) => CropMessage::Guide(*on),
            DraftStep::Angle(value) => CropMessage::AngleText(number_text(*value)),
            DraftStep::Nudge(value) => CropMessage::NudgeAngle(*value),
            DraftStep::Swap => CropMessage::Swap,
            DraftStep::Lock => CropMessage::Lock,
            DraftStep::Cancel => CropMessage::Cancel,
            DraftStep::Preset(option) => {
                let Some(index) = crop_frame(&self.modules)
                    .map(|frame| frame.presets())
                    .and_then(|presets| presets.iter().position(|preset| &preset.option == option))
                else {
                    return self
                        .fail_step(format!("no module declares the aspect option {option}"));
                };
                CropMessage::Preset(index)
            }
        };
        let modifier = matches!(step, DraftStep::Option(_) | DraftStep::Guide(_));
        if !drafting && !modifier {
            return self.fail_step("no crop draft is open");
        }
        // Setting the angle text does not change the draft; submitting it does, exactly as Enter in
        // the field does.
        let mut tasks = vec![self.crop_update(message)];
        if matches!(step, DraftStep::Angle(_)) {
            tasks.push(self.crop_update(CropMessage::SubmitAngle));
        }
        self.capture_next_frame();
        Task::batch(tasks)
    }

    /// A rectangle in box pixels, applied as two corner gestures: the top-left corner first, then the
    /// bottom-right, each a begin, a drag and an end exactly as the canvas publishes them.
    fn rect_step(&mut self, [x, y, width, height]: [f64; 4]) -> Task<Message> {
        if self.crop.is_none() {
            return self.fail_step("no crop draft is open");
        }
        let mut tasks = Vec::new();
        for (corner, target) in [
            (Corner::TopLeft, (x, y)),
            (Corner::BottomRight, (x + width, y + height)),
        ] {
            let Some(from) = self.crop.as_ref().map(|draft| corner.point(&draft.rect)) else {
                break;
            };
            let option = self.crop_option;
            for pointer in [
                CropPointer::Begin {
                    handle: Handle::Corner(corner),
                    x: from.0,
                    y: from.1,
                },
                CropPointer::Drag {
                    x: target.0,
                    y: target.1,
                    option,
                },
                CropPointer::End,
            ] {
                tasks.push(self.crop_update(CropMessage::Pointer(pointer)));
            }
        }
        self.capture_next_frame();
        Task::batch(tasks)
    }

    /// One slider gesture, driven as the exact messages a pointer drag produces: one `SliderMoved`
    /// per value with the gated tick between them, then the release, Escape or nothing at all.
    /// Nothing here reaches the owner directly; the gesture's own driver does, under its own bound.
    fn slider_step(&mut self, step: SliderStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if crate::state::tools::declared_action(&self.modules, &step.action).is_none() {
            return self.fail_step(format!("no module declares the action {}", step.action));
        }
        if step.values.is_empty() {
            return self.fail_step("a slider step needs at least one value");
        }
        let mut tasks = Vec::new();
        for value in &step.values {
            tasks.push(self.update(Message::SliderMoved {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                value: *value,
            }));
            tasks.push(self.update(Message::SliderDraftTick));
        }
        if self.slider_draft.is_none() && step.end != SliderEnd::Cancel {
            return self.fail_step(format!(
                "the {} draft could not be opened: {}",
                step.action, self.status
            ));
        }
        match step.end {
            // The committed pixels are the evidence, so this waits for the render the commit
            // produces; a return-to-start gesture settles the same step with no entry at all.
            SliderEnd::Release => {
                self.await_step(Settle::Preview);
                tasks.push(self.update(Message::SliderReleased {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                }));
            }
            // Escape, through the same message the keyboard table produces.
            SliderEnd::Cancel => {
                self.await_step(Settle::Preview);
                tasks.push(self.update(Message::SliderDraftCancel));
            }
            // Left open: the frame shows the drafted preview, captured once the gesture has
            // drained, so the pixels belong to the newest value it sent.
            SliderEnd::Open => self.await_step(Settle::SliderDraft),
        }
        Task::batch(tasks)
    }

    /// Type into one generated field and, when the step says so, press Enter in it, which commits
    /// that one field without a draft.
    fn field_step(&mut self, step: FieldStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if crate::state::tools::declared_action(&self.modules, &step.action)
            .and_then(|declared| declared.parameter(&step.parameter))
            .is_none()
        {
            return self.fail_step(format!(
                "{} declares no parameter {}",
                step.action, step.parameter
            ));
        }
        let mut tasks = vec![
            self.update(Message::EditValue {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
            }),
            self.update(Message::Field {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                text: step.text.clone(),
            }),
        ];
        if !step.submit {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.begin_request();
        tasks.push(self.update(Message::Submit {
            action: step.action,
            parameter: Some(step.parameter),
        }));
        if !self.busy {
            return self.fail_step(format!("the field was not submitted: {}", self.status));
        }
        Task::batch(tasks)
    }

    /// A module's header reset, or one control group's reset found by its declared label. Both run
    /// the action the descriptor declares, with its preset, exactly as the buttons do.
    fn reset_step(&mut self, step: ResetStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let Some(module) = crate::state::tools::module_of(&self.modules, &step.module) else {
            return self.fail_step(format!("no module is registered as {}", step.module));
        };
        let message = match &step.group {
            None => Message::ResetModule(step.module.clone()),
            Some(label) => {
                let Some(path) = group_path(&module.controls, label) else {
                    return self.fail_step(format!("{} declares no group {label}", step.module));
                };
                Message::ResetGroup {
                    module_id: step.module.clone(),
                    path,
                }
            }
        };
        self.begin_request();
        let task = self.update(message);
        if !self.busy {
            return self.fail_step(format!("the reset did not run: {}", self.status));
        }
        task
    }

    /// Answer an open slider draft's Changed elsewhere notice, through the same messages its two
    /// buttons raise.
    fn slider_draft_step(&mut self, step: SliderDraftStep) -> Task<Message> {
        if self.slider_draft.is_none() {
            return self.fail_step("no slider draft is open");
        }
        match step {
            SliderDraftStep::Discard => {
                self.await_step(Settle::Preview);
                self.update(Message::SliderDraftCancel)
            }
            SliderDraftStep::Reapply => {
                self.await_step(Settle::SliderDraft);
                self.update(Message::SliderDraftReapply)
            }
        }
    }

    /// A view change through the same session call the zoom controls make.
    fn view_step(&mut self, step: ViewStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        self.await_step(Settle::Session);
        match step {
            ViewStep::Fit => self.update(Message::Fit),
            ViewStep::Percent(value) => {
                self.zoom = number_text(f64::from(value));
                self.update(Message::ApplyZoom)
            }
        }
    }

    /// Any of the panels, the mode or the thirds overlay, sent as one `workspace.set` naming only
    /// the fields that actually differ from the session's own, exactly as `TogglePanel`, `SetMode`
    /// and `ToggleThirds` each already do for their one field. Captured on the session round trip.
    fn workspace_step(&mut self, step: WorkspaceStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mut diff = Map::new();
        let workspace = &self.session.workspace;
        if let Some(value) = step.state_panel
            && value != workspace.state_panel
        {
            diff.insert("state_panel".into(), Value::from(value));
        }
        if let Some(value) = step.tools_panel
            && value != workspace.tools_panel
        {
            diff.insert("tools_panel".into(), Value::from(value));
        }
        if let Some(value) = step.thirds
            && value != workspace.thirds
        {
            diff.insert("thirds".into(), Value::from(value));
        }
        if let Some(mode) = &step.mode
            && *mode != workspace.mode
        {
            diff.insert("mode".into(), Value::from(mode.clone()));
        }
        // Switching a clipping overlay on means a bounded derivation and an upload after the
        // session round trip, so the step waits for the mask's own pixels rather than for the
        // session, which would capture the photograph before the overlay reached it.
        let mut overlay = false;
        for (field, wanted, current) in [
            ("clip_shadows", step.clip_shadows, workspace.clip_shadows),
            (
                "clip_highlights",
                step.clip_highlights,
                workspace.clip_highlights,
            ),
        ] {
            if let Some(value) = wanted
                && value != current
            {
                diff.insert(field.into(), Value::from(value));
                overlay |= value;
            }
        }
        if diff.is_empty() {
            self.capture_next_frame();
            return Task::none();
        }
        self.await_step(if overlay {
            Settle::Overlay
        } else {
            Settle::Session
        });
        workspace_task(self.owner.clone(), self.client, Value::Object(diff))
    }

    /// Select a loaded history entry by sequence, or return to the current state, exactly as the
    /// state panel's rows and "Return to current" do. Captured once its pixels reach the GPU.
    fn preview_step(&mut self, step: PreviewStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        match step {
            PreviewStep::Current => {
                self.await_step(Settle::Preview);
                self.update(Message::ReturnCurrent)
            }
            PreviewStep::Sequence(sequence) => {
                let Some(entry_id) = self
                    .history
                    .entries
                    .iter()
                    .find(|entry| entry.sequence == sequence)
                    .map(|entry| entry.id.clone())
                else {
                    return self
                        .fail_step(format!("no loaded history entry has sequence {sequence}"));
                };
                self.await_step(Settle::Preview);
                self.update(Message::Preview(entry_id))
            }
        }
    }

    /// One pointer position over the photograph, published exactly as the canvas publishes a move,
    /// and captured once `render.sample` has answered with the three output codes under it.
    fn hover_step(&mut self, x: u32, y: u32) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.pointer == Some((x, y)) {
            // The pointer is already there, so no sample would be asked for and nothing would
            // settle the step; clearing it first makes the move a real one.
            let _ = self.update(Message::PointerMoved(None));
        }
        self.await_step(Settle::Readout);
        let task = self.update(Message::PointerMoved(Some((x, y))));
        if !self.sample_in_flight {
            return self.fail_step("the pointer readout could not be requested");
        }
        task
    }

    /// Open the palette, type the query, and either stop there or run the first match. The query
    /// step is captured on the next frame; a run settles the way its own entry would.
    fn palette_step(&mut self, step: PaletteStep) -> Task<Message> {
        let query = match &step {
            PaletteStep::Query(query) | PaletteStep::Run(query) => query.clone(),
        };
        let _ = self.update(Message::OpenPalette);
        let _ = self.update(Message::PaletteQuery(query.clone()));
        match step {
            PaletteStep::Query(_) => {
                self.capture_next_frame();
                Task::none()
            }
            PaletteStep::Run(_) => {
                let Some(action) = self
                    .workspace
                    .palette
                    .entries
                    .first()
                    .map(|entry| entry.action.clone())
                else {
                    return self.fail_step(format!("no palette entry matches {query:?}"));
                };
                self.arm_palette_settle(&action);
                self.dispatch(Message::PaletteRun)
            }
        }
    }

    /// What a palette entry settles on, matched to the same round trip its own message produces:
    /// a mutation waits for its pixels like an `api` step, a mode or panel change waits for the
    /// session, and returning to current waits for its own upload.
    fn arm_palette_settle(&mut self, action: &PaletteAction) {
        match action {
            PaletteAction::Run { .. }
            | PaletteAction::Undo
            | PaletteAction::Redo
            | PaletteAction::Restore => {
                self.begin_request();
            }
            PaletteAction::ReturnCurrent => self.await_step(Settle::Preview),
            PaletteAction::Mode(_)
            | PaletteAction::TogglePanel(_)
            | PaletteAction::ToggleThirds
            | PaletteAction::Fit
            | PaletteAction::HundredPercent => self.await_step(Settle::Session),
        }
    }

    /// The running step waits for this before its frame is captured.
    fn await_step(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence {
            evidence.awaiting = Some(settle);
        }
    }

    /// Something the running step was waiting for happened: capture its frame.
    pub(crate) fn settle_step(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence
            && evidence.awaiting == Some(settle)
        {
            evidence.awaiting = None;
            evidence.capture_pending = true;
        }
    }

    /// Capture the frame the next redraw presents. Used by the steps that only change draft state.
    fn capture_next_frame(&mut self) {
        if let Some(evidence) = &mut self.evidence {
            evidence.awaiting = None;
            evidence.capture_pending = true;
        }
    }

    /// Add detail to the running step's record.
    fn note_step(&mut self, detail: Value) {
        let Some(object) = detail.as_object() else {
            return;
        };
        if let Some(record) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.current.as_mut())
            .and_then(Value::as_object_mut)
        {
            record.extend(object.clone());
        }
    }

    /// The running step could not be sent. It is recorded and its frame is still captured, so a
    /// refused step is visible in the evidence rather than missing from it.
    fn fail_step(&mut self, reason: impl Into<String>) -> Task<Message> {
        let reason = reason.into();
        self.status = reason.clone();
        self.event("script_step_failed", json!({"reason":reason}));
        self.note_step(json!({"status":"failed","reason":reason}));
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors = true;
        }
        self.capture_next_frame();
        Task::none()
    }

    pub(crate) fn finish_evidence(&mut self) -> Task<Message> {
        self.event("shutdown", json!({}));
        let evidence = self.evidence.as_mut().expect("evidence mode");
        let dir = evidence.dir.clone();
        let frames = std::mem::take(&mut evidence.frames);
        let script = std::mem::take(&mut evidence.steps);
        let had_errors = evidence.had_errors;
        let result = json!({"run_id":self.run_id,"status":"captured","had_input_errors":had_errors,"frames":frames,"script":script,"elapsed_ms":self.started.elapsed().as_secs_f64()*1000.,"build_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH});
        let state = self.snapshot();
        let log = self.diagnostics.take();
        self.live_server.take();
        self.owner.disconnect(self.client);
        self.owner.stop();
        let join = self.owner_join.take();
        Task::perform(
            async move {
                if let Some(log) = log
                    && !log.finish()
                {
                    eprintln!("diagnostics: incomplete evidence log");
                    std::process::exit(4);
                }
                let write = || -> std::io::Result<()> {
                    std::fs::write(
                        dir.join("result.json"),
                        serde_json::to_vec_pretty(&result).expect("result is serializable"),
                    )?;
                    std::fs::write(
                        dir.join("state.json"),
                        serde_json::to_vec_pretty(&state).expect("state is serializable"),
                    )
                };
                if let Err(error) = write() {
                    eprintln!("Evidence finalize failed: {error}");
                    std::process::exit(4);
                }
                if let Some(join) = join {
                    let _ = join.join();
                }
            },
            |_| (),
        )
        .then(|_| iced::exit())
    }
}

/// Parse an evidence script: a JSON array of steps, each an object with exactly one of `api`,
/// `draft` or `view`. Parsing is strict and happens before the window opens, so a malformed script
/// fails the run instead of producing partial evidence.
pub(crate) fn parse_script(text: &str) -> Result<VecDeque<Step>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("evidence script is not JSON: {error}"))?;
    let steps = value
        .as_array()
        .ok_or("an evidence script is a JSON array of steps")?;
    if steps.len() > MAX_SCRIPT_STEPS {
        return Err(format!(
            "at most {MAX_SCRIPT_STEPS} evidence script steps are supported per run"
        ));
    }
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            parse_step(step).map_err(|error| format!("evidence script step {}: {error}", index + 1))
        })
        .collect()
}

/// The single key of a one-key object, or an error naming what was found instead.
fn sole(object: &Map<String, Value>) -> Result<(&str, &Value), String> {
    let mut entries = object.iter();
    match (entries.next(), entries.next()) {
        (Some((key, value)), None) => Ok((key.as_str(), value)),
        _ => Err(format!("expected exactly one key, found {}", object.len())),
    }
}

fn parse_step(step: &Value) -> Result<Step, String> {
    let object = step.as_object().ok_or(
        "a step is an object with one key: api, draft, slider, slider_draft, field, reset, view, workspace, preview, palette or hover",
    )?;
    let (kind, value) = sole(object)?;
    match kind {
        "api" => parse_api(value),
        "draft" => Ok(Step::Draft(parse_draft(value)?)),
        "slider" => Ok(Step::Slider(parse_slider(value)?)),
        "slider_draft" => Ok(Step::SliderDraft(parse_slider_draft(value)?)),
        "field" => Ok(Step::Field(parse_field_step(value)?)),
        "reset" => Ok(Step::Reset(parse_reset(value)?)),
        "view" => Ok(Step::View(parse_view(value)?)),
        "workspace" => Ok(Step::Workspace(parse_workspace(value)?)),
        "preview" => Ok(Step::Preview(parse_preview(value)?)),
        "palette" => Ok(Step::Palette(parse_palette(value)?)),
        "hover" => parse_hover(value),
        other => Err(format!(
            "unknown step kind {other}; expected api, draft, slider, slider_draft, field, reset, view, workspace, preview, palette or hover"
        )),
    }
}

/// The named string field of a step object, required and non-empty.
fn required_text(object: &Map<String, Value>, field: &str, step: &str) -> Result<String, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{step} needs a {field}"))
}

fn optional_flag(object: &Map<String, Value>, field: &str, step: &str) -> Result<bool, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{step} {field} takes true or false")),
    }
}

fn known_fields(object: &Map<String, Value>, allowed: &[&str], step: &str) -> Result<(), String> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown {step} field {key}"));
        }
    }
    Ok(())
}

fn parse_slider(value: &Value) -> Result<SliderStep, String> {
    let object = value
        .as_object()
        .ok_or("slider takes an object with an action, a parameter and values")?;
    known_fields(
        object,
        &["action", "parameter", "values", "release", "cancel"],
        "slider",
    )?;
    let values: Vec<f64> = object
        .get("values")
        .and_then(Value::as_array)
        .ok_or("slider needs a values array")?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| "slider values are finite numbers".to_owned())
        })
        .collect::<Result<_, _>>()?;
    if values.is_empty() {
        return Err("slider needs at least one value".into());
    }
    let (release, cancel) = (
        optional_flag(object, "release", "slider")?,
        optional_flag(object, "cancel", "slider")?,
    );
    if release && cancel {
        return Err("a slider step either releases or cancels, not both".into());
    }
    Ok(SliderStep {
        action: required_text(object, "action", "slider")?,
        parameter: required_text(object, "parameter", "slider")?,
        values,
        end: match (release, cancel) {
            (true, _) => SliderEnd::Release,
            (_, true) => SliderEnd::Cancel,
            _ => SliderEnd::Open,
        },
    })
}

fn parse_slider_draft(value: &Value) -> Result<SliderDraftStep, String> {
    match value.as_str().map(str::trim) {
        Some("discard") => Ok(SliderDraftStep::Discard),
        Some("reapply") => Ok(SliderDraftStep::Reapply),
        _ => Err("slider_draft takes \"discard\" or \"reapply\"".into()),
    }
}

fn parse_field_step(value: &Value) -> Result<FieldStep, String> {
    let object = value
        .as_object()
        .ok_or("field takes an object with an action, a parameter and text")?;
    known_fields(object, &["action", "parameter", "text", "submit"], "field")?;
    let text = match object.get("text") {
        Some(Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => return Err("field needs text".into()),
    };
    Ok(FieldStep {
        action: required_text(object, "action", "field")?,
        parameter: required_text(object, "parameter", "field")?,
        text,
        submit: optional_flag(object, "submit", "field")?,
    })
}

fn parse_reset(value: &Value) -> Result<ResetStep, String> {
    let object = value
        .as_object()
        .ok_or("reset takes an object with a module and an optional group")?;
    known_fields(object, &["module", "group"], "reset")?;
    Ok(ResetStep {
        module: required_text(object, "module", "reset")?,
        group: match object.get("group") {
            None | Some(Value::Null) => None,
            Some(_) => Some(required_text(object, "group", "reset")?),
        },
    })
}

/// Where a control group with this label sits inside a module's controls, as the position the
/// panel's own reset button names.
fn group_path(controls: &[lightwell_core::Control], label: &str) -> Option<Vec<usize>> {
    for (index, control) in controls.iter().enumerate() {
        let lightwell_core::Control::Group {
            label: declared,
            controls: children,
            ..
        } = control
        else {
            continue;
        };
        if declared == label {
            return Some(vec![index]);
        }
        if let Some(mut path) = group_path(children, label) {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

fn parse_api(value: &Value) -> Result<Step, String> {
    let object = value
        .as_object()
        .ok_or("api takes an object with a method and optional params")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "method" | "params") {
            return Err(format!("unknown api field {key}"));
        }
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| !method.trim().is_empty())
        .ok_or("api needs a method name")?;
    let params = match object.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => return Err("api params must be an object".into()),
    };
    for reserved in ["asset_id", "mutation"] {
        if params.contains_key(reserved) {
            return Err(format!(
                "the desktop fills {reserved} itself; a script may not set it"
            ));
        }
    }
    Ok(Step::Api {
        method: method.to_owned(),
        params,
    })
}

fn parse_draft(value: &Value) -> Result<DraftStep, String> {
    let object = value
        .as_object()
        .ok_or("draft takes an object with exactly one key")?;
    let (key, value) = sole(object)?;
    let number = || {
        value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("draft {key} takes a finite number"))
    };
    let requested = || match value.as_bool() {
        Some(true) => Ok(()),
        _ => Err(format!("draft {key} takes true")),
    };
    let flag = || {
        value
            .as_bool()
            .ok_or_else(|| format!("draft {key} takes true or false"))
    };
    match key {
        "start" => requested().map(|()| DraftStep::Start),
        "reapply" => requested().map(|()| DraftStep::Reapply),
        "apply" => requested().map(|()| DraftStep::Apply),
        "cancel" => requested().map(|()| DraftStep::Cancel),
        "swap" => requested().map(|()| DraftStep::Swap),
        "lock" => requested().map(|()| DraftStep::Lock),
        "angle" => number().map(DraftStep::Angle),
        "nudge" => number().map(DraftStep::Nudge),
        "option" => flag().map(DraftStep::Option),
        "guide" => flag().map(DraftStep::Guide),
        "preset" => value
            .as_str()
            .filter(|option| !option.trim().is_empty())
            .map(|option| DraftStep::Preset(option.to_owned()))
            .ok_or_else(|| "draft preset takes a declared aspect option".into()),
        "rect" => {
            let numbers: Vec<f64> = value
                .as_array()
                .ok_or("draft rect takes [x, y, width, height] in box pixels")?
                .iter()
                .filter_map(|number| number.as_f64().filter(|number| number.is_finite()))
                .collect();
            match (numbers.len(), numbers.get(2), numbers.get(3)) {
                (4, Some(width), Some(height)) if *width > 0.0 && *height > 0.0 => {
                    Ok(DraftStep::Rect([
                        numbers[0], numbers[1], numbers[2], numbers[3],
                    ]))
                }
                _ => Err(
                    "draft rect takes four finite numbers with positive extents, in box pixels"
                        .into(),
                ),
            }
        }
        other => Err(format!("unknown draft step {other}")),
    }
}

fn parse_view(value: &Value) -> Result<ViewStep, String> {
    let object = value
        .as_object()
        .ok_or("view takes an object with a zoom")?;
    let (key, value) = sole(object)?;
    if key != "zoom" {
        return Err(format!("unknown view field {key}; expected zoom"));
    }
    let percent = |value: f64| {
        (value.is_finite() && (10.0..=1600.0).contains(&value))
            .then_some(ViewStep::Percent(value as f32))
            .ok_or_else(|| "view zoom is \"fit\" or a percentage from 10 to 1600".to_owned())
    };
    match value {
        Value::String(text) if text.trim().eq_ignore_ascii_case("fit") => Ok(ViewStep::Fit),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| "view zoom is \"fit\" or a percentage from 10 to 1600".to_owned())
            .and_then(percent),
        Value::Number(number) => percent(number.as_f64().unwrap_or(f64::NAN)),
        _ => Err("view zoom is \"fit\" or a percentage from 10 to 1600".into()),
    }
}

fn parse_workspace(value: &Value) -> Result<WorkspaceStep, String> {
    let object = value.as_object().ok_or(
        "workspace takes an object with any of state_panel, tools_panel, mode, thirds, clip_shadows or clip_highlights",
    )?;
    let mut step = WorkspaceStep::default();
    let flag = |value: &Value, field: &str| {
        value
            .as_bool()
            .ok_or_else(|| format!("workspace {field} takes true or false"))
    };
    for (key, value) in object {
        match key.as_str() {
            "state_panel" => step.state_panel = Some(flag(value, "state_panel")?),
            "tools_panel" => step.tools_panel = Some(flag(value, "tools_panel")?),
            "thirds" => step.thirds = Some(flag(value, "thirds")?),
            "clip_shadows" => step.clip_shadows = Some(flag(value, "clip_shadows")?),
            "clip_highlights" => step.clip_highlights = Some(flag(value, "clip_highlights")?),
            "mode" => {
                step.mode = Some(
                    value
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                        .ok_or("workspace mode takes a module id or \"pointer\"")?
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown workspace field {other}")),
        }
    }
    if step == WorkspaceStep::default() {
        return Err(
            "workspace needs at least one of state_panel, tools_panel, mode, thirds, clip_shadows or clip_highlights"
                .into(),
        );
    }
    Ok(step)
}

/// `{"hover": {"x": N, "y": N}}`: one pixel of the displayed raster, both coordinates required.
fn parse_hover(value: &Value) -> Result<Step, String> {
    let object = value
        .as_object()
        .ok_or("hover takes an object with an x and a y")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "x" | "y") {
            return Err(format!("unknown hover field {key}"));
        }
    }
    let coordinate = |name: &str| -> Result<u32, String> {
        object
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("hover {name} takes a non-negative integer"))
    };
    Ok(Step::Hover {
        x: coordinate("x")?,
        y: coordinate("y")?,
    })
}

fn parse_preview(value: &Value) -> Result<PreviewStep, String> {
    match value {
        Value::String(text) if text.trim() == "current" => Ok(PreviewStep::Current),
        Value::Object(object) => {
            let (key, value) = sole(object)?;
            if key != "sequence" {
                return Err(format!("unknown preview field {key}; expected sequence"));
            }
            value
                .as_u64()
                .map(PreviewStep::Sequence)
                .ok_or_else(|| "preview sequence takes a non-negative integer".to_owned())
        }
        _ => Err("preview takes \"current\" or {\"sequence\": N}".into()),
    }
}

fn parse_palette(value: &Value) -> Result<PaletteStep, String> {
    let object = value
        .as_object()
        .ok_or("palette takes an object with a query or a run")?;
    let (key, value) = sole(object)?;
    let text = value
        .as_str()
        .ok_or_else(|| format!("palette {key} takes a string"))?
        .to_owned();
    match key {
        "query" => Ok(PaletteStep::Query(text)),
        "run" => Ok(PaletteStep::Run(text)),
        other => Err(format!(
            "unknown palette field {other}; expected query or run"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::{evidence, finish, scripted};
    use lightwell_core::CropStage;

    #[test]
    fn an_evidence_script_is_parsed_strictly_before_the_window_opens() {
        let steps = parse_script(
            r#"[{"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}},
                {"api":{"method":"history.undo"}},
                {"draft":{"start":true}},
                {"draft":{"angle":7.5}},
                {"draft":{"preset":"3:2"}},
                {"draft":{"rect":[10,20,300,200]}},
                {"draft":{"option":false}},
                {"draft":{"apply":true}},
                {"view":{"zoom":"fit"}},
                {"view":{"zoom":"100"}},
                {"view":{"zoom":250}},
                {"workspace":{"state_panel":false}},
                {"workspace":{"tools_panel":true,"thirds":true}},
                {"preview":{"sequence":0}},
                {"preview":"current"},
                {"palette":{"query":"rotate"}},
                {"palette":{"run":"rotate"}},
                {"workspace":{"clip_shadows":true,"clip_highlights":false}},
                {"hover":{"x":12,"y":34}}]"#,
        )
        .expect("a valid script");
        assert_eq!(steps.len(), 19);
        assert_eq!(
            steps[1],
            Step::Api {
                method: "history.undo".into(),
                params: Map::new()
            }
        );
        assert_eq!(steps[3], Step::Draft(DraftStep::Angle(7.5)));
        assert_eq!(
            steps[5],
            Step::Draft(DraftStep::Rect([10., 20., 300., 200.]))
        );
        assert_eq!(steps[8], Step::View(ViewStep::Fit));
        assert_eq!(steps[9], Step::View(ViewStep::Percent(100.0)));
        assert_eq!(
            steps[11],
            Step::Workspace(WorkspaceStep {
                state_panel: Some(false),
                ..WorkspaceStep::default()
            })
        );
        assert_eq!(
            steps[12],
            Step::Workspace(WorkspaceStep {
                tools_panel: Some(true),
                thirds: Some(true),
                ..WorkspaceStep::default()
            })
        );
        assert_eq!(steps[13], Step::Preview(PreviewStep::Sequence(0)));
        assert_eq!(steps[14], Step::Preview(PreviewStep::Current));
        assert_eq!(
            steps[15],
            Step::Palette(PaletteStep::Query("rotate".into()))
        );
        assert_eq!(steps[16], Step::Palette(PaletteStep::Run("rotate".into())));
        assert_eq!(
            steps[17],
            Step::Workspace(WorkspaceStep {
                clip_shadows: Some(true),
                clip_highlights: Some(false),
                ..WorkspaceStep::default()
            })
        );
        assert_eq!(steps[18], Step::Hover { x: 12, y: 34 });
        // Every record round-trips to the shape the script was written in.
        assert_eq!(steps[4].record(), json!({"draft":{"preset":"3:2"}}));
        assert_eq!(steps[0].record()["api"]["method"], json!("edit.crop-fit"));
        assert_eq!(
            steps[12].record(),
            json!({"workspace":{"tools_panel":true,"thirds":true}})
        );
        assert_eq!(steps[14].record(), json!({"preview":"current"}));
        assert_eq!(steps[16].record(), json!({"palette":{"run":"rotate"}}));
        assert_eq!(
            steps[17].record(),
            json!({"workspace":{"clip_shadows":true,"clip_highlights":false}})
        );
        assert_eq!(steps[18].record(), json!({"hover":{"x":12,"y":34}}));

        for (script, expected) in [
            ("{}", "array of steps"),
            ("[{}]", "exactly one key"),
            (r#"[{"api":{}}]"#, "needs a method name"),
            (r#"[{"api":{"method":"x","extra":1}}]"#, "unknown api field"),
            (
                r#"[{"api":{"method":"edit.crop","params":{"mutation":{}}}}]"#,
                "may not set it",
            ),
            (r#"[{"draft":{"start":false}}]"#, "takes true"),
            (r#"[{"draft":{"angle":"7"}}]"#, "finite number"),
            (r#"[{"draft":{"rect":[1,2,0,4]}}]"#, "positive extents"),
            (r#"[{"draft":{"nowhere":true}}]"#, "unknown draft step"),
            (r#"[{"view":{"zoom":2}}]"#, "10 to 1600"),
            (r#"[{"view":{"pan":1}}]"#, "unknown view field"),
            (r#"[{"workspace":{}}]"#, "at least one of"),
            (r#"[{"workspace":{"mode":""}}]"#, "module id"),
            (
                r#"[{"workspace":{"nowhere":true}}]"#,
                "unknown workspace field",
            ),
            (r#"[{"preview":{"sequence":-1}}]"#, "non-negative integer"),
            (r#"[{"preview":{"entry":1}}]"#, "unknown preview field"),
            (r#"[{"preview":true}]"#, "\"current\" or"),
            (r#"[{"palette":{"query":1}}]"#, "takes a string"),
            (r#"[{"palette":{"filter":"x"}}]"#, "unknown palette field"),
            (r#"[{"workspace":{"clip_shadows":1}}]"#, "true or false"),
            (r#"[{"hover":{"x":1}}]"#, "hover y takes"),
            (r#"[{"hover":{"x":-1,"y":2}}]"#, "hover x takes"),
            (r#"[{"hover":{"x":1,"y":2,"z":3}}]"#, "unknown hover field"),
            (r#"[{"hover":5}]"#, "an x and a y"),
            (r#"[{"zoom":"fit"}]"#, "unknown step kind"),
            ("not json", "not JSON"),
            (
                r#"[{"slider":{"parameter":"exposure","values":[1]}}]"#,
                "slider needs a action",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[]}}]"#,
                "at least one value",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":["1"]}}]"#,
                "finite numbers",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[1],"release":true,"cancel":true}}]"#,
                "not both",
            ),
            (
                r#"[{"slider":{"action":"a","parameter":"b","values":[1],"nowhere":true}}]"#,
                "unknown slider field",
            ),
            (r#"[{"slider_draft":"apply"}]"#, "discard"),
            (
                r#"[{"field":{"action":"a","parameter":"b"}}]"#,
                "field needs text",
            ),
            (
                r#"[{"field":{"action":"a","parameter":"b","text":"1","nowhere":1}}]"#,
                "unknown field field",
            ),
            (r#"[{"reset":{}}]"#, "reset needs a module"),
            (
                r#"[{"reset":{"module":"m","nowhere":1}}]"#,
                "unknown reset field",
            ),
        ] {
            let error = parse_script(script).expect_err(script);
            assert!(error.contains(expected), "{script}: {error}");
        }
        assert!(
            parse_script(&format!(
                "[{}]",
                vec![r#"{"draft":{"swap":true}}"#; 65].join(",")
            ))
            .expect_err("too many steps")
            .contains("at most")
        );
    }

    /// The gesture, field, reset and conflict-resolution steps parse into exactly the shapes the
    /// runner writes, and record themselves back in the same shape.
    #[test]
    fn the_slider_field_and_reset_steps_round_trip_their_scripts() {
        let steps = parse_script(
            r#"[{"slider":{"action":"set-basic","parameter":"exposure","values":[0.25,0.5,0.75],"release":true}},
                {"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"cancel":true}},
                {"slider":{"action":"set-basic","parameter":"exposure","values":[1.0]}},
                {"slider_draft":"discard"},
                {"slider_draft":"reapply"},
                {"field":{"action":"set-basic","parameter":"exposure","text":"1.5","submit":true}},
                {"reset":{"module":"lightwell.basic"}},
                {"reset":{"module":"lightwell.basic","group":"Tone"}}]"#,
        )
        .expect("a valid script");
        assert_eq!(steps.len(), 8);
        assert_eq!(
            steps[0],
            Step::Slider(SliderStep {
                action: "set-basic".into(),
                parameter: "exposure".into(),
                values: vec![0.25, 0.5, 0.75],
                end: SliderEnd::Release,
            })
        );
        assert_eq!(
            steps[1].record(),
            json!({"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":false,"cancel":true}})
        );
        assert!(matches!(
            steps[2],
            Step::Slider(SliderStep {
                end: SliderEnd::Open,
                ..
            })
        ));
        assert_eq!(steps[3], Step::SliderDraft(SliderDraftStep::Discard));
        assert_eq!(steps[4].record(), json!({"slider_draft":"reapply"}));
        assert_eq!(
            steps[5],
            Step::Field(FieldStep {
                action: "set-basic".into(),
                parameter: "exposure".into(),
                text: "1.5".into(),
                submit: true,
            })
        );
        assert_eq!(
            steps[6],
            Step::Reset(ResetStep {
                module: "lightwell.basic".into(),
                group: None,
            })
        );
        assert_eq!(
            steps[7].record(),
            json!({"reset":{"module":"lightwell.basic","group":"Tone"}})
        );
        // A group's position inside a module's controls is found by its declared label.
        let basic = lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .find(|module| module.id == "lightwell.basic")
            .expect("the Basic module is registered")
            .clone();
        assert_eq!(group_path(&basic.controls, "Tone"), Some(vec![1]));
        assert_eq!(group_path(&basic.controls, "White balance"), Some(vec![0]));
        assert_eq!(group_path(&basic.controls, "Nowhere"), None);
    }

    #[test]
    fn a_scripted_draft_change_records_its_step_and_arms_one_capture() {
        let (mut editor, catalog, _, _) = scripted(
            r#"[{"draft":{"start":true}},{"draft":{"rect":[20,10,200,150]}},{"draft":{"angle":9.0}},{"draft":{"preset":"1:1"}},{"draft":{"cancel":true}}]"#,
        );
        // Start waits for the truncated preview; nothing is captured until the draft opens.
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Draft));
        assert!(!evidence(&editor).capture_pending);
        editor.open_draft(CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        assert!(editor.crop.is_some());
        assert_eq!(evidence(&editor).awaiting, None);
        assert!(evidence(&editor).capture_pending);

        // Each later step is one message and one armed capture.
        for (step, check) in [
            (
                // Two corner gestures in Free mode reach the rectangle exactly.
                2u64,
                &(|editor: &Editor| {
                    let rect = editor.crop.as_ref().expect("a draft").rect;
                    assert_eq!((rect.x, rect.y), (20.0, 10.0));
                    assert_eq!((rect.width, rect.height), (200.0, 150.0));
                }) as &dyn Fn(&Editor),
            ),
            (3, &|editor: &Editor| {
                assert_eq!(editor.crop.as_ref().expect("a draft").stage.angle, 9.0);
                assert_eq!(editor.crop_angle, "9");
            }),
            (4, &|editor: &Editor| {
                let draft = editor.crop.as_ref().expect("a draft");
                assert_eq!(draft.preset, "1:1");
                assert!(
                    (draft.rect.width - draft.rect.height).abs() <= 1.0,
                    "{:?}",
                    draft.rect
                );
            }),
            (5, &|editor: &Editor| assert!(editor.crop.is_none())),
        ] {
            editor.evidence.as_mut().expect("evidence").capture_pending = false;
            let _ = editor.next_step();
            assert_eq!(evidence(&editor).step, step);
            assert!(evidence(&editor).capture_pending, "step {step}");
            check(&editor);
        }
        // The script is exhausted, and every step was recorded as sent.
        assert!(evidence(&editor).script.is_empty());
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_step_that_cannot_be_sent_is_recorded_and_still_captured() {
        let (mut editor, catalog, _, _) =
            scripted(r#"[{"draft":{"angle":4.0}},{"draft":{"preset":"7:5"}}]"#);
        for reason in ["no crop draft is open", "declares the aspect option 7:5"] {
            let _ = editor.next_step();
            let record = evidence(&editor).current.clone().expect("a step record");
            assert_eq!(record["status"], json!("failed"));
            assert!(
                record["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains(reason)),
                "{record}"
            );
            assert!(evidence(&editor).had_errors);
            assert!(evidence(&editor).capture_pending);
            editor.evidence.as_mut().expect("evidence").capture_pending = false;
        }
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_api_step_fills_the_envelope_and_waits_for_its_pixels() {
        let (mut editor, catalog, asset, _) = scripted(
            r#"[{"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}}]"#,
        );
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["expected_revision"], json!(4));
        assert!(
            record["request_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("desktop-")),
            "{record}"
        );
        // The request path is the ordinary one: pending until its pixels arrive, so the capture is
        // armed by `render_ready`, not by the step itself.
        assert!(editor.activity.pending);
        assert_eq!(editor.activity.requested, 2);
        assert!(!evidence(&editor).capture_pending);
        assert_eq!(editor.snapshot()["stack"]["layers"], json!([]));
        assert_eq!(
            editor.snapshot()["stack"]["revision"],
            json!(4),
            "the captured frame names the committed revision"
        );
        assert!(editor.busy, "the owner call is in flight");
        drop(asset);
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_workspace_step_sends_only_the_fields_that_differ() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"workspace":{"state_panel":false}}]"#);
        assert!(
            editor.session.workspace.state_panel,
            "starts at the default"
        );
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Session));
        assert!(
            !evidence(&editor).capture_pending,
            "the round trip has not settled yet"
        );
        finish(editor, catalog);

        // Asking for a value the session already reports needs no round trip at all: nothing to
        // settle, so the frame is captured straight away.
        let (mut editor, catalog, _, _) = scripted(r#"[{"workspace":{"tools_panel":true}}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, None);
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_preview_step_selects_by_sequence_or_returns_to_current() {
        // No loaded entry has this sequence: the step is refused, not silently ignored.
        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":{"sequence":9}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(
            record["reason"]
                .as_str()
                .is_some_and(|r| r.contains("sequence 9")),
            "{record}"
        );
        finish(editor, catalog);

        // `opened` (which `scripted` builds on) commits one entry at sequence 4.
        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":{"sequence":4}}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Preview));
        assert!(
            editor.busy,
            "the same round trip a history row's click starts"
        );
        finish(editor, catalog);

        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":"current"}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Preview));
        assert_eq!(editor.status, "Returning to current state…");
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_palette_step_opens_and_queries_or_runs_the_first_match() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"query":"crop"}}]"#);
        let _ = editor.next_step();
        assert!(editor.palette_open);
        assert_eq!(editor.palette_query, "crop");
        assert!(!editor.workspace.palette.entries.is_empty());
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);

        // The crop module's own reset is the only thing both these words can match.
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"run":"reset crop"}}]"#);
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        assert!(!editor.palette_open, "running an entry closes the palette");
        assert!(editor.busy, "{}", editor.status);
        assert!(
            editor.activity.requested > requested,
            "a mutation is tracked as evidence tracks any other open request"
        );
        finish(editor, catalog);

        // A query with no match fails the step rather than running something else.
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"run":"no such thing"}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        finish(editor, catalog);
    }
}
